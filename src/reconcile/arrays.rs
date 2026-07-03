//! Array-combining strategies for the reconcile engine.
//!
//! `deep_merge` delegates every array-vs-array case here; the strategy decides
//! whether DESIRED's list replaces, appends to, or unions with TARGET's — or, for
//! `Merge`, reconciles element membership three-way against BASE. `Merge` matches
//! elements by whole value by default, or — when a key field is configured and
//! every element of both sides is an object carrying it — by that key, deep-merging
//! the matched records.

use super::{reconcile, ArrayStrategy, Conflict, KeyPath, NodeList, Options};
use crate::value::{Leaf, Node};
use std::collections::HashSet;

/// Combine a TARGET array with a DESIRED array per `opts.arrays`, returning the
/// new element list and any `merge` [`Conflict`]s (each with a path *relative to*
/// this array — empty for a cross-over reorder of the array itself, a
/// `[field=value]` element selector for a conflict nested inside a keyed record).
/// `base` is the merge ancestor at this path (used only by `Merge`); only `Merge`
/// can conflict. `parent_key` is the object key holding this array — used to
/// resolve per-key merge keys.
pub(super) fn combine<L: Leaf>(
    target: &[Node<L>],
    desired: &[Node<L>],
    base: Option<&Node<L>>,
    opts: &Options,
    parent_key: Option<&str>,
) -> (NodeList<L>, Vec<Conflict<L>>) {
    match opts.arrays {
        // Atomic: DESIRED's array wins wholesale.
        ArrayStrategy::Replace => (desired.to_vec(), Vec::new()),
        // Append DESIRED onto TARGET, keeping order and duplicates.
        ArrayStrategy::Concat => {
            let mut out = target.to_vec();
            out.extend(desired.iter().cloned());
            (out, Vec::new())
        }
        // Union ignoring order: keep TARGET's elements, append any DESIRED
        // element not already present (dedup by value).
        ArrayStrategy::Set => {
            let mut out = target.to_vec();
            for e in desired {
                if !out.contains(e) {
                    out.push(e.clone());
                }
            }
            (out, Vec::new())
        }
        // Three-way, move-aware merge against BASE — the only strategy that can
        // conflict. BASE elements only matter when BASE is itself an array here;
        // anything else (absent / type change) leaves membership a plain two-way
        // union, matching `Set`.
        ArrayStrategy::Merge => {
            let base_arr: &[Node<L>] = match base {
                Some(Node::Array(b)) => b,
                _ => &[],
            };
            ordered_merge(
                target,
                desired,
                base_arr,
                opts,
                opts.merge_keys.candidates(parent_key),
            )
        }
    }
}

/// Move-aware three-way merge of two arrays via a generalized topological sort
/// (GTS). Elements are matched by **identity** — the whole value by default, or,
/// when `keys` names candidate fields and every element of both sides is an object
/// carrying one, by that key field (a keyed record). Matched keyed records are
/// three-way *merged* (fields reconciled); value-matched elements are taken as-is.
/// Returns the ordered survivors and any [`Conflict`]s (a contradictory cross-over
/// cycle at this array, plus, in keyed mode, conflicts nested inside a merged
/// record).
fn ordered_merge<L: Leaf>(
    target: &[Node<L>],
    desired: &[Node<L>],
    base: &[Node<L>],
    opts: &Options,
    keys: &[String],
) -> (NodeList<L>, Vec<Conflict<L>>) {
    if !keys.is_empty() && all_keyed(target, keys) && all_keyed(desired, keys) {
        keyed_merge(target, desired, base, opts, keys)
    } else {
        value_merge(target, desired, base)
    }
}

/// Value-identity merge: survivors are the membership set (by value); each is
/// emitted as-is. Elements are whole values (no recursion), so the only possible
/// conflict is a cross-over reorder of the array itself.
fn value_merge<L: Leaf>(
    target: &[Node<L>],
    desired: &[Node<L>],
    base: &[Node<L>],
) -> (NodeList<L>, Vec<Conflict<L>>) {
    let verts = membership_merge(target, desired, base);
    let id_of = |e: &Node<L>| verts.iter().position(|v| v == e);
    assemble(verts.len(), id_of, &verts, target, desired, base)
}

