// src/graph/centrality.rs
//
// M2: eigenvector centrality. A node is important if its neighbours are
// important. Written as an equation, with A the adjacency matrix and x the
// scores:
//
//     λ x = A x        x_i = (1/λ) Σ_j A_ij x_j
//
// so x is an eigenvector of A. Perron–Frobenius says that for a connected
// graph the eigenvector of the largest eigenvalue λ₁ has all entries
// positive and is unique up to scale. That one is the centrality.
//
// ── Power iteration ─────────────────────────────────────────────────────────
//
// Start from x = (1, 1, …, 1) and repeat x ← A x, normalised. Write the
// start as a sum of eigenvectors: after k steps each part has been scaled
// by λᵢᵏ, so the λ₁ part wins as long as |λ₁| is strictly the largest.
//
// It isn't, on a bipartite graph. There −λ₁ is also an eigenvalue, the
// two parts stay the same size, and x flips between two vectors forever.
// A path a–b–c is bipartite; so is every tree, and so is a graph of notes
// that only point at topics. So iterate with (A + I) instead: same
// eigenvectors, eigenvalues shifted by +1, and now λ₁ + 1 > |−λ₁ + 1|.
// NetworkX does the same. The test `plain_iteration_oscillates_on_a_path`
// shows the failure the shift fixes.
//
// ── Three choices that matter more than the formula ─────────────────────────
//
// 1. Undirected. Mailroom's meaning edges mostly point one way (note →
//    topic, session → course), so the directed graph is close to acyclic.
//    For an acyclic graph every eigenvalue is 0 and power iteration decays
//    to the zero vector: no answer at all. Centrality here treats a→b and
//    b→a as the same connection. The directed question ("who is pointed
//    at by important things") is PageRank, which adds teleportation to
//    get around exactly this; it's the stretch goal.
//
// 2. Per connected component. On a disconnected graph, iterating over
//    everything converges to the eigenvector of whichever component has
//    the largest λ, and every other component decays to zero. Those zeros
//    say nothing about the nodes in them. So each component is computed
//    on its own, normalised to unit length on its own, and reported with
//    its own λ. Scores compare within a component, not across.
//
// 3. Meaning edges by default. VISITED, HAS_CONTENT and EMITTED_FROM
//    record where things went. Left in, a nest every envelope visited is
//    the most "central" node in the vault, which is true and useless.
//
// Weights: two nodes joined by several edges (any labels, either
// direction) get A_ij = the number of them. A self-loop adds 1 to A_ii.

use std::collections::{BTreeSet, HashMap, VecDeque};

use serde::Serialize;

use super::{Graph, NodeId};

/// Edge labels Mailroom writes itself, left out by default.
pub const SYSTEM_LABELS: [&str; 3] = ["VISITED", "HAS_CONTENT", "EMITTED_FROM"];

