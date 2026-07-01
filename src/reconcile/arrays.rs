//! Array-combining strategies for the reconcile engine.
//!
//! `deep_merge` delegates every array-vs-array case here; the strategy decides
//! whether DESIRED's list replaces, appends to, or unions with TARGET's — or, for
//! `Merge`, reconciles element membership three-way against BASE.

use super::ArrayStrategy;
use crate::value::{Leaf, Node};
use std::collections::HashSet;

/// Combine a TARGET array with a DESIRED array per `strategy`, returning the new
/// element list. `base` is the merge ancestor at this path (used only by `Merge`).
pub(super) fn combine<L: Leaf>(
    target: &[Node<L>],
    desired: &[Node<L>],
    base: Option<&Node<L>>,
    strategy: ArrayStrategy,
) -> Vec<Node<L>> {
    match strategy {
        // Atomic: DESIRED's array wins wholesale.
        ArrayStrategy::Replace => desired.to_vec(),
        // Append DESIRED onto TARGET, keeping order and duplicates.
        ArrayStrategy::Concat => {
            let mut out = target.to_vec();
            out.extend(desired.iter().cloned());
            out
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
            out
        }
        // Three-way, move-aware merge against BASE. BASE elements only matter
        // when BASE is itself an array here; anything else (absent / type change)
        // leaves membership a plain two-way union, matching `Set`.
        ArrayStrategy::Merge => {
            let base_arr: &[Node<L>] = match base {
                Some(Node::Array(b)) => b,
                _ => &[],
            };
            ordered_merge(target, desired, base_arr)
        }
    }
}

/// Three-way membership merge of two arrays (membership only, abstracting away
/// order). An element survives iff it is present in TARGET or DESIRED and
/// was deleted on *neither* branch relative to BASE: a BASE element dropped from
/// DESIRED is pruned, a BASE element the user removed from TARGET stays removed,
/// and an insertion on either side is kept. Survivors are emitted in TARGET
/// order, then DESIRED-only insertions appended. Membership is by value
/// equality, so duplicate values collapse (an ordered set, not a bag).
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
    // Then DESIRED-only insertions, in DESIRED order — but a BASE element the
    // user already removed from TARGET stays removed (don't resurrect it).
    for e in desired {
        let removed_from_target = in_base(e) && !in_target(e);
        if !removed_from_target && !out.contains(e) {
            out.push(e.clone());
        }
    }
    out
}

