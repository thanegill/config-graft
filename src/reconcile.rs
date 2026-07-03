//! Pure three-way reconcile algorithm — no I/O.
//!
//! TARGET (the live file), DESIRED (the managed subset), BASE (the snapshot of
//! what we applied last time). Arrays and scalars are atomic leaves, so a list
//! is reconciled and pruned as a whole, never element-by-element.

use crate::value::{Leaf, Node};
use clap::ValueEnum;
use indexmap::IndexMap;
use std::collections::HashSet;

mod arrays;

/// A list of array elements — the payload of a `Node::Array`. It's what the
/// array-combining strategies produce, and (on a `merge` conflict) report.
pub type NodeList<L> = Vec<Node<L>>;

/// A managed leaf path: a sequence of object keys (arrays/scalars are atomic
/// leaves). Distinct from `std::path::Path` — this addresses keys, not files.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct KeyPath(Vec<String>);

impl KeyPath {
    /// An empty path (the document root).
    fn new() -> KeyPath {
        KeyPath(Vec::new())
    }

    /// Append a key segment.
    fn push(&mut self, seg: String) {
        self.0.push(seg);
    }

    /// Drop the last key segment.
    fn pop(&mut self) {
        self.0.pop();
    }

    /// Prepend a key segment — used as a conflict bubbles up out of a subtree,
    /// gaining its parent key at each level.
    fn prepend(&mut self, seg: String) {
        self.0.insert(0, seg);
    }

    /// The path of the first `n` segments (a proper ancestor when `n < len`).
    fn prefix(&self, n: usize) -> KeyPath {
        KeyPath(self.0[..n].to_vec())
    }

    /// Render as a user-facing string: segments joined by `sep`, or `<root>` for
    /// the empty path. `sep` is format-specific.
    pub fn render(&self, sep: &str) -> String {
        if self.0.is_empty() {
            "<root>".to_string()
        } else {
            self.0.join(sep)
        }
    }
}

impl std::ops::Deref for KeyPath {
    type Target = [String];
    fn deref(&self) -> &[String] {
        &self.0
    }
}

/// How a DESIRED array combines with a TARGET array during the deep-merge.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum ArrayStrategy {
    /// DESIRED's array replaces TARGET's wholesale (atomic; the default).
    Replace,
    /// Append DESIRED's elements onto TARGET's (order preserved, duplicates kept).
    Concat,
    /// Union of both arrays, ignoring order and dropping duplicates.
    Set,
    /// Three-way merge against BASE: keep elements present on either side, prune
    /// a BASE element dropped from DESIRED (unless the user removed it from TARGET
    /// first), and order the survivors move-aware so a reordering on either side
    /// is preserved (via a generalized topological sort). Membership by value;
    /// duplicates collapse.
    Merge,
}

/// Options controlling reconciliation.
pub struct Options {
    /// Prune keys dropped from DESIRED (requires BASE). When false, only merge.
    pub prune: bool,
    /// How DESIRED arrays combine with TARGET arrays.
    pub arrays: ArrayStrategy,
}

/// A `merge` array conflict: at `path`, TARGET and DESIRED reordered `elements`
/// contradictorily (a cross-over move). The reconcile still resolves the order
/// deterministically; this records where and what so a caller can surface it.
pub struct Conflict<L> {
    /// The object path of the conflicted array.
    pub path: KeyPath,
    /// The elements caught in the contradictory reorder (in membership order).
    pub elements: NodeList<L>,
}