pub const TOLERANCE: f64 = 1e-10;
pub const MAX_ITERATIONS: usize = 10_000;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Scored {
    pub id: NodeId,
    /// Eigenvector centrality within its component (unit length per component).
    pub score: f64,
    /// Number of distinct neighbours, ignoring direction.
    pub degree: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Component {
    /// Nodes, highest score first; ties by id.
    pub nodes: Vec<Scored>,
    /// Largest eigenvalue of this component's A (Rayleigh quotient xᵀAx).
    pub lambda: f64,
    pub iterations: usize,
}

/// Which edges to use: `labels` if non-empty, otherwise every label except
/// SYSTEM_LABELS. Nodes with no included edge are left out.
pub fn eigenvector(graph: &Graph, labels: &[&str]) -> anyhow::Result<Vec<Component>> {
    let keep = |l: &str| {
        if labels.is_empty() {
            !SYSTEM_LABELS.contains(&l)
        } else {
            labels.contains(&l)
        }
    };

    // Undirected weighted adjacency over the nodes that have an included edge.
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut ids: Vec<&NodeId> = Vec::new();
    let mut weight: Vec<HashMap<usize, f64>> = Vec::new();
    fn idx<'g>(
        id: &'g NodeId,
        index: &mut HashMap<&'g str, usize>,
        ids: &mut Vec<&'g NodeId>,
        weight: &mut Vec<HashMap<usize, f64>>,
    ) -> usize {
        *index.entry(id.as_str()).or_insert_with(|| {
            ids.push(id);
            weight.push(HashMap::new());
            ids.len() - 1
        })
    }
    for e in graph.edges().iter().filter(|e| keep(&e.label)) {
        let a = idx(&e.from, &mut index, &mut ids, &mut weight);
        let b = idx(&e.to, &mut index, &mut ids, &mut weight);
        *weight[a].entry(b).or_default() += 1.0;
        if a != b {
            *weight[b].entry(a).or_default() += 1.0;
        }
    }
    let adj: Vec<Vec<(usize, f64)>> = weight
        .iter()
        .map(|m| {
            let mut v: Vec<(usize, f64)> = m.iter().map(|(&j, &w)| (j, w)).collect();
            v.sort_by_key(|&(j, _)| j);
            v
        })
        .collect();

    let mut components = Vec::new();
    for members in connected_components(&adj) {
        let (x, iterations) = power_iteration(&adj, &members, true)
            .map_err(|it| anyhow::anyhow!("power iteration did not converge in {it} steps"))?;
        let lambda = rayleigh(&adj, &members, &x);
        let mut nodes: Vec<Scored> = members
            .iter()
            .zip(&x)
            .map(|(&i, &score)| Scored {
                id: ids[i].clone(),
                score,
                degree: adj[i].iter().filter(|&&(j, _)| j != i).count(),
            })
            .collect();
        nodes.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        components.push(Component {
            nodes,
            lambda,
            iterations,
        });
    }
    // Biggest component first; ties by its first node's id, so output is stable.
    components.sort_by(|a, b| {
        b.nodes
            .len()
            .cmp(&a.nodes.len())
            .then_with(|| a.nodes[0].id.cmp(&b.nodes[0].id))
    });
    Ok(components)
}

/// Components as sorted lists of node indexes, found by BFS.
fn connected_components(adj: &[Vec<(usize, f64)>]) -> Vec<Vec<usize>> {
    let mut seen = vec![false; adj.len()];
    let mut out = Vec::new();
    for start in 0..adj.len() {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut members = BTreeSet::from([start]);
        let mut queue = VecDeque::from([start]);
        while let Some(i) = queue.pop_front() {
            for &(j, _) in &adj[i] {
                if !seen[j] {
                    seen[j] = true;
                    members.insert(j);
                    queue.push_back(j);
                }
            }
        }
        out.push(members.into_iter().collect());
    }
    out
}

/// Power iteration on one component. `shift` iterates with (A + I).
/// Ok((x, steps)): x has unit length, entries in `members` order.
/// Err(steps) if it didn't settle within MAX_ITERATIONS.
fn power_iteration(
    adj: &[Vec<(usize, f64)>],
    members: &[usize],
    shift: bool,
) -> Result<(Vec<f64>, usize), usize> {
    let pos: HashMap<usize, usize> = members.iter().enumerate().map(|(p, &i)| (i, p)).collect();
    let n = members.len();
    let mut x = vec![1.0 / (n as f64).sqrt(); n];
    for step in 1..=MAX_ITERATIONS {
        let mut next = if shift { x.clone() } else { vec![0.0; n] };
        for (p, &i) in members.iter().enumerate() {
            for &(j, w) in &adj[i] {
                next[p] += w * x[pos[&j]];
            }
        }
        let norm = next.iter().map(|v| v * v).sum::<f64>().sqrt();
        if norm == 0.0 {
            // A component with no edges inside it (can't happen here: every
            // node arrived with an edge). Every node is equally central.
            return Ok((vec![1.0 / (n as f64).sqrt(); n], step));
        }
        next.iter_mut().for_each(|v| *v /= norm);
        let change = next.iter().zip(&x).map(|(a, b)| (a - b).abs()).sum::<f64>();
        x = next;
        if change < TOLERANCE * n as f64 {
            return Ok((x, step));
        }
    }
    Err(MAX_ITERATIONS)
}

