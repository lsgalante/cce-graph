//! The vault as a link graph: one node per note (plus a ghost per link
//! target that does not exist yet, as Obsidian shows them), an edge per
//! pair of notes linked either way, and a force layout that cools and
//! stops so the app goes idle once the picture settles.
//!
//! Pure model, no drawing: positions are world units, the app owns the
//! camera. Kept apart from cce-ui's `Graph` widget on purpose — ports,
//! grid snapping and name-keyed wires are node-editor semantics, and a
//! vault of thousands of notes needs id-keyed edges and a spatial grid.

use std::collections::{HashMap, HashSet, VecDeque};

use cce_vault::index::stem;
use cce_vault::{FileKind, Index};

/// The rest length of a link's spring.
const LINK_LEN: f32 = 90.0;
const LINK_K: f32 = 0.08;
const REPULSE: f32 = 9000.0;
/// Repulsion only reaches this far; past it the centering pull dominates
/// anyway. It is also the spatial grid's cell size.
const REPULSE_RANGE: f32 = 300.0;
const CENTER_K: f32 = 0.006;
const DAMPING: f32 = 0.55;
const COOLING: f32 = 0.985;
/// Below this temperature the layout is settled and stops stepping.
const SETTLED: f32 = 0.004;

#[derive(Clone, Debug)]
pub struct Node {
    /// The note's vault path; for a ghost, the link text that resolves
    /// nowhere.
    pub path: String,
    pub name: String,
    pub ghost: bool,
    pub tags: Vec<String>,
    pub pos: (f32, f32),
    vel: (f32, f32),
    /// Held where the user dropped it.
    pub pinned: bool,
}

#[derive(Default)]
pub struct LinkGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<(usize, usize)>,
    adj: Vec<Vec<usize>>,
    by_path: HashMap<String, usize>,
    /// The layout's temperature: forces scale with it, and it decays.
    pub alpha: f32,
}

impl LinkGraph {
    /// Build from the index. Nodes that were in `prev` keep their place;
    /// new ones start next to a neighbour already placed, or on a spiral.
    pub fn build(ix: &Index, prev: Option<&LinkGraph>) -> LinkGraph {
        let mut g = LinkGraph::default();
        let mut notes: Vec<&str> = ix.files().iter().filter(|(_, e)| e.kind == FileKind::Note).map(|(p, _)| p.as_str()).collect();
        notes.sort();
        for p in notes {
            let tags = ix.note(p).map(|n| n.tags.iter().map(|t| t.name.to_lowercase()).collect()).unwrap_or_default();
            g.add(Node { path: p.to_string(), name: stem(p).to_string(), ghost: false, tags, pos: (0.0, 0.0), vel: (0.0, 0.0), pinned: false });
        }
        let mut seen: HashSet<(usize, usize)> = HashSet::new();
        let count = g.nodes.len();
        for i in 0..count {
            let from = g.nodes[i].path.clone();
            for (link, target) in ix.outgoing(&from) {
                let j = match target {
                    Some(t) => match g.by_path.get(t) {
                        Some(&j) => j,
                        // A link to an attachment or canvas: not drawn.
                        None => continue,
                    },
                    None => {
                        let key = format!("?{}", link.target.to_lowercase());
                        match g.by_path.get(&key) {
                            Some(&j) => j,
                            None => g.add(Node {
                                path: key,
                                name: link.target.clone(),
                                ghost: true,
                                tags: Vec::new(),
                                pos: (0.0, 0.0),
                                vel: (0.0, 0.0),
                                pinned: false,
                            }),
                        }
                    }
                };
                if i != j && seen.insert((i.min(j), i.max(j))) {
                    g.edges.push((i.min(j), i.max(j)));
                }
            }
        }
        g.adj = vec![Vec::new(); g.nodes.len()];
        for &(a, b) in &g.edges {
            g.adj[a].push(b);
            g.adj[b].push(a);
        }
        g.place(prev);
        g.alpha = 1.0;
        g
    }

    fn add(&mut self, node: Node) -> usize {
        let i = self.nodes.len();
        self.by_path.insert(node.path.clone(), i);
        self.nodes.push(node);
        i
    }