/// Reconcile DESIRED into a clone of TARGET, using BASE as the merge ancestor.
/// Returns the reconciled value plus any [`Conflict`]s where the `merge` strategy
/// hit a contradictory cross-over reorder (TARGET and DESIRED order the same
/// elements oppositely) that the tie-break had to resolve arbitrarily. The result
/// is still deterministic; the conflicts let a caller surface that the order was
/// resolved, not agreed.
pub fn reconcile<L: Leaf>(
    target: &Node<L>,
    desired: &Node<L>,
    base: Option<&Node<L>>,
    opts: &Options,
) -> (Node<L>, Vec<Conflict<L>>) {
    let mut result = match target {
        Node::Map(_) => target.clone(),
        _ => Node::Map(IndexMap::new()),
    };

    // 1-3: prune leaves we managed before (present in BASE) but no longer do
    // (gone from DESIRED) — but only where TARGET still holds the BASE value, so
    // a value the user/app changed by hand is left alone.
    let mut removed: Vec<KeyPath> = Vec::new();
    if opts.prune {
        if let Some(base) = base {
            let desired_leaves: HashSet<KeyPath> = leaf_paths(desired).into_iter().collect();
            removed = leaf_paths(base)
                .into_iter()
                .filter(|p| !desired_leaves.contains(p))
                .collect();
            for p in &removed {
                let live = get_path(&result, p);
                if live.is_some() && live == get_path(base, p) {
                    del_path(&mut result, p);
                }
            }
        }
    }

    // 4: deep-merge DESIRED (DESIRED wins leaf conflicts). BASE is threaded
    // alongside so the three-way `Merge` array strategy can see it. Array
    // conflicts bubble up as `deep_merge`'s return, gaining a path segment at each
    // level, so a location is built only for the (rare) conflicts.
    let conflicts = deep_merge(&mut result, desired, base, opts.arrays);

    // 5: collapse objects left empty by the prune (deepest first, cascading).
    if !removed.is_empty() {
        let mut ancestors: Vec<KeyPath> = Vec::new();
        for p in &removed {
            for i in 1..p.len() {
                ancestors.push(p.prefix(i));
            }
        }
        ancestors.sort();
        ancestors.dedup();
        ancestors.sort_by_key(|p| std::cmp::Reverse(p.len()));
        for anc in &ancestors {
            if matches!(get_path(&result, anc), Some(Node::Map(o)) if o.is_empty()) {
                del_path(&mut result, anc);
            }
        }
    }

    (result, conflicts)
}

/// Managed leaf paths: descend only through objects, so arrays and scalars are
/// atomic leaves.
pub fn leaf_paths<L: Leaf>(v: &Node<L>) -> Vec<KeyPath> {
    let mut out = Vec::new();
    let mut prefix = KeyPath::new();
    collect(v, &mut prefix, &mut out);
    out
}

fn collect<L: Leaf>(v: &Node<L>, prefix: &mut KeyPath, out: &mut Vec<KeyPath>) {
    match v {
        Node::Map(map) => {
            for (k, val) in map {
                prefix.push(k.clone());
                collect(val, prefix, out);
                prefix.pop();
            }
        }
        _ => out.push(prefix.clone()),
    }
}

/// Value at an object path, if present.
pub fn get_path<'a, L: Leaf>(v: &'a Node<L>, path: &[String]) -> Option<&'a Node<L>> {
    let mut cur = v;
    for key in path {
        cur = cur.as_map()?.get(key)?;
    }
    Some(cur)
}

fn del_path<L: Leaf>(v: &mut Node<L>, path: &[String]) {
    let Some((first, rest)) = path.split_first() else {
        return;
    };
    let Some(obj) = v.as_map_mut() else {
        return;
    };
    if rest.is_empty() {
        obj.shift_remove(first); // preserve the order of the surviving keys
    } else if let Some(child) = obj.get_mut(first) {
        del_path(child, rest);
    }
}