/// Key-identity merge: elements are matched by the first present `keys` field.
/// Survivors are the membership set of *keys*; each survivor's value is the
/// three-way merge of the matched objects (or the lone side's object when present
/// on only one branch). A conflict *inside* a merged record is surfaced too, with
/// a `[field=value]` element selector prepended to its path so it locates down to
/// the record (e.g. `[name="web"].tags`).
fn keyed_merge<L: Leaf>(
    target: &[Node<L>],
    desired: &[Node<L>],
    base: &[Node<L>],
    opts: &Options,
    keys: &[String],
) -> (NodeList<L>, Vec<Conflict<L>>) {
    let key = |e: &Node<L>| identity(e, keys);
    let has = |seq: &[Node<L>], k: &Ident<L>| seq.iter().any(|e| key(e).as_ref() == Some(k));

    // Survivor keys, in membership order (the `membership_merge` rule, on keys).
    let mut survivors: Vec<Ident<L>> = Vec::new();
    for e in target {
        let k = key(e).expect("gated: every target element is keyed");
        let dropped_from_desired = has(base, &k) && !has(desired, &k);
        if !dropped_from_desired && !survivors.contains(&k) {
            survivors.push(k);
        }
    }
    for e in desired {
        let k = key(e).expect("gated: every desired element is keyed");
        let removed_from_target = has(base, &k) && !has(target, &k);
        if !removed_from_target && !survivors.contains(&k) {
            survivors.push(k);
        }
    }

    // Merged value per survivor key. When a survivor is present on both sides its
    // fields are reconciled recursively; any conflict from that merge is kept,
    // tagged with the record's `[field=value]` selector so it locates inside the
    // array.
    let find =
        |seq: &[Node<L>], k: &Ident<L>| seq.iter().find(|e| key(e).as_ref() == Some(k)).cloned();
    let mut merged: NodeList<L> = Vec::with_capacity(survivors.len());
    let mut nested: Vec<Conflict<L>> = Vec::new();
    for k in &survivors {
        let value = match (find(target, k), find(desired, k)) {
            (Some(t), Some(d)) => {
                let (m, conflicts) = reconcile(&t, &d, find(base, k).as_ref(), opts);
                let (field, key_value) = k;
                let selector = format!("[{field}={}]", render_value(key_value));
                for mut c in conflicts {
                    c.path.prepend(selector.clone());
                    nested.push(c);
                }
                m
            }
            (Some(t), None) => t,
            (None, Some(d)) => d,
            (None, None) => unreachable!("a survivor key comes from target or desired"),
        };
        merged.push(value);
    }

    let id_of = |e: &Node<L>| key(e).and_then(|k| survivors.iter().position(|s| *s == k));
    let (out, mut conflicts) = assemble(survivors.len(), id_of, &merged, target, desired, base);
    conflicts.append(&mut nested);
    (out, conflicts)
}

