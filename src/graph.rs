// src/graph.rs
//
// The graph: a property graph built by replaying the ledger.
//
//   Node  { id, labels, props }       e.g. "course/calc-3", {course}, {title: "Calc 3"}
//   Edge  { from, label, to, seq }    e.g. note -ABOUT-> topic/partial-derivatives
//
// The ledger is the truth; this is a projection of it. Throw it away and
// replay the ledger and you get the same graph, edge for edge. Nothing
// here writes to disk.
//
// ── Where nodes and edges come from ─────────────────────────────────────────
//
//   Ledger event   Graph effect
//   ────────────   ─────────────────────────────────────────────────────────
//   Received       node for the envelope (its key, or env:<uuid>), labelled
//                  with the first part of its data_type ("note/class" → note)
//                    + HAS_CONTENT  → blob:<sha256>      if it has content
//                    + EMITTED_FROM → parent envelope    if it came from fan-out
//   Transition     VISITED → nest:<id>   (the envelope arrived at that stop)
//   Linked         the edge the Link hop recorded, label as given
//   everything     no change
//   else
//
// Labels on edges written by Mailroom itself (VISITED, HAS_CONTENT,
// EMITTED_FROM) describe where things went. Labels you write with links
// (ABOUT, TAKEN_AT, PART_OF, REVISES, CITES, …) describe what things mean.
// Knowledge queries usually want only the second kind: every envelope that
// visited nest 40 is two VISITED hops from every other one, which would
// make "how is this paper related to Calc 3?" answer "they're in the same
// folder". So walk and path take a label filter.
//
// ── Choices worth knowing ───────────────────────────────────────────────────
//
// Keys. An envelope with meta.key = "course/calc-3" IS the node
// course/calc-3. A later envelope with the same key updates the same node
// (labels add up, props are overwritten by the newer one). A link to a key
// nothing has claimed yet makes an empty node, which fills in when an
// envelope claims the key: wiki-link behaviour.
//
// Set semantics. The same (from, label, to) recorded twice is one edge,
// carrying the seq of the first record. Visiting a nest twice is one
// VISITED edge. Counts of repeated events belong to the ledger, not here.
//
// Event-level causality (one event caused by another) stays in the ledger.
// Making every event a node would add two nodes per stop and bury the
// knowledge graph under bookkeeping.

pub mod snapshot;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::event::EventKind;
use crate::ledger::Record;

pub type NodeId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub labels: BTreeSet<String>,
    pub props: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    pub from: NodeId,
    pub label: String,
    pub to: NodeId,
    /// The ledger record this edge came from.
    pub seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Follow edges from → to.
    Out,
    /// Follow edges backwards, to → from.
    In,
    /// Either way.
    Both,
}