/// Deep-merge `desired` into `target`, with `base` as the merge ancestor at the
/// same path (for the three-way `Merge` strategy; ignored by the others).
/// Objects always merge recursively. Two arrays combine per `arrays` (replace /
/// concat / set-union / three-way merge); every other case — scalars, type
/// changes, array-vs-non-array — is replaced wholesale by `desired`.
///
/// Returns any `merge` [`Conflict`]s found, each with a path *relative to*
/// `target`. Callers prepend their own key as the conflicts bubble up, so a
/// location is built only for the (rare) conflicts, never for clean subtrees.
pub fn deep_merge<L: Leaf>(
    target: &mut Node<L>,
    desired: &Node<L>,
    base: Option<&Node<L>>,
    arrays: ArrayStrategy,
) -> Vec<Conflict<L>> {
    match desired {
        Node::Map(d) => {
            if let Node::Map(t) = target {
                let mut conflicts = Vec::new();
                for (k, dv) in d {
                    let bv = base.and_then(|b| b.as_map()).and_then(|bm| bm.get(k));
                    if let Some(tv) = t.get_mut(k) {
                        for mut c in deep_merge(tv, dv, bv, arrays) {
                            c.path.prepend(k.clone());
                            conflicts.push(c);
                        }
                    } else {
                        t.insert(k.clone(), dv.clone());
                    }
                }
                return conflicts;
            }
        }
        Node::Array(d) => {
            if let Node::Array(t) = target {
                let (combined, conflict) = arrays::combine(t, d, base, arrays);
                *t = combined;
                return conflict
                    .map(|elements| Conflict {
                        path: KeyPath::new(),
                        elements,
                    })
                    .into_iter()
                    .collect();
            }
        }
        _ => {}
    }
    *target = desired.clone();
    Vec::new()
}