/// Render a keyed record's identity value for an element selector. Key fields are
/// scalars in practice (`Leaf::render`, which quotes strings); the composite arms
/// are a deterministic fallback for the degenerate non-scalar-key case.
fn render_value<L: Leaf>(v: &Node<L>) -> String {
    match v {
        Node::Leaf(l) => l.render(),
        Node::Array(a) => {
            let inner: Vec<String> = a.iter().map(render_value).collect();
            format!("[{}]", inner.join(","))
        }
        Node::Map(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, val)| format!("{k}={}", render_value(val)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

/// Shared GTS assembly: given the survivor count `n`, a map from an input element
/// to its survivor index (`id_of`), and the output value per survivor (`merged`),
/// order the survivors move-aware and report any cross-over cycle as a single
/// [`Conflict`] at this array (empty path — callers prepend the array's own key).
fn assemble<L: Leaf>(
    n: usize,
    id_of: impl Fn(&Node<L>) -> Option<usize>,
    merged: &[Node<L>],
    target: &[Node<L>],
    desired: &[Node<L>],
    base: &[Node<L>],
) -> (NodeList<L>, Vec<Conflict<L>>) {
    if n <= 1 {
        return (merged.to_vec(), Vec::new());
    }
    // Restrict each input to the survivors, in its own order (deduped), as an
    // index sequence.
    let restrict = |seq: &[Node<L>]| -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for e in seq {
            if let Some(i) = id_of(e) {
                if !out.contains(&i) {
                    out.push(i);
                }
            }
        }
        out
    };
    let (order, conflict_ids) =
        gts_order(n, &restrict(target), &restrict(desired), &restrict(base));
    let out: NodeList<L> = order.iter().map(|&i| merged[i].clone()).collect();
    let conflicts = if conflict_ids.is_empty() {
        Vec::new()
    } else {
        vec![Conflict {
            path: KeyPath::new(),
            elements: conflict_ids.iter().map(|&i| merged[i].clone()).collect(),
        }]
    };
    (out, conflicts)
}

/// The index-only generalized topological sort over `n` vertices, given the three
/// inputs as vertex-index sequences (`target_seq`, `desired_seq`, `base_seq`).
/// Fixed tie-break: earliest in TARGET, then DESIRED, then index — deterministic
/// and idempotent.
///
/// Returns `(order, conflict_ids)`:
/// - `order`: the `n` vertex indices in merged order (a permutation of `0..n`).
/// - `conflict_ids`: the vertex indices caught in a contradictory cross-over
///   cycle (a strongly connected component of size > 1), empty when there is no
///   conflict. These are still placed in `order`; the list only flags that their
///   relative order was resolved arbitrarily.
fn gts_order(
    n: usize,
    target_seq: &[usize],
    desired_seq: &[usize],
    base_seq: &[usize],
) -> (Vec<usize>, Vec<usize>) {
    // Position of each vertex within a restricted sequence (None if absent). The
    // transitive closure of a linear chain is just its order, so "a precedes b in
    // sequence j" (i.e. `Ej+`) is the O(1) test `pos_j[a] < pos_j[b]`.
    let positions = |seq: &[usize]| -> Vec<Option<usize>> {
        let mut p = vec![None; n];
        for (i, &v) in seq.iter().enumerate() {
            p[v] = Some(i);
        }
        p
    };
    let target_pos = positions(target_seq);
    let desired_pos = positions(desired_seq);
    let base_pos = positions(base_seq);
    let prec = |p: &[Option<usize>], a: usize, b: usize| matches!((p[a], p[b]), (Some(x), Some(y)) if x < y);

    // Steps 3-4: merged immediate-successor edges. An immediate edge from one
    // branch is dropped iff BASE ordered that pair transitively but the *other*
    // branch transitively reversed/deleted it:
    //   Em = (E1 \ (Eb+ \ E2+)) ∪ (E2 \ (Eb+ \ E1+))
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for w in target_seq.windows(2) {
        let (a, b) = (w[0], w[1]);
        if prec(&base_pos, a, b) && !prec(&desired_pos, a, b) {
            continue;
        }
        if !edges.contains(&(a, b)) {
            edges.push((a, b));
        }
    }
    for w in desired_seq.windows(2) {
        let (a, b) = (w[0], w[1]);
        if prec(&base_pos, a, b) && !prec(&target_pos, a, b) {
            continue;
        }
        if !edges.contains(&(a, b)) {
            edges.push((a, b));
        }
    }

    // Steps 5-6: re-add non-contradictory transitive order that a cross-over move
    // dropped. For a vertex with no incoming edge, connect its closest common
    // predecessor (before it in *both* branches, nearest in TARGET); for no
    // outgoing edge, its closest common successor.
    let incoming = |edges: &[(usize, usize)], v: usize| edges.iter().any(|&(_, b)| b == v);
    let outgoing = |edges: &[(usize, usize)], v: usize| edges.iter().any(|&(a, _)| a == v);
    for v in 0..n {
        if incoming(&edges, v) {
            continue;
        }
        let ccp = (0..n)
            .filter(|&u| u != v && prec(&target_pos, u, v) && prec(&desired_pos, u, v))
            .max_by_key(|&u| {
                (
                    target_pos[u].unwrap(),
                    desired_pos[u].unwrap(),
                    usize::MAX - u,
                )
            });
        if let Some(u) = ccp {
            if !edges.contains(&(u, v)) {
                edges.push((u, v));
            }
        }
    }
    for v in 0..n {
        if outgoing(&edges, v) {
            continue;
        }
        let ccs = (0..n)
            .filter(|&u| u != v && prec(&target_pos, v, u) && prec(&desired_pos, v, u))
            .min_by_key(|&u| (target_pos[u].unwrap(), desired_pos[u].unwrap(), u));
        if let Some(u) = ccs {
            if !edges.contains(&(v, u)) {
                edges.push((v, u));
            }
        }
    }

    // Adjacency (sorted/deduped for determinism).
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in &edges {
        adj[a].push(b);
    }
    for row in adj.iter_mut() {
        row.sort_unstable();
        row.dedup();
    }

    // Step 7: strongly connected components. n is small (config arrays), so use a
    // Warshall reachability closure and group by mutual reachability.
    let mut reach = vec![vec![false; n]; n];
    for (v, row) in reach.iter_mut().enumerate() {
        row[v] = true;
    }
    for &(a, b) in &edges {
        reach[a][b] = true;
    }
    for k in 0..n {
        let row_k = reach[k].clone();
        for row_i in reach.iter_mut() {
            if row_i[k] {
                for (j, &reachable) in row_k.iter().enumerate() {
                    if reachable {
                        row_i[j] = true;
                    }
                }
            }
        }
    }
    let mut comp = vec![usize::MAX; n];
    let mut ncomp = 0;
    for v in 0..n {
        if comp[v] != usize::MAX {
            continue;
        }
        comp[v] = ncomp;
        for u in (v + 1)..n {
            if comp[u] == usize::MAX && reach[v][u] && reach[u][v] {
                comp[u] = ncomp;
            }
        }
        ncomp += 1;
    }

    // Condensation: members, cross-component edges, in-degrees.
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); ncomp];
    for (v, &c) in comp.iter().enumerate() {
        members[c].push(v);
    }
    // A component with more than one vertex is a cycle: TARGET and DESIRED
    // reordered those elements contradictorily (a cross-over move).
    let conflict_ids: Vec<usize> = {
        let mut ids: Vec<usize> = members
            .iter()
            .filter(|m| m.len() > 1)
            .flatten()
            .copied()
            .collect();
        ids.sort_unstable();
        ids
    };
    let mut cadj: Vec<Vec<usize>> = vec![Vec::new(); ncomp];
    let mut cindeg = vec![0usize; ncomp];
    for &(a, b) in &edges {
        let (ca, cb) = (comp[a], comp[b]);
        if ca != cb && !cadj[ca].contains(&cb) {
            cadj[ca].push(cb);
            cindeg[cb] += 1;
        }
    }

    // A vertex's tie-break key: earliest in TARGET, then DESIRED, then index.
    let key = |v: usize| {
        (
            target_pos[v].unwrap_or(usize::MAX),
            desired_pos[v].unwrap_or(usize::MAX),
            v,
        )
    };
    let ckey: Vec<(usize, usize, usize)> = (0..ncomp)
        .map(|c| members[c].iter().map(|&v| key(v)).min().unwrap())
        .collect();

    // Step 8: topological sort of the condensation (Kahn), emitting a ready
    // component with the smallest key, each serialized internally by the marking
    // rule.
    let mut out: Vec<usize> = Vec::with_capacity(n);
    let mut done = vec![false; ncomp];
    for _ in 0..ncomp {
        let c = (0..ncomp)
            .filter(|&c| !done[c] && cindeg[c] == 0)
            .min_by_key(|&c| ckey[c])
            .expect("a DAG condensation always has a ready component");
        for v in order_scc(&members[c], &adj, &target_pos, &desired_pos) {
            out.push(v);
        }
        done[c] = true;
        for &d in &cadj[c] {
            cindeg[d] -= 1;
        }
    }
    (out, conflict_ids)
}