    fn place(&mut self, prev: Option<&LinkGraph>) {
        let mut placed = vec![false; self.nodes.len()];
        if let Some(prev) = prev {
            for (i, n) in self.nodes.iter_mut().enumerate() {
                if let Some(&j) = prev.by_path.get(&n.path) {
                    n.pos = prev.nodes[j].pos;
                    n.pinned = prev.nodes[j].pinned;
                    placed[i] = true;
                }
            }
        }
        // A golden-angle spiral spreads the rest without overlaps; a new
        // note with a placed neighbour starts beside it instead.
        for i in 0..self.nodes.len() {
            if placed[i] {
                continue;
            }
            let near = self.adj[i].iter().find(|&&j| placed[j]).map(|&j| self.nodes[j].pos);
            let t = i as f32 * 2.399_963;
            self.nodes[i].pos = match near {
                Some((x, y)) => (x + 20.0 * t.cos(), y + 20.0 * t.sin()),
                None => {
                    let r = 18.0 * (i as f32 + 1.0).sqrt();
                    (r * t.cos(), r * t.sin())
                }
            };
            placed[i] = true;
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn index_of(&self, path: &str) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    pub fn neighbors(&self, i: usize) -> &[usize] {
        &self.adj[i]
    }

    pub fn degree(&self, i: usize) -> usize {
        self.adj[i].len()
    }

    /// Draw radius: grows with the number of links, as Obsidian's does.
    pub fn radius(&self, i: usize) -> f32 {
        3.5 + (self.degree(i) as f32).sqrt() * 2.2
    }

    /// Every node within `hops` links of `center`.
    pub fn within(&self, center: usize, hops: usize) -> Vec<bool> {
        let mut seen = vec![false; self.nodes.len()];
        let mut queue = VecDeque::from([(center, 0usize)]);
        seen[center] = true;
        while let Some((i, d)) = queue.pop_front() {
            if d == hops {
                continue;
            }
            for &j in &self.adj[i] {
                if !seen[j] {
                    seen[j] = true;
                    queue.push_back((j, d + 1));
                }
            }
        }
        seen
    }

    pub fn reheat(&mut self, to: f32) {
        self.alpha = self.alpha.max(to);
    }

    pub fn settled(&self) -> bool {
        self.alpha < SETTLED
    }

    /// One step of the layout over the `visible` nodes (the others are
    /// frozen and exert nothing). False once settled.
    pub fn step(&mut self, visible: &[bool]) -> bool {
        if self.settled() {
            return false;
        }
        let n = self.nodes.len();
        let mut force = vec![(0.0f32, 0.0f32); n];

        // Repulsion between nodes sharing or neighbouring a grid cell.
        let cell = |p: (f32, f32)| ((p.0 / REPULSE_RANGE).floor() as i32, (p.1 / REPULSE_RANGE).floor() as i32);
        let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for i in (0..n).filter(|&i| visible[i]) {
            grid.entry(cell(self.nodes[i].pos)).or_default().push(i);
        }
        let range2 = REPULSE_RANGE * REPULSE_RANGE;
        for (&(cx, cy), members) in &grid {
            for dx in -1..=1 {
                for dy in -1..=1 {
                    let Some(others) = grid.get(&(cx + dx, cy + dy)) else { continue };
                    for &i in members {
                        for &j in others {
                            if j <= i {
                                continue;
                            }
                            let (pi, pj) = (self.nodes[i].pos, self.nodes[j].pos);
                            let (mut ddx, mut ddy) = (pi.0 - pj.0, pi.1 - pj.1);
                            let mut d2 = ddx * ddx + ddy * ddy;
                            if d2 > range2 {
                                continue;
                            }
                            if d2 < 0.01 {
                                // Coincident: nudge apart deterministically.
                                ddx = 0.1 * ((i as f32).sin() + 0.5);
                                ddy = 0.1 * ((j as f32).cos() + 0.5);
                                d2 = ddx * ddx + ddy * ddy;
                            }
                            let f = REPULSE / d2.max(25.0);
                            let d = d2.sqrt();
                            let (fx, fy) = (ddx / d * f, ddy / d * f);
                            force[i].0 += fx;
                            force[i].1 += fy;
                            force[j].0 -= fx;
                            force[j].1 -= fy;
                        }
                    }
                }
            }
        }
        // Springs along the links.
        for &(a, b) in &self.edges {
            if !(visible[a] && visible[b]) {
                continue;
            }
            let (pa, pb) = (self.nodes[a].pos, self.nodes[b].pos);
            let (dx, dy) = (pb.0 - pa.0, pb.1 - pa.1);
            let d = (dx * dx + dy * dy).sqrt().max(0.01);
            let f = (d - LINK_LEN) * LINK_K;
            let (fx, fy) = (dx / d * f, dy / d * f);
            force[a].0 += fx;
            force[a].1 += fy;
            force[b].0 -= fx;
            force[b].1 -= fy;
        }
        // A gentle pull to the middle keeps islands and orphans in view.
        let mut moving = false;
        for i in (0..n).filter(|&i| visible[i]) {
            let node = &mut self.nodes[i];
            if node.pinned {
                node.vel = (0.0, 0.0);
                continue;
            }
            let (fx, fy) = (force[i].0 - node.pos.0 * CENTER_K, force[i].1 - node.pos.1 * CENTER_K);
            node.vel.0 = (node.vel.0 + fx * self.alpha) * DAMPING;
            node.vel.1 = (node.vel.1 + fy * self.alpha) * DAMPING;
            // Cap a step, so a cold start cannot fling a node away.
            let speed = (node.vel.0 * node.vel.0 + node.vel.1 * node.vel.1).sqrt();
            if speed > 40.0 {
                node.vel.0 *= 40.0 / speed;
                node.vel.1 *= 40.0 / speed;
            }
            node.pos.0 += node.vel.0;
            node.pos.1 += node.vel.1;
            moving |= speed > 0.05;
        }
        self.alpha *= COOLING;
        if !moving {
            self.alpha = 0.0;
        }
        !self.settled()
    }

    /// The topmost visible node within its radius (plus `slop`) of a world
    /// point.
    pub fn hit(&self, visible: &[bool], x: f32, y: f32, slop: f32) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for i in (0..self.nodes.len()).filter(|&i| visible[i]) {
            let (px, py) = self.nodes[i].pos;
            let d2 = (px - x).powi(2) + (py - y).powi(2);
            let r = self.radius(i) + slop;
            if d2 <= r * r && best.is_none_or(|(_, b)| d2 < b) {
                best = Some((i, d2));
            }
        }
        best.map(|(i, _)| i)
    }

    /// The bounds of the visible nodes, as (min, max).
    pub fn bounds(&self, visible: &[bool]) -> Option<((f32, f32), (f32, f32))> {
        let mut it = (0..self.nodes.len()).filter(|&i| visible[i]).map(|i| self.nodes[i].pos);
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), p| ((lo.0.min(p.0), lo.1.min(p.1)), (hi.0.max(p.0), hi.1.max(p.1)))))
    }
}

