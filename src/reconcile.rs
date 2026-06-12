//! Pure three-way reconcile algorithm — no I/O.
//!
//! TARGET (the live file), DESIRED (the managed subset), BASE (the snapshot of
//! what we applied last time). Arrays and scalars are atomic leaves, so a list
//! is reconciled and pruned as a whole, never element-by-element.

use clap::ValueEnum;
use serde_json::{Map, Value};
use std::collections::HashSet;

/// A managed leaf path: object keys only (arrays/scalars are atomic leaves).
pub type Path = Vec<String>;

/// How a DESIRED array combines with a TARGET array during the deep-merge.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, ValueEnum)]
pub enum ArrayStrategy {
    /// DESIRED's array replaces TARGET's wholesale (atomic; the default).
    #[default]
    Replace,
    /// Append DESIRED's elements onto TARGET's (order preserved, duplicates kept).
    Concat,
    /// Union of both arrays, ignoring order and dropping duplicates.
    Set,
}

/// Options controlling reconciliation.
pub struct Options {
    /// Prune keys dropped from DESIRED (requires BASE). When false, only merge.
    pub prune: bool,
    /// How DESIRED arrays combine with TARGET arrays.
    pub arrays: ArrayStrategy,
}

/// Reconcile DESIRED into a clone of TARGET, using BASE as the merge ancestor.
pub fn reconcile(target: &Value, desired: &Value, base: Option<&Value>, opts: &Options) -> Value {
    let mut result = match target {
        Value::Object(_) => target.clone(),
        _ => Value::Object(Map::new()),
    };

    // 1-3: prune leaves we managed before (present in BASE) but no longer do
    // (gone from DESIRED) — but only where TARGET still holds the BASE value, so
    // a value the user/app changed by hand is left alone.
    let mut removed: Vec<Path> = Vec::new();
    if opts.prune {
        if let Some(base) = base {
            let desired_leaves: HashSet<Path> = leaf_paths(desired).into_iter().collect();
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

    // 4: deep-merge DESIRED (DESIRED wins leaf conflicts).
    deep_merge(&mut result, desired, opts.arrays);

    // 5: collapse objects left empty by the prune (deepest first, cascading).
    if !removed.is_empty() {
        let mut ancestors: Vec<Path> = Vec::new();
        for p in &removed {
            for i in 1..p.len() {
                ancestors.push(p[..i].to_vec());
            }
        }
        ancestors.sort();
        ancestors.dedup();
        ancestors.sort_by_key(|p| std::cmp::Reverse(p.len()));
        for anc in &ancestors {
            if matches!(get_path(&result, anc), Some(Value::Object(o)) if o.is_empty()) {
                del_path(&mut result, anc);
            }
        }
    }

    result
}

/// Managed leaf paths: descend only through objects, so arrays and scalars are
/// atomic leaves.
pub fn leaf_paths(v: &Value) -> Vec<Path> {
    let mut out = Vec::new();
    let mut prefix = Vec::new();
    collect(v, &mut prefix, &mut out);
    out
}

fn collect(v: &Value, prefix: &mut Path, out: &mut Vec<Path>) {
    match v {
        Value::Object(map) => {
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
pub fn get_path<'a>(v: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut cur = v;
    for key in path {
        cur = cur.as_object()?.get(key)?;
    }
    Some(cur)
}

fn del_path(v: &mut Value, path: &[String]) {
    let Some((first, rest)) = path.split_first() else {
        return;
    };
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    if rest.is_empty() {
        obj.shift_remove(first); // preserve the order of the surviving keys
    } else if let Some(child) = obj.get_mut(first) {
        del_path(child, rest);
    }
}

/// Deep-merge `desired` into `target`: objects always merge recursively. Two
/// arrays combine per `arrays` (replace / concat / set-union); every other
/// case — scalars, type changes, array-vs-non-array — is replaced wholesale by
/// `desired`.
pub fn deep_merge(target: &mut Value, desired: &Value, arrays: ArrayStrategy) {
    match desired {
        Value::Object(d) => {
            if let Value::Object(t) = target {
                for (k, dv) in d {
                    if let Some(tv) = t.get_mut(k) {
                        deep_merge(tv, dv, arrays);
                    } else {
                        t.insert(k.clone(), dv.clone());
                    }
                }
                return;
            }
        }
        Value::Array(d) if arrays != ArrayStrategy::Replace => {
            if let Value::Array(t) = target {
                match arrays {
                    ArrayStrategy::Concat => t.extend(d.iter().cloned()),
                    // Union ignoring order: keep TARGET's elements, append any
                    // DESIRED element not already present (dedup by value).
                    ArrayStrategy::Set => {
                        for e in d {
                            if !t.contains(e) {
                                t.push(e.clone());
                            }
                        }
                    }
                    ArrayStrategy::Replace => unreachable!(),
                }
                return;
            }
        }
        _ => {}
    }
    *target = desired.clone();
}

/// Recursively sort every object's keys (for `--sort-keys`).
pub fn sort_keys(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut sorted = Map::new();
            for (k, val) in entries {
                sorted.insert(k.clone(), sort_keys(val));
            }
            Value::Object(sorted)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sort_keys).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reconciled(t: Value, d: Value, b: Option<Value>, prune: bool) -> Value {
        reconcile(
            &t,
            &d,
            b.as_ref(),
            &Options {
                prune,
                arrays: ArrayStrategy::Replace,
            },
        )
    }

    fn reconciled_arrays(t: Value, d: Value, arrays: ArrayStrategy) -> Value {
        reconcile(
            &t,
            &d,
            None,
            &Options {
                prune: true,
                arrays,
            },
        )
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
    fn arrays_replace_by_default() {
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
    fn array_strategy_only_applies_when_both_are_arrays() {
        // TARGET value isn't an array -> DESIRED array replaces regardless.
        assert_eq!(
            reconciled_arrays(json!({"a":5}), json!({"a":[1,2]}), ArrayStrategy::Set),
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
        let result = reconcile(
            &json!({"a":[1,2],"z":9}),
            &json!({}),
            Some(&json!({"a":[1,2]})),
            &Options {
                prune: true,
                arrays: ArrayStrategy::Set,
            },
        );
        assert_eq!(result, json!({"z":9}));
    }

    // ----- helper-level unit tests -----

    #[test]
    fn leaf_paths_treats_arrays_as_atomic() {
        let mut paths = leaf_paths(&json!({"a":1,"b":{"c":2},"d":[1,2]}));
        paths.sort();
        assert_eq!(
            paths,
            vec![
                vec!["a".to_string()],
                vec!["b".to_string(), "c".to_string()],
                vec!["d".to_string()],
            ]
        );
    }

    #[test]
    fn get_path_reads_nested_and_misses() {
        let v = json!({"a":{"b":7}});
        assert_eq!(get_path(&v, &["a".into(), "b".into()]), Some(&json!(7)));
        assert_eq!(get_path(&v, &["a".into(), "x".into()]), None);
        assert_eq!(get_path(&v, &["nope".into()]), None);
    }

    #[test]
    fn sort_keys_sorts_recursively() {
        let sorted = sort_keys(&json!({"b":1,"a":{"d":1,"c":2}}));
        assert_eq!(
            serde_json::to_string(&sorted).unwrap(),
            r#"{"a":{"c":2,"d":1},"b":1}"#
        );
    }
}