/// The identity of `e` for keyed matching: the first `keys` field it carries, as
/// `(field, value)` (the field disambiguates `name:"x"` from `id:"x"`). `None` if
/// `e` isn't an object with a candidate field.
type Ident<L> = (String, Node<L>);
fn identity<L: Leaf>(e: &Node<L>, keys: &[String]) -> Option<Ident<L>> {
    let map = e.as_map()?;
    keys.iter()
        .find_map(|f| map.get(f).map(|v| (f.clone(), v.clone())))
}

/// Whether every element of `seq` is an object carrying a candidate key field.
fn all_keyed<L: Leaf>(seq: &[Node<L>], keys: &[String]) -> bool {
    seq.iter().all(|e| identity(e, keys).is_some())
}

/// Three-way membership merge of two arrays (membership only, abstracting away
/// order). An element survives iff it is present in TARGET or DESIRED and was
/// deleted on *neither* branch relative to BASE: a BASE element dropped from
/// DESIRED is pruned, a BASE element the user removed from TARGET stays removed,
/// and an insertion on either side is kept. Survivors are emitted in TARGET order,
/// then DESIRED-only insertions appended. Membership is by value equality, so
/// duplicate values collapse (an ordered set, not a bag).
fn membership_merge<L: Leaf>(
    target: &[Node<L>],
    desired: &[Node<L>],
    base: &[Node<L>],
) -> Vec<Node<L>> {
    let in_base = |e: &Node<L>| base.contains(e);
    let in_target = |e: &Node<L>| target.contains(e);
    let in_desired = |e: &Node<L>| desired.contains(e);

    let mut out: Vec<Node<L>> = Vec::new();
    // TARGET order first: keep each element unless it is a BASE element that
    // DESIRED dropped (a managed deletion).
    for e in target {
        let dropped_from_desired = in_base(e) && !in_desired(e);
        if !dropped_from_desired && !out.contains(e) {
            out.push(e.clone());
        }
    }
    // Then DESIRED-only insertions, in DESIRED order — but a BASE element the user
    // already removed from TARGET stays removed (don't resurrect it).
    for e in desired {
        let removed_from_target = in_base(e) && !in_target(e);
        if !removed_from_target && !out.contains(e) {
            out.push(e.clone());
        }
    }
    out
}