/// Recursively sort every object's keys (for `--sort-keys`).
pub fn sort_keys<L: Leaf>(v: &Node<L>) -> Node<L> {
    match v {
        Node::Map(map) => {
            let mut entries: Vec<(&String, &Node<L>)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut sorted = IndexMap::with_capacity(entries.len());
            for (k, val) in entries {
                sorted.insert(k.clone(), sort_keys(val));
            }
            Node::Map(sorted)
        }
        Node::Array(arr) => Node::Array(arr.iter().map(|v| sort_keys(v)).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::json::JsonLeaf;
    use crate::format::{Json, ValueCodec};
    use serde_json::{json, Value};

    /// JSON value → `Node` (the JSON codec), so the tests can keep expressing
    /// inputs and expectations as readable `json!(...)` literals.
    fn n(v: Value) -> Node<JsonLeaf> {
        Json::decode(&v).unwrap()
    }

    /// `Node` → JSON value, for comparing results against `json!(...)`.
    fn j(node: &Node<JsonLeaf>) -> Value {
        Json::encode(node)
    }

    fn reconciled(t: Value, d: Value, b: Option<Value>, prune: bool) -> Value {
        j(&reconcile(
            &n(t),
            &n(d),
            b.map(n).as_ref(),
            &Options {
                prune,
                arrays: ArrayStrategy::Replace,
            },
        )
        .0)
    }

    fn reconciled_arrays(t: Value, d: Value, arrays: ArrayStrategy) -> Value {
        j(&reconcile(
            &n(t),
            &n(d),
            None,
            &Options {
                prune: true,
                arrays,
            },
        )
        .0)
    }

    fn reconciled_arrays_base(
        t: Value,
        d: Value,
        b: Option<Value>,
        arrays: ArrayStrategy,
    ) -> Value {
        j(&reconcile(
            &n(t),
            &n(d),
            b.map(n).as_ref(),
            &Options {
                prune: true,
                arrays,
            },
        )
        .0)
    }

    #[test]
    fn no_base_deep_merges() {
        assert_eq!(
            reconciled(
                json!({"a":1,"b":{"x":1}}),
                json!({"b":{"y":2},"a":9}),
                None,
                true
            ),
            json!({"a":9,"b":{"x":1,"y":2}})
        );
    }

    #[test]
    fn desired_wins_leaf_conflicts() {
        assert_eq!(
            reconciled(json!({"a":1}), json!({"a":2}), Some(json!({})), true),
            json!({"a":2})
        );
    }

    #[test]
    fn target_only_keys_preserved() {
        assert_eq!(
            reconciled(json!({"app":true}), json!({"a":1}), Some(json!({})), true),
            json!({"app":true,"a":1})
        );
    }

    #[test]
    fn prune_dropped_leaf() {
        assert_eq!(
            reconciled(
                json!({"a":{"b":1,"c":2,"d":3}}),
                json!({"a":{"b":1}}),
                Some(json!({"a":{"b":1,"c":2}})),
                true
            ),
            json!({"a":{"b":1,"d":3}})
        );
    }

    #[test]
    fn prune_empties_parent() {
        assert_eq!(
            reconciled(
                json!({"x":{"y":1},"z":2}),
                json!({}),
                Some(json!({"x":{"y":1}})),
                true
            ),
            json!({"z":2})
        );
    }

    #[test]
    fn keep_user_edited_scalar() {
        assert_eq!(
            reconciled(json!({"a":5}), json!({}), Some(json!({"a":1})), true),
            json!({"a":5})
        );
    }

    #[test]
    fn list_replaced_wholesale() {
        assert_eq!(
            reconciled(
                json!({"a":[1,2,3],"app":[7,8]}),
                json!({"a":[9]}),
                Some(json!({})),
                true
            ),
            json!({"a":[9],"app":[7,8]})
        );
    }

    #[test]
    fn list_of_objects_atomic() {
        assert_eq!(
            reconciled(
                json!({"s":[{"name":"old","keep":1}]}),
                json!({"s":[{"name":"new"}]}),
                Some(json!({})),
                true
            ),
            json!({"s":[{"name":"new"}]})
        );
    }

    #[test]
    fn prune_dropped_list() {
        assert_eq!(
            reconciled(
                json!({"a":[1,2],"z":9}),
                json!({}),
                Some(json!({"a":[1,2]})),
                true
            ),
            json!({"z":9})
        );
    }

    #[test]
    fn prune_list_empties_parent() {
        assert_eq!(
            reconciled(
                json!({"x":{"y":[1,2]},"z":2}),
                json!({}),
                Some(json!({"x":{"y":[1,2]}})),
                true
            ),
            json!({"z":2})
        );
    }

    #[test]
    fn keep_user_edited_list() {
        assert_eq!(
            reconciled(
                json!({"a":[1,2,3]}),
                json!({}),
                Some(json!({"a":[1,2]})),
                true
            ),
            json!({"a":[1,2,3]})
        );
    }

    #[test]
    fn no_prune_flag_keeps_dropped() {
        assert_eq!(
            reconciled(
                json!({"a":1,"b":2}),
                json!({"a":1}),
                Some(json!({"a":1,"b":2})),
                false
            ),
            json!({"a":1,"b":2})
        );
    }

    #[test]
    fn null_is_a_value_not_a_delete() {
        assert_eq!(
            reconciled(json!({"a":1}), json!({"a":null}), Some(json!({})), true),
            json!({"a":null})
        );
    }

    #[test]
    fn three_way_example_from_spec() {
        assert_eq!(
            reconciled(
                json!({"a":1,"b":5,"appOnly":true}),
                json!({"c":3}),
                Some(json!({"a":1,"b":2})),
                true
            ),
            json!({"b":5,"appOnly":true,"c":3})
        );
    }

    #[test]
    fn deeply_nested_merge() {
        assert_eq!(
            reconciled(
                json!({"a":{"b":{"c":{"d":1,"keep":9}}}}),
                json!({"a":{"b":{"c":{"d":2}}}}),
                Some(json!({})),
                true
            ),
            json!({"a":{"b":{"c":{"d":2,"keep":9}}}})
        );
    }

    #[test]
    fn object_replaces_scalar_on_type_change() {
        assert_eq!(
            reconciled(json!({"a":1}), json!({"a":{"x":2}}), Some(json!({})), true),
            json!({"a":{"x":2}})
        );
    }

    #[test]
    fn scalar_replaces_object_on_type_change() {
        assert_eq!(
            reconciled(json!({"a":{"x":2}}), json!({"a":7}), Some(json!({})), true),
            json!({"a":7})
        );
    }

    #[test]
    fn non_object_target_coerced_to_empty() {
        assert_eq!(
            reconciled(json!([1, 2, 3]), json!({"a":1}), None, true),
            json!({"a":1})
        );
    }

    #[test]
    fn empty_desired_no_base_is_noop() {
        assert_eq!(
            reconciled(json!({"a":1,"b":[2]}), json!({}), None, true),
            json!({"a":1,"b":[2]})
        );
    }

    #[test]
    fn prune_only_when_target_matches_base() {
        // c matches base -> pruned; d was user-changed -> kept.
        assert_eq!(
            reconciled(
                json!({"c":2,"d":9}),
                json!({}),
                Some(json!({"c":2,"d":2})),
                true
            ),
            json!({"d":9})
        );
    }

    // ----- array strategies -----

    #[test]
    fn arrays_replace_strategy_is_wholesale() {
        assert_eq!(
            reconciled_arrays(
                json!({"a":[1,2,3]}),
                json!({"a":[3,4]}),
                ArrayStrategy::Replace
            ),
            json!({"a":[3,4]})
        );
    }

    #[test]
    fn arrays_concat_keeps_order_and_duplicates() {
        assert_eq!(
            reconciled_arrays(
                json!({"a":[1,2]}),
                json!({"a":[2,3]}),
                ArrayStrategy::Concat
            ),
            json!({"a":[1,2,2,3]})
        );
    }

    #[test]
    fn arrays_set_unions_ignoring_order_and_dedups() {
        assert_eq!(
            reconciled_arrays(
                json!({"a":[1,2,3]}),
                json!({"a":[3,2,4]}),
                ArrayStrategy::Set
            ),
            json!({"a":[1,2,3,4]})
        );
    }

    #[test]
    fn arrays_set_is_idempotent_when_subset() {
        // DESIRED already a subset of TARGET -> no change.
        assert_eq!(
            reconciled_arrays(json!({"a":[1,2,3]}), json!({"a":[2,3]}), ArrayStrategy::Set),
            json!({"a":[1,2,3]})
        );
    }

    #[test]
    fn arrays_set_dedups_object_elements_by_value() {
        assert_eq!(
            reconciled_arrays(
                json!({"s":[{"id":1}]}),
                json!({"s":[{"id":1},{"id":2}]}),
                ArrayStrategy::Set
            ),
            json!({"s":[{"id":1},{"id":2}]})
        );
    }

    #[test]
    fn set_array_replaces_when_target_not_array() {
        // TARGET value isn't an array -> DESIRED array replaces regardless.
        assert_eq!(
            reconciled_arrays(json!({"a":5}), json!({"a":[1,2]}), ArrayStrategy::Set),
            json!({"a":[1,2]})
        );
    }

    #[test]
    fn concat_array_replaces_when_target_not_array() {
        assert_eq!(
            reconciled_arrays(json!({"a":5}), json!({"a":[1,2]}), ArrayStrategy::Concat),
            json!({"a":[1,2]})
        );
    }

    #[test]
    fn array_strategy_recurses_into_nested_objects() {
        assert_eq!(
            reconciled_arrays(
                json!({"o":{"a":[1,2]}}),
                json!({"o":{"a":[2,3]}}),
                ArrayStrategy::Set
            ),
            json!({"o":{"a":[1,2,3]}})
        );
    }

    #[test]
    fn set_merged_list_still_pruned_atomically_when_dropped_unchanged() {
        // With prune + Set, a managed list unchanged from base is removed whole.
        let result = j(&reconcile(
            &n(json!({"a":[1,2],"z":9})),
            &n(json!({})),
            Some(&n(json!({"a":[1,2]}))),
            &Options {
                prune: true,
                arrays: ArrayStrategy::Set,
            },
        )
        .0);
        assert_eq!(result, json!({"z":9}));
    }

    // ----- three-way membership merge (`merge`) -----

    #[test]
    fn merge_prunes_base_element_dropped_from_desired() {
        // The headline gap: BASE had [a,b], DESIRED drops to [a], TARGET still
        // holds both unchanged -> b is pruned (set would keep it).
        assert_eq!(
            reconciled_arrays_base(
                json!({"a":["x","y"]}),
                json!({"a":["x"]}),
                Some(json!({"a":["x","y"]})),
                ArrayStrategy::Merge
            ),
            json!({"a":["x"]})
        );
    }

    #[test]
    fn merge_keeps_unmanaged_target_element() {
        // "z" is in TARGET but never in BASE/DESIRED (app/user wrote it) -> kept.
        assert_eq!(
            reconciled_arrays_base(
                json!({"a":["x","z"]}),
                json!({"a":["x"]}),
                Some(json!({"a":["x"]})),
                ArrayStrategy::Merge
            ),
            json!({"a":["x","z"]})
        );
    }

    #[test]
    fn merge_appends_desired_insertion() {
        assert_eq!(
            reconciled_arrays_base(
                json!({"a":["x"]}),
                json!({"a":["x","y"]}),
                Some(json!({"a":["x"]})),
                ArrayStrategy::Merge
            ),
            json!({"a":["x","y"]})
        );
    }

    #[test]
    fn merge_respects_user_deletion_of_base_element() {
        // BASE [x,y], the user deleted y from TARGET; DESIRED still lists it,
        // but a user-removed managed element stays removed.
        assert_eq!(
            reconciled_arrays_base(
                json!({"a":["x"]}),
                json!({"a":["x","y"]}),
                Some(json!({"a":["x","y"]})),
                ArrayStrategy::Merge
            ),
            json!({"a":["x"]})
        );
    }

    #[test]
    fn merge_without_base_is_union() {
        // No BASE -> every element is an insertion -> same as `set`.
        assert_eq!(
            reconciled_arrays_base(
                json!({"a":[1, 2, 3]}),
                json!({"a":[3, 2, 4]}),
                None,
                ArrayStrategy::Merge
            ),
            json!({"a":[1, 2, 3, 4]})
        );
    }

    #[test]
    fn merge_is_idempotent_on_a_settled_list() {
        // TARGET already equals DESIRED with BASE matching -> no change.
        assert_eq!(
            reconciled_arrays_base(
                json!({"a":["x","y"]}),
                json!({"a":["x","y"]}),
                Some(json!({"a":["x","y"]})),
                ArrayStrategy::Merge
            ),
            json!({"a":["x","y"]})
        );
    }

    #[test]
    fn merge_combines_both_sides_changes() {
        // BASE [a,b,c]; DESIRED drops c and adds d; TARGET kept its own e and
        // dropped b. Result: a (kept), b pruned (DESIRED... no, TARGET removed
        // b -> stays gone), c pruned (DESIRED dropped), e kept (unmanaged), d
        // appended.
        assert_eq!(
            reconciled_arrays_base(
                json!({"a":["a","c","e"]}),
                json!({"a":["a","b","d"]}),
                Some(json!({"a":["a","b","c"]})),
                ArrayStrategy::Merge
            ),
            // a: in both, kept. c: BASE elem dropped from DESIRED -> pruned.
            // e: unmanaged TARGET elem -> kept. b: BASE elem the user removed
            // from TARGET -> stays removed. d: DESIRED insertion -> appended.
            json!({"a":["a","e","d"]})
        );
    }

    #[test]
    fn merge_dedups_by_value() {
        assert_eq!(
            reconciled_arrays_base(
                json!({"a":[1, 1, 2]}),
                json!({"a":[2, 3, 3]}),
                None,
                ArrayStrategy::Merge
            ),
            json!({"a":[1, 2, 3]})
        );
    }

    #[test]
    fn merge_array_replaces_when_target_not_array() {
        assert_eq!(
            reconciled_arrays_base(
                json!({"a":5}),
                json!({"a":[1, 2]}),
                Some(json!({"a":[1, 2]})),
                ArrayStrategy::Merge
            ),
            json!({"a":[1, 2]})
        );
    }

    #[test]
    fn merge_managed_list_dropped_whole_is_pruned_atomically() {
        // Whole-array drop from DESIRED is still the atomic prune path, even
        // under Merge (arrays are atomic leaf paths for pruning).
        assert_eq!(
            reconciled_arrays_base(
                json!({"a":[1, 2],"z":9}),
                json!({}),
                Some(json!({"a":[1, 2]})),
                ArrayStrategy::Merge
            ),
            json!({"z":9})
        );
    }

    // ----- move-aware ordering (`merge`, GTS) -----

    #[test]
    fn merge_preserves_a_target_move() {
        // BASE [a,b,c]; the user moved a to the end in TARGET; DESIRED unchanged.
        // The move is the only change, so it wins.
        assert_eq!(
            reconciled_arrays_base(
                json!({"l":["b","c","a"]}),
                json!({"l":["a","b","c"]}),
                Some(json!({"l":["a","b","c"]})),
                ArrayStrategy::Merge
            ),
            json!({"l":["b","c","a"]})
        );
    }

    #[test]
    fn merge_preserves_a_desired_move() {
        // Symmetric: DESIRED moves c to the front, TARGET unchanged -> move wins.
        assert_eq!(
            reconciled_arrays_base(
                json!({"l":["a","b","c"]}),
                json!({"l":["c","a","b"]}),
                Some(json!({"l":["a","b","c"]})),
                ArrayStrategy::Merge
            ),
            json!({"l":["c","a","b"]})
        );
    }

    #[test]
    fn merge_move_and_insert_combine() {
        // BASE [a,b,c]; TARGET moves c to the front; DESIRED inserts x between a
        // and b. Both non-conflicting changes land: c leads, x sits between a
        // and b.
        assert_eq!(
            reconciled_arrays_base(
                json!({"l":["c","a","b"]}),
                json!({"l":["a","x","b","c"]}),
                Some(json!({"l":["a","b","c"]})),
                ArrayStrategy::Merge
            ),
            json!({"l":["c","a","x","b"]})
        );
    }

    #[test]
    fn merge_is_idempotent_under_moves() {
        // Re-applying the reconciled result (same DESIRED/BASE) is a fixpoint.
        let t = json!({"l":["a","b","c"]});
        let d = json!({"l":["c","a","b"]});
        let b = json!({"l":["a","b","c"]});
        let once = reconciled_arrays_base(t, d.clone(), Some(b.clone()), ArrayStrategy::Merge);
        let twice = reconciled_arrays_base(once.clone(), d, Some(b), ArrayStrategy::Merge);
        assert_eq!(once, twice);
    }

    #[test]
    fn merge_worked_example() {
        // A full worked example: BASE TKQNFBP, TARGET KQTNJPFS, DESIRED TKMNPJFX.
        // Q and B are deleted on one branch (pruned); J/P form a contradictory-move
        // cycle. Expected linearization K{TM}N{JP}F{XS} -- our fixed tie-break
        // (TARGET before DESIRED) resolves the arbitrary pairs to KTMNJPFSX.
        assert_eq!(
            reconciled_arrays_base(
                json!({"l":["K", "Q", "T", "N", "J", "P", "F", "S"]}),
                json!({"l":["T", "K", "M", "N", "P", "J", "F", "X"]}),
                Some(json!({"l":["T", "K", "Q", "N", "F", "B", "P"]})),
                ArrayStrategy::Merge
            ),
            json!({"l":["K", "T", "M", "N", "J", "P", "F", "S", "X"]})
        );
    }

    // ----- conflict surfacing -----

    fn conflict_paths(t: Value, d: Value, b: Option<Value>, arrays: ArrayStrategy) -> Vec<String> {
        let (_result, conflicts) = reconcile(
            &n(t),
            &n(d),
            b.map(n).as_ref(),
            &Options {
                prune: true,
                arrays,
            },
        );
        conflicts.iter().map(|c| c.path.render(".")).collect()
    }

    #[test]
    fn keypath_render_uses_the_given_separator() {
        let mut p = KeyPath::new();
        p.push("a".to_string());
        p.push("b".to_string());
        assert_eq!(p.render("."), "a.b");
        assert_eq!(p.render(":"), "a:b");
        assert_eq!(KeyPath::new().render(":"), "<root>"); // empty path
    }

    #[test]
    fn merge_conflict_names_the_conflicting_elements() {
        // The conflict carries the elements caught in the contradictory reorder.
        let (_result, conflicts) = reconcile(
            &n(json!({"l": ["x", "y"]})),
            &n(json!({"l": ["y", "x"]})),
            None,
            &Options {
                prune: true,
                arrays: ArrayStrategy::Merge,
            },
        );
        assert_eq!(conflicts.len(), 1);
        assert_eq!(
            j(&Node::Array(conflicts[0].elements.clone())),
            json!(["x", "y"])
        );
    }

    #[test]
    fn merge_reports_contradictory_reorder() {
        // TARGET and DESIRED order the same two elements oppositely, with no BASE
        // to arbitrate -> a cross-over cycle -> a conflict at path `l`.
        assert_eq!(
            conflict_paths(
                json!({"l": ["x", "y"]}),
                json!({"l": ["y", "x"]}),
                None,
                ArrayStrategy::Merge
            ),
            vec!["l"]
        );
    }

    #[test]
    fn merge_conflict_path_is_nested_and_dotted() {
        assert_eq!(
            conflict_paths(
                json!({"a": {"b": {"tags": ["x", "y"]}}}),
                json!({"a": {"b": {"tags": ["y", "x"]}}}),
                None,
                ArrayStrategy::Merge
            ),
            vec!["a.b.tags"]
        );
    }

    #[test]
    fn merge_clean_reorder_is_not_a_conflict() {
        // Only DESIRED moves an element; TARGET agrees with BASE -> no contradiction.
        assert!(conflict_paths(
            json!({"l": ["a", "b", "c"]}),
            json!({"l": ["c", "a", "b"]}),
            Some(json!({"l": ["a", "b", "c"]})),
            ArrayStrategy::Merge
        )
        .is_empty());
    }

    #[test]
    fn non_merge_strategies_never_conflict() {
        // The same contradictory input under `replace` is not a conflict — only
        // `merge` reconciles element order, so only it can conflict.
        assert!(conflict_paths(
            json!({"l": ["x", "y"]}),
            json!({"l": ["y", "x"]}),
            None,
            ArrayStrategy::Replace
        )
        .is_empty());
    }

    // ----- helper-level unit tests -----

    #[test]
    fn leaf_paths_treats_arrays_as_atomic() {
        let mut paths = leaf_paths(&n(json!({"a":1,"b":{"c":2},"d":[1,2]})));
        paths.sort();
        assert_eq!(
            paths,
            vec![
                KeyPath(vec!["a".to_string()]),
                KeyPath(vec!["b".to_string(), "c".to_string()]),
                KeyPath(vec!["d".to_string()]),
            ]
        );
    }

    #[test]
    fn get_path_reads_nested_and_misses() {
        let v = n(json!({"a":{"b":7}}));
        assert_eq!(get_path(&v, &["a".into(), "b".into()]), Some(&n(json!(7))));
        assert_eq!(get_path(&v, &["a".into(), "x".into()]), None);
        assert_eq!(get_path(&v, &["nope".into()]), None);
    }

    #[test]
    fn sort_keys_sorts_recursively() {
        let sorted = sort_keys(&n(json!({"b":1,"a":{"d":1,"c":2}})));
        assert_eq!(
            serde_json::to_string(&j(&sorted)).unwrap(),
            r#"{"a":{"c":2,"d":1},"b":1}"#
        );
    }

    #[test]
    fn sort_keys_recurses_through_arrays() {
        let sorted = sort_keys(&n(json!({"list":[{"b":1,"a":2}],"n":5})));
        assert_eq!(
            serde_json::to_string(&j(&sorted)).unwrap(),
            r#"{"list":[{"a":2,"b":1}],"n":5}"#
        );
    }

    #[test]
    fn array_strategy_derives_clone_eq_debug() {
        let s = ArrayStrategy::Set;
        assert_eq!(s, s); // Eq
        assert_eq!(s, s.clone()); // Clone (Copy)
        assert_ne!(ArrayStrategy::Replace, ArrayStrategy::Concat);
        assert!(format!("{s:?}").contains("Set")); // Debug
    }

    #[test]
    fn del_path_guards_empty_path_and_non_object() {
        // Empty path: no-op.
        let mut v = n(json!({"a":1}));
        del_path(&mut v, &[]);
        assert_eq!(v, n(json!({"a":1})));

        // Descending into a non-object value: no-op.
        let mut scalar = n(json!(5));
        del_path(&mut scalar, &["a".to_string()]);
        assert_eq!(scalar, n(json!(5)));

        // Happy path still deletes.
        let mut nested = n(json!({"a":{"b":1}}));
        del_path(&mut nested, &["a".to_string(), "b".to_string()]);
        assert_eq!(nested, n(json!({"a":{}})));
    }
}