/// What the filter box asks for: every term must match. `#tag` matches a
/// note carrying the tag (or one nested under it), `path:x` a path
/// containing `x`, anything else a name containing it.
#[derive(Default, Debug, PartialEq)]
pub struct Filter {
    tags: Vec<String>,
    paths: Vec<String>,
    words: Vec<String>,
}

impl Filter {
    pub fn parse(q: &str) -> Filter {
        let mut f = Filter::default();
        for term in q.split_whitespace() {
            let t = term.to_lowercase();
            if let Some(tag) = t.strip_prefix('#').or_else(|| t.strip_prefix("tag:")) {
                if !tag.is_empty() {
                    f.tags.push(tag.to_string());
                }
            } else if let Some(p) = t.strip_prefix("path:") {
                if !p.is_empty() {
                    f.paths.push(p.to_string());
                }
            } else {
                f.words.push(t);
            }
        }
        f
    }

    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.paths.is_empty() && self.words.is_empty()
    }

    pub fn matches(&self, n: &Node) -> bool {
        let name = n.name.to_lowercase();
        let path = n.path.to_lowercase();
        self.words.iter().all(|w| name.contains(w.as_str()))
            && self.paths.iter().all(|p| !n.ghost && path.contains(p.as_str()))
            && self.tags.iter().all(|t| n.tags.iter().any(|nt| nt == t || nt.starts_with(&format!("{t}/"))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(files: &[(&str, &str)]) -> (tempfile::TempDir, Index) {
        let dir = tempfile::tempdir().unwrap();
        for (p, t) in files {
            let abs = dir.path().join(p);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(abs, t).unwrap();
        }
        let ix = Index::open(dir.path(), false).unwrap();
        (dir, ix)
    }

    fn sample() -> (tempfile::TempDir, Index) {
        vault(&[
            ("A.md", "[[B]] [[C]] [[Nowhere]] #proj\n"),
            ("B.md", "[[A]] back again\n"),
            ("dir/C.md", "[[D]]\n"),
            ("D.md", "end #proj/sub\n"),
            ("Lone.md", "no links\n"),
            ("pic.png", "x"),
        ])
    }

    #[test]
    fn nodes_edges_and_ghosts() {
        let (_d, ix) = sample();
        let g = LinkGraph::build(&ix, None);
        let names: Vec<&str> = g.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "D", "Lone", "C", "Nowhere"]);
        assert!(g.nodes[5].ghost);
        // A–B once although linked both ways; A–C, A–Nowhere, C–D.
        assert_eq!(g.edges.len(), 4);
        let a = g.index_of("A.md").unwrap();
        assert_eq!(g.degree(a), 3);
        assert!(g.radius(a) > g.radius(g.index_of("Lone.md").unwrap()));
    }

    #[test]
    fn hops_reach_the_neighbourhood() {
        let (_d, ix) = sample();
        let g = LinkGraph::build(&ix, None);
        let d = g.index_of("D.md").unwrap();
        let names = |v: Vec<bool>| -> Vec<String> {
            v.iter().enumerate().filter(|(_, &s)| s).map(|(i, _)| g.nodes[i].name.clone()).collect()
        };
        assert_eq!(names(g.within(d, 1)), ["D", "C"]);
        assert_eq!(names(g.within(d, 2)), ["A", "D", "C"]);
    }

    #[test]
    fn layout_settles_and_keeps_links_short() {
        let (_d, ix) = sample();
        let mut g = LinkGraph::build(&ix, None);
        let all = vec![true; g.len()];
        let mut steps = 0;
        while g.step(&all) {
            steps += 1;
            assert!(steps < 2000, "never settled");
        }
        let dist = |a: &str, b: &str| {
            let (p, q) = (g.nodes[g.index_of(a).unwrap()].pos, g.nodes[g.index_of(b).unwrap()].pos);
            ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt()
        };
        // Linked notes sit closer than an unlinked pair.
        assert!(dist("A.md", "B.md") < dist("B.md", "D.md"), "{} vs {}", dist("A.md", "B.md"), dist("B.md", "D.md"));
        assert!(g.nodes.iter().all(|n| n.pos.0.is_finite() && n.pos.1.is_finite()));
    }

    #[test]
    fn rebuild_keeps_positions_and_pins() {
        let (_d, ix) = sample();
        let mut g = LinkGraph::build(&ix, None);
        let a = g.index_of("A.md").unwrap();
        g.nodes[a].pos = (500.0, -40.0);
        g.nodes[a].pinned = true;
        let g2 = LinkGraph::build(&ix, Some(&g));
        let a2 = g2.index_of("A.md").unwrap();
        assert_eq!(g2.nodes[a2].pos, (500.0, -40.0));
        assert!(g2.nodes[a2].pinned);
    }

    #[test]
    fn filters_by_name_path_and_tag() {
        let (_d, ix) = sample();
        let g = LinkGraph::build(&ix, None);
        let hits = |q: &str| -> Vec<String> {
            let f = Filter::parse(q);
            g.nodes.iter().filter(|n| f.matches(n)).map(|n| n.name.clone()).collect()
        };
        assert_eq!(hits("a"), ["A"]);
        assert_eq!(hits("path:dir/"), ["C"]);
        assert_eq!(hits("#proj"), ["A", "D"]);
        assert_eq!(hits("tag:proj/sub"), ["D"]);
        assert!(Filter::parse("  ").is_empty());
    }

    #[test]
    fn hit_finds_the_nearest_visible_node() {
        let (_d, ix) = sample();
        let mut g = LinkGraph::build(&ix, None);
        for (i, n) in g.nodes.iter_mut().enumerate() {
            n.pos = (i as f32 * 100.0, 0.0);
        }
        let mut vis = vec![true; g.len()];
        assert_eq!(g.hit(&vis, 101.0, 2.0, 0.0), Some(1));
        vis[1] = false;
        assert_eq!(g.hit(&vis, 101.0, 2.0, 0.0), None);
    }
}