/// Move-aware three-way merge of two ordered arrays via a generalized topological
/// sort (GTS). Membership is `membership_merge`'s result; this then *orders* those
/// survivors so a relative order that BASE and a branch agree on is preserved even
/// when an element was moved. Contradictory cross-over moves (each side reorders
/// the other's pair) form a cycle that is broken consistently with one input
/// rather than fabricating a new order. Every non-deterministic choice takes a
/// fixed tie-break — earliest position in TARGET, then DESIRED, then insertion
/// order — so the result is deterministic and idempotent (required for `--check`
/// and re-apply no-ops).
fn ordered_merge<L: Leaf>(
    target: &[Node<L>],
    desired: &[Node<L>],
    base: &[Node<L>],
) -> Vec<Node<L>> {
    // Step 1: the surviving vertex set (deduped, membership-correct).
    let verts = membership_merge(target, desired, base);
    let n = verts.len();
    if n <= 1 {
        return verts;
    }
    let id = |e: &Node<L>| verts.iter().position(|v| v == e);

    // Step 2: restrict each input to the survivors, in that input's own order
    // (deduped), as vertex-id sequences.
    let restrict = |seq: &[Node<L>]| -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for e in seq {
            if let Some(i) = id(e) {
                if !out.contains(&i) {
                    out.push(i);
                }
            }
        }
        out
    };
    let r1 = restrict(target);
    let r2 = restrict(desired);
    let rb = restrict(base);

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
    let pos1 = positions(&r1);
    let pos2 = positions(&r2);
    let posb = positions(&rb);
    let prec = |p: &[Option<usize>], a: usize, b: usize| matches!((p[a], p[b]), (Some(x), Some(y)) if x < y);

    // Steps 3-4: merged immediate-successor edges. An immediate edge from one
    // branch is dropped iff BASE ordered that pair transitively but the *other*
    // branch transitively reversed/deleted it:
    //   Em = (E1 \ (Eb+ \ E2+)) ∪ (E2 \ (Eb+ \ E1+))
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for w in r1.windows(2) {
        let (a, b) = (w[0], w[1]);
        if prec(&posb, a, b) && !prec(&pos2, a, b) {
            continue;
        }
        if !edges.contains(&(a, b)) {
            edges.push((a, b));
        }
    }
    for w in r2.windows(2) {
        let (a, b) = (w[0], w[1]);
        if prec(&posb, a, b) && !prec(&pos1, a, b) {
            continue;
        }
        if !edges.contains(&(a, b)) {
            edges.push((a, b));
        }
    }

    // Steps 5-6: re-add non-contradictory transitive order that a cross-over move
    // dropped. For a vertex with no incoming edge, connect its closest common
    // predecessor (a vertex before it in *both* branches, nearest in TARGET); for
    // no outgoing edge, its closest common successor.
    let incoming = |edges: &[(usize, usize)], v: usize| edges.iter().any(|&(_, b)| b == v);
    let outgoing = |edges: &[(usize, usize)], v: usize| edges.iter().any(|&(a, _)| a == v);
    for v in 0..n {
        if incoming(&edges, v) {
            continue;
        }
        // closest common predecessor: max pos1, then max pos2, then min id.
        let ccp = (0..n)
            .filter(|&u| u != v && prec(&pos1, u, v) && prec(&pos2, u, v))
            .max_by_key(|&u| (pos1[u].unwrap(), pos2[u].unwrap(), usize::MAX - u));
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
        // closest common successor: min pos1, then min pos2, then min id.
        let ccs = (0..n)
            .filter(|&u| u != v && prec(&pos1, v, u) && prec(&pos2, v, u))
            .min_by_key(|&u| (pos1[u].unwrap(), pos2[u].unwrap(), u));
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
    // Warshall reachability closure and group by mutual reachability rather than a
    // hand-rolled Tarjan.
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

    // Condensation: members, edges between components, and in-degrees.
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); ncomp];
    for (v, &c) in comp.iter().enumerate() {
        members[c].push(v);
    }
    let mut cadj: Vec<Vec<usize>> = vec![Vec::new(); ncomp];
    let mut cindeg = vec![0usize; ncomp];
    for &(a, b) in &edges {
        let (ca, cb) = (comp[a], comp[b]);
        if ca != cb && !cadj[ca].contains(&cb) {
            cadj[ca].push(cb);
            cindeg[cb] += 1;
        }
    }

    // A vertex's tie-break key: earliest in TARGET, then DESIRED, then id.
    let key = |v: usize| {
        (
            pos1[v].unwrap_or(usize::MAX),
            pos2[v].unwrap_or(usize::MAX),
            v,
        )
    };
    let ckey: Vec<(usize, usize, usize)> = (0..ncomp)
        .map(|c| members[c].iter().map(|&v| key(v)).min().unwrap())
        .collect();

    // Step 8: topological sort of the condensation (Kahn), emitting a ready
    // component with the smallest key, each component serialized internally by the
    // marking rule.
    let mut out: Vec<Node<L>> = Vec::with_capacity(n);
    let mut done = vec![false; ncomp];
    for _ in 0..ncomp {
        let c = (0..ncomp)
            .filter(|&c| !done[c] && cindeg[c] == 0)
            .min_by_key(|&c| ckey[c])
            .expect("a DAG condensation always has a ready component");
        for v in order_scc(&members[c], &adj, &pos1, &pos2) {
            out.push(verts[v].clone());
        }
        done[c] = true;
        for &d in &cadj[c] {
            cindeg[d] -= 1;
        }
    }
    out
}

/// Serialize the vertices of one strongly connected component. A trivial (size-1)
/// component is the vertex itself; a cycle is emitted by a marking rule — start
/// from the vertices first in TARGET or DESIRED, follow successor edges,
/// and mark each emitted vertex's successors as eligible — so the intra-cycle
/// order follows an actual input rather than inventing one. Ties break by
/// `key(v)` (earliest in TARGET, then DESIRED, then id).
fn order_scc(
    members: &[usize],
    adj: &[Vec<usize>],
    pos1: &[Option<usize>],
    pos2: &[Option<usize>],
) -> Vec<usize> {
    if members.len() == 1 {
        return vec![members[0]];
    }
    let key = |v: usize| {
        (
            pos1[v].unwrap_or(usize::MAX),
            pos2[v].unwrap_or(usize::MAX),
            v,
        )
    };
    let inset: HashSet<usize> = members.iter().copied().collect();
    let mut marked: HashSet<usize> = HashSet::new();
    // Mark the vertex first in TARGET and the vertex first in DESIRED.
    if let Some(&v) = members
        .iter()
        .filter(|&&v| pos1[v].is_some())
        .min_by_key(|&&v| pos1[v].unwrap())
    {
        marked.insert(v);
    }
    if let Some(&v) = members
        .iter()
        .filter(|&&v| pos2[v].is_some())
        .min_by_key(|&&v| pos2[v].unwrap())
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