impl Direction {
    pub fn parse(s: &str) -> Option<Direction> {
        match s.to_ascii_lowercase().as_str() {
            "out" => Some(Direction::Out),
            "in" => Some(Direction::In),
            "both" | "any" => Some(Direction::Both),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Graph {
    nodes: BTreeMap<NodeId, Node>,
    edges: Vec<Edge>,
    /// node → indexes into `edges` where the node is `from`
    out: HashMap<NodeId, Vec<usize>>,
    /// node → indexes into `edges` where the node is `to`
    inc: HashMap<NodeId, Vec<usize>>,
    seen: HashSet<(NodeId, String, NodeId)>,
    /// envelope uuid → its node, for envelopes with a key
    envelope_node: HashMap<Uuid, NodeId>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Degree {
    pub id: NodeId,
    pub in_degree: usize,
    pub out_degree: usize,
}

impl Graph {
    /// Replay ledger records, in order, into a graph. The ledger builds
    /// its live graph record by record; this is the same thing in one go,
    /// used by tests to check the live graph against a fresh replay.
    #[allow(dead_code)]
    pub fn from_records(records: &[Record]) -> Graph {
        let mut g = Graph::default();
        for r in records {
            g.apply(r);
        }
        g
    }

    /// Apply one ledger record. See the table at the top of this file.
    pub fn apply(&mut self, r: &Record) {
        let e = &r.event;
        match e.kind {
            EventKind::Received => {
                let Some(facts) = &e.envelope else { return };
                let node = facts.node.clone();
                if let Some(id) = e.envelope_id {
                    self.envelope_node.insert(id, node.clone());
                }
                let n = self.ensure(&node);
                n.labels.insert(kind_of(&facts.data_type));
                n.props
                    .insert("data_type".into(), Value::from(facts.data_type.clone()));
                n.props.insert(
                    "created_at".into(),
                    Value::from(facts.created_at.to_rfc3339()),
                );
                n.props
                    .insert("device".into(), Value::from(r.device.clone()));
                if let Some(title) = &facts.title {
                    n.props.insert("title".into(), Value::from(title.clone()));
                }
                if !facts.tags.is_empty() {
                    n.props
                        .insert("tags".into(), Value::from(facts.tags.clone()));
                }
                if let Some(hash) = &e.content_hash {
                    let blob = format!("blob:{hash}");
                    self.ensure(&blob).labels.insert("blob".into());
                    self.add_edge(&node, "HAS_CONTENT", &blob, r.seq);
                }
                if let Some(parent) = e.envelope_parent {
                    let parent = self.envelope(parent);
                    self.add_edge(&node, "EMITTED_FROM", &parent, r.seq);
                }
            }
            EventKind::Transition => {
                let Some(id) = e.envelope_id else { return };
                let node = self.envelope(id);
                let nest = format!("nest:{}", e.to);
                self.ensure(&nest).labels.insert("nest".into());
                self.add_edge(&node, "VISITED", &nest, r.seq);
            }
            EventKind::Linked => {
                let Some(link) = &e.link else { return };
                let to = self.resolve(&link.to);
                self.add_edge(&link.from, &link.label, &to, r.seq);
            }
            EventKind::Completed | EventKind::Error | EventKind::Interrupted => {}
        }
    }

    // ── Reading ─────────────────────────────────────────────────────────────

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Edges touching `id` in the given direction, optionally of one label.
    pub fn edges_of(&self, id: &str, dir: Direction, label: Option<&str>) -> Vec<&Edge> {
        let mut found = Vec::new();
        if matches!(dir, Direction::Out | Direction::Both) {
            found.extend(
                self.out
                    .get(id)
                    .into_iter()
                    .flatten()
                    .map(|&i| &self.edges[i]),
            );
        }
        if matches!(dir, Direction::In | Direction::Both) {
            found.extend(
                self.inc
                    .get(id)
                    .into_iter()
                    .flatten()
                    .map(|&i| &self.edges[i]),
            );
        }
        found.retain(|e| label.is_none_or(|l| e.label == l));
        found
    }

    /// One step from every node in `from`, along edges with `label`
    /// (any label if None), in direction `dir`. Sorted, no duplicates.
    /// Chain calls to express a pattern:
    ///   hop(hop([paper], ABOUT, Out), ABOUT, In)  =  things about what the paper is about
    pub fn hop(&self, from: &[NodeId], label: Option<&str>, dir: Direction) -> Vec<NodeId> {
        let mut next = BTreeSet::new();
        for id in from {
            for e in self.edges_of(id, dir, label) {
                next.insert(other_end(e, id).to_string());
            }
        }
        next.into_iter().collect()
    }

    /// Keep only the ids whose node has `label`.
    pub fn with_label(&self, ids: Vec<NodeId>, label: &str) -> Vec<NodeId> {
        ids.into_iter()
            .filter(|id| self.nodes.get(id).is_some_and(|n| n.labels.contains(label)))
            .collect()
    }

    /// The Calc 3 query, generalised: nodes labelled `label` that share a
    /// `via` target with `from`. "Notes about what this paper is about."
    pub fn related(&self, from: &str, via: &str, label: &str) -> Vec<NodeId> {
        let shared = self.hop(&[from.to_string()], Some(via), Direction::Out);
        let mut found = self.with_label(self.hop(&shared, Some(via), Direction::In), label);
        found.retain(|id| id != from);
        found
    }

    /// Breadth-first from `start`, up to `max_depth` steps, following only
    /// edges whose label is in `labels` (all labels if empty). Returns each
    /// reachable node (not `start`) with its distance, nearest first.
    pub fn walk(
        &self,
        start: &str,
        max_depth: usize,
        dir: Direction,
        labels: &[&str],
    ) -> Vec<(NodeId, usize)> {
        let mut dist: HashMap<String, usize> = HashMap::from([(start.to_string(), 0)]);
        let mut queue = VecDeque::from([start.to_string()]);
        let mut order = Vec::new();
        while let Some(id) = queue.pop_front() {
            let d = dist[&id];
            if d == max_depth {
                continue;
            }
            let mut next: Vec<String> = self
                .edges_of(&id, dir, None)
                .into_iter()
                .filter(|e| labels.is_empty() || labels.contains(&e.label.as_str()))
                .map(|e| other_end(e, &id).to_string())
                .collect();
            next.sort();
            for n in next {
                if !dist.contains_key(&n) {
                    dist.insert(n.clone(), d + 1);
                    order.push((n.clone(), d + 1));
                    queue.push_back(n);
                }
            }
        }
        order
    }

    /// A shortest path from `a` to `b` (fewest edges), following only
    /// edges whose label is in `labels` (all if empty). Ties go to the
    /// alphabetically first neighbour, so the answer is repeatable.
    pub fn path(&self, a: &str, b: &str, dir: Direction, labels: &[&str]) -> Option<Vec<NodeId>> {
        if !self.nodes.contains_key(a) || !self.nodes.contains_key(b) {
            return None;
        }
        let mut prev: HashMap<String, Option<String>> = HashMap::from([(a.to_string(), None)]);
        let mut queue = VecDeque::from([a.to_string()]);
        while let Some(id) = queue.pop_front() {
            if id == b {
                let mut path = vec![id.clone()];
                let mut cur = id;
                while let Some(Some(p)) = prev.get(&cur) {
                    path.push(p.clone());
                    cur = p.clone();
                }
                path.reverse();
                return Some(path);
            }
            let mut next: Vec<String> = self
                .edges_of(&id, dir, None)
                .into_iter()
                .filter(|e| labels.is_empty() || labels.contains(&e.label.as_str()))
                .map(|e| other_end(e, &id).to_string())
                .collect();
            next.sort();
            for n in next {
                if !prev.contains_key(&n) {
                    prev.insert(n.clone(), Some(id.clone()));
                    queue.push_back(n);
                }
            }
        }
        None
    }

    // ── M1: degree and adjacency ────────────────────────────────────────────

    /// In- and out-degree of every node, sorted by id. Self-loops count
    /// once each way. Sum of in-degrees = sum of out-degrees = edge count.
    pub fn degrees(&self) -> Vec<Degree> {
        self.nodes
            .keys()
            .map(|id| Degree {
                id: id.clone(),
                in_degree: self.inc.get(id).map_or(0, Vec::len),
                out_degree: self.out.get(id).map_or(0, Vec::len),
            })
            .collect()
    }

    // adjacency, mat_mul and mat_pow are M1's matrix view. Only the tests
    // call them today; M2 (eigenvector centrality) builds on them.
    #[allow(dead_code)]
    /// Adjacency matrix over `order`: a[i][j] = number of edges
    /// order[i] → order[j] with a label in `labels` (all if empty).
    /// With set semantics that number is 0 or 1 per label.
    pub fn adjacency(&self, order: &[NodeId], labels: &[&str]) -> Vec<Vec<u64>> {
        let index: HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();
        let mut a = vec![vec![0u64; order.len()]; order.len()];
        for e in &self.edges {
            if !labels.is_empty() && !labels.contains(&e.label.as_str()) {
                continue;
            }
            if let (Some(&i), Some(&j)) = (index.get(e.from.as_str()), index.get(e.to.as_str())) {
                a[i][j] += 1;
            }
        }
        a
    }

    /// Edge list for NetworkX: `nx.read_edgelist(..., delimiter=",", create_using=nx.MultiDiGraph)`
    /// or pandas. One header line, then from,to,label,seq. Fields containing
    /// a comma or quote are quoted.
    pub fn edges_csv(&self) -> String {
        let mut out = String::from("source,target,label,seq\n");
        for e in &self.edges {
            out.push_str(&format!(
                "{},{},{},{}\n",
                csv(&e.from),
                csv(&e.to),
                csv(&e.label),
                e.seq
            ));
        }
        out
    }

    // ── Snapshot support ────────────────────────────────────────────────────

    /// Envelopes received with a key, and the node each one is. Sorted by
    /// uuid so a snapshot of the same graph is the same bytes.
    pub fn envelope_keys(&self) -> Vec<(Uuid, &NodeId)> {
        let mut keys: Vec<_> = self.envelope_node.iter().map(|(u, n)| (*u, n)).collect();
        keys.sort();
        keys
    }

    /// Rebuild a graph from what a snapshot stores. The indexes (out, in,
    /// seen) are derived, not stored, so they can't disagree with the edges.
    /// Rejects edges that point at nodes the snapshot doesn't hold.
    pub fn from_parts(
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        env_keys: Vec<(Uuid, NodeId)>,
    ) -> anyhow::Result<Graph> {
        let mut g = Graph::default();
        for n in nodes {
            let id = n.id.clone();
            if g.nodes.insert(id.clone(), n).is_some() {
                anyhow::bail!("snapshot holds node {id:?} twice");
            }
        }
        for e in edges {
            for end in [&e.from, &e.to] {
                if !g.nodes.contains_key(end) {
                    anyhow::bail!(
                        "snapshot edge {} -{}-> {} names missing node {end:?}",
                        e.from,
                        e.label,
                        e.to
                    );
                }
            }
            if !g
                .seen
                .insert((e.from.clone(), e.label.clone(), e.to.clone()))
            {
                anyhow::bail!(
                    "snapshot holds edge {} -{}-> {} twice",
                    e.from,
                    e.label,
                    e.to
                );
            }
            let i = g.edges.len();
            g.out.entry(e.from.clone()).or_default().push(i);
            g.inc.entry(e.to.clone()).or_default().push(i);
            g.edges.push(e);
        }
        g.envelope_node = env_keys.into_iter().collect();
        Ok(g)
    }

    // ── Building ────────────────────────────────────────────────────────────

    fn ensure(&mut self, id: &str) -> &mut Node {
        self.nodes.entry(id.to_string()).or_insert_with(|| Node {
            id: id.to_string(),
            labels: BTreeSet::new(),
            props: BTreeMap::new(),
        })
    }

    fn add_edge(&mut self, from: &str, label: &str, to: &str, seq: u64) {
        let key = (from.to_string(), label.to_string(), to.to_string());
        if !self.seen.insert(key) {
            return;
        }
        self.ensure(from);
        self.ensure(to);
        let i = self.edges.len();
        self.edges.push(Edge {
            from: from.to_string(),
            label: label.to_string(),
            to: to.to_string(),
            seq,
        });
        self.out.entry(from.to_string()).or_default().push(i);
        self.inc.entry(to.to_string()).or_default().push(i);
    }

    /// The node for an envelope: its key if it was received with one,
    /// otherwise env:<uuid>.
    fn envelope(&mut self, id: Uuid) -> NodeId {
        let node = self
            .envelope_node
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("env:{id}"));
        self.ensure(&node);
        node
    }

    /// A link target as recorded ("env:<uuid>" or a key) → node id.
    fn resolve(&mut self, target: &str) -> NodeId {
        if let Some(id) = target
            .strip_prefix("env:")
            .and_then(|u| Uuid::parse_str(u).ok())
        {
            return self.envelope(id);
        }
        target.to_string()
    }
}

#[allow(dead_code)]
/// Matrix product, for counting walks: (A^k)[i][j] = number of walks of
/// length k from i to j.
pub fn mat_mul(a: &[Vec<u64>], b: &[Vec<u64>]) -> Vec<Vec<u64>> {
    let n = a.len();
    let m = b.first().map_or(0, Vec::len);
    let mut c = vec![vec![0u64; m]; n];
    for i in 0..n {
        for k in 0..b.len() {
            if a[i][k] == 0 {
                continue;
            }
            for j in 0..m {
                c[i][j] += a[i][k] * b[k][j];
            }
        }
    }
    c
}

#[allow(dead_code)]
/// A^k for k ≥ 1.
pub fn mat_pow(a: &[Vec<u64>], k: u32) -> Vec<Vec<u64>> {
    assert!(k >= 1, "A^0 is the identity; ask for k >= 1");
    let mut result = a.to_vec();
    for _ in 1..k {
        result = mat_mul(&result, a);
    }
    result
}

fn other_end<'a>(e: &'a Edge, id: &str) -> &'a str {
    if e.from == id {
        &e.to
    } else {
        &e.from
    }
}

/// "note/class" → "note"; "paper" → "paper".
fn kind_of(data_type: &str) -> String {
    data_type.split('/').next().unwrap_or(data_type).to_string()
}

fn csv(field: &str) -> String {
    if field.contains([',', '"', '\n']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, LinkSpec, Payload, Source};
    use crate::event::{Event, LinkFacts};
    use crate::route::{Hop, Route, Stop};

    fn linked(seq: u64, from: &str, label: &str, to: &str) -> Record {
        let mut event = Event::new("n", "n", None, None, EventKind::Linked, None);
        event.link = Some(LinkFacts {
            from: from.into(),
            label: label.into(),
            to: to.into(),
        });
        Record {
            v: 1,
            seq,
            device: "t".into(),
            event,
        }
    }

    fn ids(v: &[&str]) -> Vec<NodeId> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ── M1, worked by hand ──────────────────────────────────────────────
    //
    //   A → B, A → C, B → C, C → D
    //
    //   out-degree  A 2  B 1  C 1  D 0   sum 4
    //   in-degree   A 0  B 1  C 2  D 1   sum 4  = |E|
    //
    //   A (rows/cols A B C D)   A² = walks of length 2   A³
    //     0 1 1 0               0 0 1 1                  0 0 0 1
    //     0 0 1 0               0 0 0 1                  0 0 0 0
    //     0 0 0 1               0 0 0 0                  0 0 0 0
    //     0 0 0 0               0 0 0 0                  0 0 0 0
    //
    //   A²[A][C] = 1: A→B→C.   A²[A][D] = 1: A→C→D.   A³[A][D] = 1: A→B→C→D.
    #[test]
    fn m1_degrees_and_walk_counts_match_the_hand_calculation() {
        let g = Graph::from_records(&[
            linked(1, "A", "R", "B"),
            linked(2, "A", "R", "C"),
            linked(3, "B", "R", "C"),
            linked(4, "C", "R", "D"),
        ]);

        let d: Vec<(usize, usize)> = g
            .degrees()
            .iter()
            .map(|d| (d.out_degree, d.in_degree))
            .collect();
        assert_eq!(d, vec![(2, 0), (1, 1), (1, 2), (0, 1)]);
        let (outs, ins): (Vec<usize>, Vec<usize>) = d.into_iter().unzip();
        assert_eq!(outs.iter().sum::<usize>(), g.edge_count());
        assert_eq!(ins.iter().sum::<usize>(), g.edge_count());

        let order = ids(&["A", "B", "C", "D"]);
        let a = g.adjacency(&order, &[]);
        assert_eq!(
            a,
            vec![
                vec![0, 1, 1, 0],
                vec![0, 0, 1, 0],
                vec![0, 0, 0, 1],
                vec![0, 0, 0, 0]
            ]
        );
        assert_eq!(
            mat_pow(&a, 2),
            vec![
                vec![0, 0, 1, 1],
                vec![0, 0, 0, 1],
                vec![0, 0, 0, 0],
                vec![0, 0, 0, 0]
            ]
        );
        assert_eq!(mat_pow(&a, 3)[0][3], 1);
        assert_eq!(
            mat_pow(&a, 4),
            vec![vec![0; 4]; 4],
            "no walk of length 4 in a 4-node DAG path"
        );
    }

    #[test]
    fn the_same_link_twice_is_one_edge() {
        let g = Graph::from_records(&[linked(1, "a", "ABOUT", "b"), linked(2, "a", "ABOUT", "b")]);
        assert_eq!(g.edge_count(), 1);
        assert_eq!(
            g.edges()[0].seq,
            1,
            "the edge keeps the record that first made it"
        );
    }

    #[test]
    fn a_link_to_an_unclaimed_key_makes_a_node_that_fills_in_later() {
        let mut g = Graph::from_records(&[linked(1, "note-1", "ABOUT", "topic/trig")]);
        assert!(g.node("topic/trig").unwrap().labels.is_empty());

        let mut topic = Envelope::new(
            Source::Manual,
            "topic",
            Payload::Text("Trigonometry".into()),
            None::<&str>,
        );
        topic.meta.insert("key".into(), "topic/trig".into());
        topic.meta.insert("title".into(), "Trigonometry".into());
        let mut received = Event::new(
            "intake",
            "x",
            None,
            Some(topic.id),
            EventKind::Received,
            None,
        );
        received.envelope = Some(crate::event::EnvelopeFacts::of(&topic));
        g.apply(&Record {
            v: 1,
            seq: 2,
            device: "t".into(),
            event: received,
        });

        let n = g.node("topic/trig").unwrap();
        assert!(n.labels.contains("topic"));
        assert_eq!(n.props["title"], "Trigonometry");
        assert_eq!(
            g.hop(&ids(&["topic/trig"]), Some("ABOUT"), Direction::In),
            ids(&["note-1"])
        );
    }

    // ── The Calc 3 query, through real routes ───────────────────────────
    //
    // Everything below goes through Route::run with a Link hop, into a real
    // ledger on disk, and the graph is rebuilt from that ledger.
    //
    //   course/calc-3                       (course)
    //     ↑ PART_OF            ↑ PART_OF
    //   session/calc-3/4      session/calc-3/6
    //     ↑ TAKEN_AT            ↑ TAKEN_AT
    //   note4 ──ABOUT──┐      note6 ──ABOUT──→ topic/trigonometry
    //     │  └─ABOUT───┼────────────────────→ topic/trigonometry
    //     ↑ REVISES    ↓
    //   rewrite ─ABOUT→ topic/partial-derivatives ←ABOUT─ paper
    //
    // By hand:
    //   related(paper, ABOUT, note)       = {note4, rewrite}   (note6 is trig only)
    //   walk(course, In, knowledge edges) = s4, s6 (1); note4, note6 (2); rewrite (3)
    //   path(paper → course), knowledge edges only
    //                                     = paper, topic/pd, note4, s4, course
    //   path(paper → course), any edge    = paper, nest:40, course
    //     (both visited nest 40: the folder, not the meaning)
    async fn send(
        vault: &std::path::Path,
        data_type: &str,
        key: Option<&str>,
        title: &str,
        links: &[(&str, &str)],
    ) -> Envelope {
        let mut e = Envelope::new(
            Source::Manual,
            data_type,
            Payload::Text(title.into()),
            None::<&str>,
        );
        if let Some(k) = key {
            e.meta.insert("key".into(), k.into());
        }
        e.meta.insert("title".into(), title.into());
        e.links = links
            .iter()
            .map(|(l, t)| LinkSpec {
                label: l.to_string(),
                to: t.to_string(),
            })
            .collect();
        let mut route = Route::new(
            "librarian",
            vec![Stop {
                nest_id: "40".into(),
                hops: vec![Hop::Link],
            }],
        );
        route.run(vault, Some(&e)).await.unwrap();
        e
    }

    #[tokio::test]
    async fn the_calc_3_query_finds_the_class_notes_for_a_paper() {
        let vault = tempfile::tempdir().unwrap();
        let v = vault.path();
        send(v, "course", Some("course/calc-3"), "Calc 3", &[]).await;
        send(
            v,
            "session",
            Some("session/calc-3/4"),
            "Calc 3 class 4",
            &[("PART_OF", "course/calc-3")],
        )
        .await;
        send(
            v,
            "session",
            Some("session/calc-3/6"),
            "Calc 3 class 6",
            &[("part of", "course/calc-3")],
        )
        .await;
        send(
            v,
            "topic",
            Some("topic/partial-derivatives"),
            "Partial derivatives",
            &[],
        )
        .await;
        let note4 = send(
            v,
            "note/class",
            None,
            "Class 4 notes",
            &[
                ("TAKEN_AT", "session/calc-3/4"),
                ("ABOUT", "topic/partial-derivatives"),
                ("ABOUT", "topic/trigonometry"),
            ],
        )
        .await;
        let rewrite = send(
            v,
            "note/rewrite",
            None,
            "Partial derivatives, in my words",
            &[
                ("REVISES", &note4.id.to_string()),
                ("ABOUT", "topic/partial-derivatives"),
            ],
        )
        .await;
        let note6 = send(
            v,
            "note/class",
            None,
            "Class 6 notes",
            &[
                ("TAKEN_AT", "session/calc-3/6"),
                ("ABOUT", "topic/trigonometry"),
            ],
        )
        .await;
        let paper = send(
            v,
            "paper",
            None,
            "A paper on gradients",
            &[("ABOUT", "topic/partial-derivatives")],
        )
        .await;

        let records = crate::ledger::for_vault(v).unwrap().records().unwrap();
        let g = Graph::from_records(&records);
        let (n4, rw, n6, pp) = (
            note4.node_id(),
            rewrite.node_id(),
            note6.node_id(),
            paper.node_id(),
        );

        // The query itself.
        let mut expected = vec![n4.clone(), rw.clone()];
        expected.sort();
        assert_eq!(g.related(&pp, "ABOUT", "note"), expected);

        // "Everything in Calc 3."
        let knowledge = ["PART_OF", "TAKEN_AT", "REVISES", "ABOUT"];
        let walk = g.walk("course/calc-3", 5, Direction::In, &knowledge);
        let mut by_depth: Vec<(usize, String)> = walk.into_iter().map(|(id, d)| (d, id)).collect();
        by_depth.sort();
        let mut want = vec![
            (1, "session/calc-3/4".to_string()),
            (1, "session/calc-3/6".to_string()),
            (2, n4.clone()),
            (2, n6.clone()),
            (3, rw.clone()),
        ];
        want.sort();
        assert_eq!(by_depth, want);

        // How a paper connects to the course: by meaning, and by folder.
        assert_eq!(
            g.path(&pp, "course/calc-3", Direction::Both, &knowledge)
                .unwrap(),
            vec![
                pp.clone(),
                "topic/partial-derivatives".into(),
                n4.clone(),
                "session/calc-3/4".into(),
                "course/calc-3".into()
            ]
        );
        assert_eq!(
            g.path(&pp, "course/calc-3", Direction::Both, &[]).unwrap(),
            vec![pp.clone(), "nest:40".into(), "course/calc-3".into()]
        );

        // Odds and ends the hand diagram also implies.
        assert_eq!(g.node("course/calc-3").unwrap().props["title"], "Calc 3");
        assert!(
            g.node(&n4).unwrap().labels.contains("note"),
            "note/class is labelled note"
        );
        assert!(
            g.node("topic/trigonometry").unwrap().labels.is_empty(),
            "linked, never received"
        );
        assert_eq!(
            g.edges_of(&rw, Direction::Out, Some("REVISES"))[0].to,
            n4,
            "a UUID link resolves to the envelope's node"
        );
        assert_eq!(
            g.edges_of(&pp, Direction::Out, Some("HAS_CONTENT")).len(),
            1
        );
    }

    #[tokio::test]
    async fn a_bad_link_label_records_none_of_the_envelopes_links() {
        let vault = tempfile::tempdir().unwrap();
        let mut e = Envelope::new(
            Source::Manual,
            "note",
            Payload::Text("x".into()),
            None::<&str>,
        );
        e.links = vec![
            LinkSpec {
                label: "ABOUT".into(),
                to: "topic/a".into(),
            },
            LinkSpec {
                label: "about!".into(),
                to: "topic/b".into(),
            },
        ];
        let mut route = Route::new(
            "t",
            vec![Stop {
                nest_id: "40".into(),
                hops: vec![Hop::Link],
            }],
        );
        assert!(route.run(vault.path(), Some(&e)).await.is_err());

        let records = crate::ledger::for_vault(vault.path())
            .unwrap()
            .records()
            .unwrap();
        assert!(records.iter().all(|r| r.event.kind != EventKind::Linked));
    }

    #[test]
    fn edges_csv_quotes_awkward_fields() {
        let g = Graph::from_records(&[linked(1, "a,b", "ABOUT", "say \"hi\"")]);
        assert_eq!(
            g.edges_csv(),
            "source,target,label,seq\n\"a,b\",\"say \"\"hi\"\"\",ABOUT,1\n"
        );
    }
}