/// Serialize the vertices of one strongly connected component. A trivial (size-1)
/// component is the vertex itself; a cycle is emitted by a marking rule — start
/// from the vertices first in TARGET or DESIRED, follow successor edges, and mark
/// each emitted vertex's successors as eligible — so the intra-cycle order follows
/// an actual input rather than inventing one. Ties break by `key(v)` (earliest in
/// TARGET, then DESIRED, then index).
fn order_scc(
    members: &[usize],
    adj: &[Vec<usize>],
    target_pos: &[Option<usize>],
    desired_pos: &[Option<usize>],
) -> Vec<usize> {
    if members.len() == 1 {
        return vec![members[0]];
    }
    let key = |v: usize| {
        (
            target_pos[v].unwrap_or(usize::MAX),
            desired_pos[v].unwrap_or(usize::MAX),
            v,
        )
    };
    let inset: HashSet<usize> = members.iter().copied().collect();
    let mut marked: HashSet<usize> = HashSet::new();
    // Mark the vertex first in TARGET and the vertex first in DESIRED.
    if let Some(&v) = members
        .iter()
        .filter(|&&v| target_pos[v].is_some())
        .min_by_key(|&&v| target_pos[v].unwrap())
    {
        marked.insert(v);
    }
    if let Some(&v) = members
        .iter()
        .filter(|&&v| desired_pos[v].is_some())
        .min_by_key(|&&v| desired_pos[v].unwrap())
    {
        marked.insert(v);
    }

    let mut remaining: HashSet<usize> = inset.clone();
    let mut queue: Vec<usize> = Vec::new();
    let mut out: Vec<usize> = Vec::with_capacity(members.len());
    while !remaining.is_empty() {
        // Prefer a marked successor of the last-emitted vertex; else any marked
        // remaining vertex; else (defensive) the smallest-key remaining vertex.
        let pick = queue
            .iter()
            .copied()
            .filter(|v| remaining.contains(v) && marked.contains(v))
            .min_by_key(|&v| key(v))
            .or_else(|| {
                remaining
                    .iter()
                    .copied()
                    .filter(|v| marked.contains(v))
                    .min_by_key(|&v| key(v))
            })
            .or_else(|| remaining.iter().copied().min_by_key(|&v| key(v)))
            .expect("remaining is non-empty");
        out.push(pick);
        remaining.remove(&pick);
        queue.clear();
        for &s in &adj[pick] {
            if inset.contains(&s) && remaining.contains(&s) {
                marked.insert(s);
                queue.push(s);
            }
        }
        queue.sort_by_key(|&v| key(v));
    }
    out
}