/// xᵀ A x for a unit vector x: the eigenvalue x belongs to.
fn rayleigh(adj: &[Vec<(usize, f64)>], members: &[usize], x: &[f64]) -> f64 {
    let pos: HashMap<usize, usize> = members.iter().enumerate().map(|(p, &i)| (i, p)).collect();
    members
        .iter()
        .enumerate()
        .map(|(p, &i)| x[p] * adj[i].iter().map(|&(j, w)| w * x[pos[&j]]).sum::<f64>())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventKind, LinkFacts};
    use crate::ledger::Record;

    fn graph(edges: &[(&str, &str, &str)]) -> Graph {
        let records: Vec<Record> = edges
            .iter()
            .enumerate()
            .map(|(i, (from, label, to))| {
                let mut event = Event::new("n", "n", None, None, EventKind::Linked, None);
                event.link = Some(LinkFacts {
                    from: from.to_string(),
                    label: label.to_string(),
                    to: to.to_string(),
                });
                Record {
                    v: 1,
                    seq: i as u64 + 1,
                    device: "t".into(),
                    event,
                }
            })
            .collect();
        Graph::from_records(&records)
    }

    fn score(c: &Component, id: &str) -> f64 {
        c.nodes.iter().find(|n| n.id == id).unwrap().score
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-8
    }

    // ── By hand ──────────────────────────────────────────────────────────
    //
    // Path a – b – c.        A = 0 1 0     characteristic polynomial
    //                            1 0 1     −λ³ + 2λ = 0  →  λ ∈ {−√2, 0, √2}
    //                            0 1 0
    //
    // For λ = √2: from row 1, b = √2·a; row 3, b = √2·c; so a = c and
    // x ∝ (1, √2, 1). Length √(1 + 2 + 1) = 2, so x = (1/2, √2/2, 1/2).
    // The middle node is √2 ≈ 1.41 times as central as the ends.
    #[test]
    fn a_path_of_three_matches_the_hand_calculation() {
        let g = graph(&[("a", "R", "b"), ("b", "R", "c")]);
        let c = &eigenvector(&g, &[]).unwrap()[0];
        assert!(close(score(c, "a"), 0.5));
        assert!(close(score(c, "b"), 2f64.sqrt() / 2.0));
        assert!(close(score(c, "c"), 0.5));
        assert!(close(c.lambda, 2f64.sqrt()));
        assert_eq!(c.nodes[0].id, "b");
    }

    // The same path, iterated without the +I shift, from (1,1,1)/√3:
    //   A(1,1,1) = (1,2,1)   → (1,2,1)/√6
    //   A(1,2,1) = (2,2,2)   → (1,1,1)/√3   … back where it started.
    // It never settles, because −√2 is as large as √2.
    #[test]
    fn plain_iteration_oscillates_on_a_path_and_the_shift_fixes_it() {
        let adj = vec![vec![(1, 1.0)], vec![(0, 1.0), (2, 1.0)], vec![(1, 1.0)]];
        assert_eq!(
            power_iteration(&adj, &[0, 1, 2], false),
            Err(MAX_ITERATIONS)
        );
        assert!(power_iteration(&adj, &[0, 1, 2], true).is_ok());
    }

    // Star: centre z, leaves p q r. Guess x = (z, l, l, l):
    //   row z: λ z = 3 l        rows p q r: λ l = z
    //   → λ² = 3, λ = √3, z = √3 · l
    //   unit length: 3l² + 3l² = 1 → l = 1/√6, z = 1/√2.
    #[test]
    fn a_star_matches_the_hand_calculation() {
        let g = graph(&[("z", "R", "p"), ("q", "R", "z"), ("z", "R", "r")]);
        let c = &eigenvector(&g, &[]).unwrap()[0];
        assert!(close(score(c, "z"), 1.0 / 2f64.sqrt()));
        for leaf in ["p", "q", "r"] {
            assert!(close(score(c, leaf), 1.0 / 6f64.sqrt()), "{leaf}");
        }
        assert!(close(c.lambda, 3f64.sqrt()));
        assert_eq!(c.nodes[0].degree, 3);
    }

    // A triangle (λ = 2, each 1/√3) and a separate pair (λ = 1, each 1/√2).
    // Over the whole graph, (A+I)ᵏ grows the triangle by 3ᵏ and the pair by
    // 2ᵏ, so the pair would fade to 0. Per component, both get real scores.
    #[test]
    fn each_component_gets_its_own_scores_and_eigenvalue() {
        let g = graph(&[
            ("t1", "R", "t2"),
            ("t2", "R", "t3"),
            ("t3", "R", "t1"),
            ("p1", "R", "p2"),
        ]);
        let cs = eigenvector(&g, &[]).unwrap();
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].nodes.len(), 3, "biggest component first");
        assert!(close(cs[0].lambda, 2.0));
        assert!(cs[0]
            .nodes
            .iter()
            .all(|n| close(n.score, 1.0 / 3f64.sqrt())));
        assert!(close(cs[1].lambda, 1.0));
        assert!(cs[1]
            .nodes
            .iter()
            .all(|n| close(n.score, 1.0 / 2f64.sqrt())));
    }

    // a→b→c→d is acyclic. As a directed matrix it is nilpotent (A⁴ = 0), so
    // directed power iteration would end at the zero vector. Undirected,
    // it's a path of four: λ = golden ratio φ ≈ 1.618, and by symmetry the
    // two middle nodes outrank the ends by exactly φ.
    #[test]
    fn direction_is_ignored_so_an_acyclic_chain_still_has_an_answer() {
        let g = graph(&[("a", "R", "b"), ("b", "R", "c"), ("c", "R", "d")]);
        let reversed = graph(&[("b", "R", "a"), ("c", "R", "b"), ("d", "R", "c")]);
        let (c, r) = (
            &eigenvector(&g, &[]).unwrap()[0],
            &eigenvector(&reversed, &[]).unwrap()[0],
        );
        let phi = (1.0 + 5f64.sqrt()) / 2.0;
        assert!(close(c.lambda, phi));
        assert!(close(score(c, "b") / score(c, "a"), phi));
        assert_eq!(c.nodes, r.nodes);
    }

    #[test]
    fn system_edges_are_left_out_unless_asked_for() {
        // Three notes all visited one nest; only two share a topic.
        let g = graph(&[
            ("n1", "VISITED", "nest:40"),
            ("n2", "VISITED", "nest:40"),
            ("n3", "VISITED", "nest:40"),
            ("n1", "ABOUT", "topic"),
            ("n2", "ABOUT", "topic"),
        ]);
        let meaning = eigenvector(&g, &[]).unwrap();
        assert!(meaning
            .iter()
            .flat_map(|c| &c.nodes)
            .all(|n| n.id != "nest:40"));
        assert_eq!(meaning[0].nodes[0].id, "topic");

        let visits = eigenvector(&g, &["VISITED"]).unwrap();
        assert_eq!(visits[0].nodes[0].id, "nest:40");
    }

    #[test]
    fn parallel_edges_add_weight() {
        // a–b twice (ABOUT and CITES, opposite directions), b–c once.
        // A = [[0,2,0],[2,0,1],[0,1,0]]: λ² = 5, x ∝ (2, √5, 1), length √10.
        let g = graph(&[
            ("a", "ABOUT", "b"),
            ("b", "CITES", "a"),
            ("b", "ABOUT", "c"),
        ]);
        let c = &eigenvector(&g, &[]).unwrap()[0];
        assert!(close(c.lambda, 5f64.sqrt()));
        assert!(close(score(c, "a"), 2.0 / 10f64.sqrt()));
        assert!(close(score(c, "b"), 5f64.sqrt() / 10f64.sqrt()));
        assert!(close(score(c, "c"), 1.0 / 10f64.sqrt()));
        assert_eq!(
            c.nodes.iter().find(|n| n.id == "a").unwrap().degree,
            1,
            "degree counts neighbours, not edges"
        );
    }
}
