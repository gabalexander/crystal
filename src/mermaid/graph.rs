//! Boxes and arrows laid out in layers, the one layout every diagram but
//! the sequence goes through: a flowchart as written, a state diagram's
//! states and transitions, a class or ER diagram's entities and relations.
//!
//! It is the classic Sugiyama pipeline, cut down to what a character grid
//! can show:
//!
//! 1. **Cycles are broken** by turning the back edges of a depth-first walk
//!    around for the layout only: the arrowhead still points the way the
//!    author wrote it, it just enters its node from the other side.
//! 2. **Layering** is longest path from the sources, with each source then
//!    pulled down to sit just above its nearest successor, so a node that
//!    only feeds the bottom of the graph is not stranded at the top with a
//!    long edge hanging off it. An edge that spans several layers gets a
//!    dummy node in each layer it crosses, so every edge afterwards only
//!    ever joins neighbouring layers.
//! 3. **Ordering** within a layer is barycentric (each node goes to the
//!    mean position of its neighbours in the layer before), swept down and
//!    up twice, keeping whichever ordering crossed least. Subgraphs sort as
//!    one key, so a subgraph's members stay side by side in every layer and
//!    its frame can be drawn around them.
//! 4. **Placement** packs each layer from the left, a subgraph's block at
//!    the same offset in every layer it spans, then nudges every node toward
//!    the middle of its neighbours so edges run straight where they can.
//! 5. **Routing** is orthogonal: an edge leaves the middle of its node's far
//!    side, runs along the gap between two layers on a track of its own,
//!    and enters its target at a port of its own. Tracks are assigned so no
//!    two horizontal runs share cells and no corner lands on another edge's
//!    line, which is what keeps a `┼` meaning "crossing" and a `┬` meaning
//!    "joins".
//!
//! Everything is computed in two abstract axes, *along* the layers and
//! *across* them, and mapped to the screen at the end, so `LR` is the same
//! code as `TD` with the axes swapped. `BT` and `RL` are `TD` and `LR` with
//! every edge turned around.

use std::collections::HashMap;

use super::canvas::{Canvas, DOWN, Glyphs, LEFT, RIGHT, Stroke, UP};
use super::width::{display_width, truncate};

/// The most nodes a diagram may have and still be drawn: past this a
/// terminal-width grid is a wall of boxes nobody reads, and the source is
/// the better page.
pub const MAX_NODES: usize = 40;

/// The most edges, for the same reason, and for time: an edge crossing
/// many layers is a dummy node in each, and a diagram is laid out on the
/// event loop, so its cost has to stay bounded whatever a model wrote. At
/// this many, a gap between two layers holds at most this many segments
/// and the crossing count and track assignment stay small.
pub const MAX_EDGES: usize = 100;

/// The label caps tried, in turn, when a diagram does not fit its width.
const CAPS: &[usize] = &[32, 24, 18, 14, 11, 8, 6];

/// Which way the layers run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dir {
    /// `TD` / `TB`: top to bottom.
    #[default]
    Down,
    /// `BT`: bottom to top.
    Up,
    /// `LR`: left to right.
    Right,
    /// `RL`: right to left.
    Left,
}

impl Dir {
    pub fn parse(s: &str) -> Option<Dir> {
        match s.trim().to_ascii_uppercase().as_str() {
            "TD" | "TB" => Some(Dir::Down),
            "BT" => Some(Dir::Up),
            "LR" => Some(Dir::Right),
            "RL" => Some(Dir::Left),
            _ => None,
        }
    }

    /// Layers run left to right (or right to left).
    fn across(self) -> bool {
        matches!(self, Dir::Right | Dir::Left)
    }

    /// Edges are laid out turned around.
    fn reversed(self) -> bool {
        matches!(self, Dir::Up | Dir::Left)
    }

    /// The same flow with the axes swapped: the fallback when a layout is
    /// too wide as written.
    fn turned(self) -> Dir {
        match self {
            Dir::Down => Dir::Right,
            Dir::Right => Dir::Down,
            Dir::Up => Dir::Left,
            Dir::Left => Dir::Up,
        }
    }
}

/// One row inside a node's box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Text(String),
    /// A rule across the box: a class's fields from its methods.
    Rule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shape {
    #[default]
    Rect,
    /// `(( ))`: a rounded box, since a terminal has no circles.
    Round,
    /// `{ }`: a box with `<` and `>` for sides.
    Diamond,
    /// A state diagram's `[*]` as a source: `●`.
    Start,
    /// …and as a sink: `◉`.
    End,
    /// A state diagram's fork or join: a thick bar.
    Bar,
}

/// What sits at one end of an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mark {
    #[default]
    None,
    Arrow,
    /// Inheritance and realisation: a hollow triangle.
    Triangle,
    /// Composition: a filled diamond.
    Diamond,
    /// Aggregation: a hollow diamond.
    Hollow,
    Cross,
    Circle,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    pub rows: Vec<Row>,
    pub shape: Shape,
    /// The innermost subgraph it is drawn in.
    pub group: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub label: Option<String>,
    pub stroke: Stroke,
    pub from_mark: Mark,
    pub to_mark: Mark,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub title: String,
    pub parent: Option<usize>,
}

/// A diagram as boxes, edges and frames, ready to lay out.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub dir: Dir,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub groups: Vec<Group>,
    index: HashMap<String, usize>,
}

impl Graph {
    pub fn new(dir: Dir) -> Self {
        Self {
            dir,
            ..Self::default()
        }
    }

    /// The node called `id`, made (a box labelled with its id) the first
    /// time it is named.
    pub fn node(&mut self, id: &str) -> usize {
        if let Some(&i) = self.index.get(id) {
            return i;
        }
        let i = self.nodes.len();
        self.nodes.push(Node {
            id: id.to_string(),
            rows: vec![Row::Text(id.to_string())],
            shape: Shape::Rect,
            group: None,
        });
        self.index.insert(id.to_string(), i);
        i
    }

    pub fn find(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    pub fn add_group(&mut self, title: &str, parent: Option<usize>) -> usize {
        self.groups.push(Group {
            title: title.to_string(),
            parent,
        });
        self.groups.len() - 1
    }

    /// Put `node` in `group`, unless it already sits in a subgraph that is
    /// not one of `group`'s ancestors: the first subgraph that names a
    /// node keeps it, a subgraph nested inside that one may take it deeper.
    pub fn assign(&mut self, node: usize, group: Option<usize>) {
        let Some(g) = group else { return };
        match self.nodes[node].group {
            None => self.nodes[node].group = Some(g),
            Some(cur) if cur != g && self.within(Some(g), cur) => self.nodes[node].group = Some(g),
            _ => {}
        }
    }

    pub fn edge(&mut self, from: usize, to: usize, label: Option<String>, stroke: Stroke) {
        self.edges.push(Edge {
            from,
            to,
            label: label.filter(|l| !l.trim().is_empty()),
            stroke,
            from_mark: Mark::None,
            to_mark: Mark::Arrow,
        });
    }

    /// Is cluster `c` (a node's group) `anc` or inside it?
    fn within(&self, mut c: Option<usize>, anc: usize) -> bool {
        while let Some(g) = c {
            if g == anc {
                return true;
            }
            c = self.groups[g].parent;
        }
        false
    }

    fn depth(&self, g: usize) -> usize {
        let mut d = 0;
        let mut c = self.groups[g].parent;
        while let Some(p) = c {
            d += 1;
            c = self.groups[p].parent;
        }
        d
    }

    /// The groups from the outermost down to `c`.
    fn chain(&self, mut c: Option<usize>) -> Vec<usize> {
        let mut out = Vec::new();
        while let Some(g) = c {
            out.push(g);
            c = self.groups[g].parent;
        }
        out.reverse();
        out
    }

    fn common(&self, a: Option<usize>, b: Option<usize>) -> Option<usize> {
        let (ca, cb) = (self.chain(a), self.chain(b));
        ca.iter()
            .zip(cb.iter())
            .take_while(|(x, y)| x == y)
            .last()
            .map(|(x, _)| *x)
    }

    /// The widest label anywhere in the graph.
    fn longest_label(&self) -> usize {
        let rows = self
            .nodes
            .iter()
            .flat_map(|n| n.rows.iter())
            .map(|r| match r {
                Row::Text(t) => display_width(t),
                Row::Rule => 0,
            });
        let edges = self
            .edges
            .iter()
            .filter_map(|e| e.label.as_deref())
            .map(display_width);
        let titles = self.groups.iter().map(|g| display_width(&g.title));
        rows.chain(edges).chain(titles).max().unwrap_or(0)
    }
}

/// Draw `graph` in at most `max_width` columns: as written, then with the
/// labels cut shorter and shorter, then with the axes turned (an `LR` chain
/// too long for the width often fits as `TD`), and failing all that, why
/// not.
pub fn draw(graph: &Graph, glyphs: &Glyphs, max_width: usize) -> Result<Vec<String>, String> {
    if graph.nodes.is_empty() {
        return Err("the diagram has no nodes".into());
    }
    if graph.nodes.len() > MAX_NODES {
        return Err(format!(
            "{} nodes: at most {MAX_NODES} are drawn",
            graph.nodes.len()
        ));
    }
    if graph.edges.len() > MAX_EDGES {
        return Err(format!(
            "{} edges: at most {MAX_EDGES} are drawn",
            graph.edges.len()
        ));
    }
    let longest = graph.longest_label();
    let mut caps = vec![None];
    caps.extend(CAPS.iter().filter(|c| **c < longest).map(|c| Some(*c)));
    let mut narrowest = usize::MAX;
    for dir in [graph.dir, graph.dir.turned()] {
        for cap in &caps {
            let lines = layout(graph, dir, *cap, glyphs);
            let width = lines.iter().map(|l| display_width(l)).max().unwrap_or(0);
            if width <= max_width {
                return Ok(lines);
            }
            narrowest = narrowest.min(width);
        }
    }
    Err(format!(
        "needs {narrowest} columns even with its labels cut short; {max_width} available"
    ))
}

/// A node of the layout: a real node, a dummy an edge passes through, or a
/// filler that holds a subgraph's place in a layer it has nothing in.
#[derive(Debug, Clone)]
struct Vertex {
    real: Option<usize>,
    layer: usize,
    /// Size across the layers and along them.
    cross: usize,
    along: usize,
    cluster: Option<usize>,
    filler: bool,
    /// The layout edge a dummy belongs to.
    ledge: Option<usize>,
}

/// An edge as laid out: from `u` in an earlier layer to `v` in a later one.
#[derive(Debug, Clone)]
struct LaidEdge {
    u: usize,
    v: usize,
    edge: usize,
    /// Turned around, to break a cycle or for `BT`/`RL`.
    flipped: bool,
}

/// One hop of a layout edge, between neighbouring layers.
#[derive(Debug, Clone)]
struct Segment {
    a: usize,
    b: usize,
    le: usize,
    first: bool,
    last: bool,
}

/// The segments of one gap that leave the same node: they share the node's
/// port and one horizontal run.
#[derive(Debug, Clone)]
struct Fan {
    out: usize,
    ins: Vec<usize>,
    segs: Vec<usize>,
    lo: usize,
    hi: usize,
    track: Option<usize>,
}

/// A gap between two layers, and what is stacked in it along the flow:
/// from-end marks, the bottoms of frames that end above, the tracks, the
/// labels, the tops of frames that start below, the arrowheads.
#[derive(Debug, Clone, Default)]
struct Gap {
    fm: usize,
    ends: Vec<usize>,
    tracks: usize,
    zone: usize,
    starts: Vec<usize>,
    arrow: usize,
    extra: usize,
}

impl Gap {
    fn lines(&self) -> usize {
        self.fm
            + self.ends.len()
            + self.tracks
            + self.zone
            + self.extra
            + self.starts.len()
            + self.arrow
    }
    fn ends_off(&self) -> usize {
        self.fm
    }
    fn track_off(&self) -> usize {
        self.fm + self.ends.len()
    }
    fn zone_off(&self) -> usize {
        self.track_off() + self.tracks
    }
    fn starts_off(&self) -> usize {
        self.zone_off() + self.zone + self.extra
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Node(usize),
    Group(usize),
}

fn cmp_f(a: f64, b: f64) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

/// One layout of `g` in direction `dir`, every label cut to `cap` columns.
fn layout(g: &Graph, dir: Dir, cap: Option<usize>, glyphs: &Glyphs) -> Vec<String> {
    let across = dir.across();
    let cut = |s: &str| -> String {
        let t = glyphs.text(s.to_string());
        glyphs.text(match cap {
            Some(c) => truncate(&t, c),
            None => t,
        })
    };
    let n = g.nodes.len();

    // ---- the rows each box shows ----
    let mut rows: Vec<Vec<Row>> = g
        .nodes
        .iter()
        .map(|node| {
            let mut r: Vec<Row> = node
                .rows
                .iter()
                .map(|r| match r {
                    Row::Text(t) => Row::Text(cut(t)),
                    Row::Rule => Row::Rule,
                })
                .collect();
            if r.is_empty() {
                r.push(Row::Text(String::new()));
            }
            r
        })
        .collect();
    for e in &g.edges {
        if e.from == e.to
            && matches!(
                g.nodes[e.from].shape,
                Shape::Rect | Shape::Round | Shape::Diamond
            )
        {
            let text = match &e.label {
                Some(l) => format!("{} {}", glyphs.self_loop(), cut(l)),
                None => glyphs.self_loop().to_string(),
            };
            rows[e.from].push(Row::Text(text));
        }
    }

    // ---- cycles, direction ----
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, e) in g.edges.iter().enumerate() {
        if e.from != e.to {
            adj[e.from].push(i);
        }
    }
    let mut post = vec![0usize; n];
    let mut state = vec![0u8; n];
    let mut counter = 0;
    // The walk starts at the sources, so the edge a cycle loses is the one
    // that closes it, not one on the way in: `[*] → Draft → … → Draft`
    // keeps `Draft` near the top.
    let mut has_in = vec![false; n];
    for e in &g.edges {
        if e.from != e.to {
            has_in[e.to] = true;
        }
    }
    let roots: Vec<usize> = (0..n)
        .filter(|&v| !has_in[v])
        .chain((0..n).filter(|&v| has_in[v]))
        .collect();
    for s in roots {
        if state[s] != 0 {
            continue;
        }
        state[s] = 1;
        let mut stack: Vec<(usize, usize)> = vec![(s, 0)];
        while let Some(top) = stack.last_mut() {
            let (v, i) = *top;
            if i < adj[v].len() {
                top.1 += 1;
                let w = g.edges[adj[v][i]].to;
                if state[w] == 0 {
                    state[w] = 1;
                    stack.push((w, 0));
                }
            } else {
                post[v] = counter;
                counter += 1;
                state[v] = 2;
                stack.pop();
            }
        }
    }
    let mut ledges: Vec<LaidEdge> = Vec::new();
    for (i, e) in g.edges.iter().enumerate() {
        if e.from == e.to {
            continue;
        }
        let mut flipped = post[e.from] < post[e.to];
        let (mut u, mut v) = if flipped {
            (e.to, e.from)
        } else {
            (e.from, e.to)
        };
        if dir.reversed() {
            std::mem::swap(&mut u, &mut v);
            flipped = !flipped;
        }
        ledges.push(LaidEdge {
            u,
            v,
            edge: i,
            flipped,
        });
    }
    let upper_mark = |le: &LaidEdge| {
        let e = &g.edges[le.edge];
        if le.flipped { e.to_mark } else { e.from_mark }
    };
    let lower_mark = |le: &LaidEdge| {
        let e = &g.edges[le.edge];
        if le.flipped { e.from_mark } else { e.to_mark }
    };

    // ---- layering ----
    let mut outs: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for (i, le) in ledges.iter().enumerate() {
        outs[le.u].push(i);
        indeg[le.v] += 1;
    }
    let mut layer = vec![0usize; n];
    let mut deg = indeg.clone();
    let mut queue: Vec<usize> = (0..n).filter(|&v| deg[v] == 0).collect();
    let mut qi = 0;
    while qi < queue.len() {
        let u = queue[qi];
        qi += 1;
        for &li in &outs[u] {
            let v = ledges[li].v;
            layer[v] = layer[v].max(layer[u] + 1);
            deg[v] -= 1;
            if deg[v] == 0 {
                queue.push(v);
            }
        }
    }
    for u in 0..n {
        if indeg[u] == 0 && !outs[u].is_empty() {
            let nearest = outs[u]
                .iter()
                .map(|&li| layer[ledges[li].v])
                .min()
                .unwrap_or(1);
            layer[u] = nearest.saturating_sub(1);
        }
    }
    let low = layer.iter().copied().min().unwrap_or(0);
    for l in &mut layer {
        *l -= low;
    }

    // ---- layout nodes: real, dummy, filler ----
    let mut in_count = vec![0usize; n];
    for le in &ledges {
        in_count[le.v] += 1;
    }
    let mut vs: Vec<Vertex> = (0..n)
        .map(|i| {
            let node = &g.nodes[i];
            let tw = rows[i]
                .iter()
                .map(|r| match r {
                    Row::Text(t) => display_width(t),
                    Row::Rule => 0,
                })
                .max()
                .unwrap_or(0);
            let (mut w, mut h) = match node.shape {
                Shape::Rect | Shape::Round | Shape::Diamond => (tw + 4, rows[i].len() + 2),
                Shape::Start | Shape::End => (1, 1),
                Shape::Bar => {
                    if across {
                        (1, 3)
                    } else {
                        (7, 1)
                    }
                }
            };
            if matches!(node.shape, Shape::Rect | Shape::Round | Shape::Diamond) {
                if across {
                    h = h.max(in_count[i] + 2);
                } else {
                    w = w.max(2 * in_count[i] + 1);
                }
            }
            let (cross, along) = if across { (h, w) } else { (w, h) };
            Vertex {
                real: Some(i),
                layer: layer[i],
                cross,
                along,
                cluster: node.group,
                filler: false,
                ledge: None,
            }
        })
        .collect();
    let mut chains: Vec<Vec<usize>> = Vec::with_capacity(ledges.len());
    for (li, le) in ledges.iter().enumerate() {
        let mut path = vec![le.u];
        let cluster = g.common(g.nodes[le.u].group, g.nodes[le.v].group);
        for l in layer[le.u] + 1..layer[le.v] {
            vs.push(Vertex {
                real: None,
                layer: l,
                cross: 1,
                along: 1,
                cluster,
                filler: false,
                ledge: Some(li),
            });
            path.push(vs.len() - 1);
        }
        path.push(le.v);
        chains.push(path);
    }
    let ng = g.groups.len();
    let span_of = |vs: &[Vertex]| {
        let mut span: Vec<Option<(usize, usize)>> = vec![None; ng];
        for v in vs {
            let mut c = v.cluster;
            while let Some(gid) = c {
                span[gid] = Some(match span[gid] {
                    None => (v.layer, v.layer),
                    Some((lo, hi)) => (lo.min(v.layer), hi.max(v.layer)),
                });
                c = g.groups[gid].parent;
            }
        }
        span
    };
    let mut by_depth: Vec<usize> = (0..ng).collect();
    by_depth.sort_by_key(|&gid| std::cmp::Reverse(g.depth(gid)));
    for &gid in &by_depth {
        let span = span_of(&vs);
        let Some((lo, hi)) = span[gid] else { continue };
        for l in lo..=hi {
            let present = vs.iter().any(|v| v.layer == l && g.within(v.cluster, gid));
            if !present {
                vs.push(Vertex {
                    real: None,
                    layer: l,
                    cross: 1,
                    along: 0,
                    cluster: Some(gid),
                    filler: true,
                    ledge: None,
                });
            }
        }
    }
    let span = span_of(&vs);
    let nv = vs.len();
    let nl = vs.iter().map(|v| v.layer).max().unwrap_or(0) + 1;
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); nl];
    for (i, v) in vs.iter().enumerate() {
        layers[v.layer].push(i);
    }
    let mut up: Vec<Vec<usize>> = vec![Vec::new(); nv];
    let mut down: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for path in &chains {
        for w in path.windows(2) {
            down[w[0]].push(w[1]);
            up[w[1]].push(w[0]);
        }
    }

    // ---- ordering ----
    let mut pos = vec![0usize; nv];
    let set_pos = |layers: &[Vec<usize>], pos: &mut Vec<usize>| {
        for layer in layers {
            for (i, &v) in layer.iter().enumerate() {
                pos[v] = i;
            }
        }
    };
    set_pos(&layers, &mut pos);
    let norm = |p: usize, len: usize| p as f64 / (len.max(2) - 1) as f64;
    let group_keys = |layers: &[Vec<usize>], pos: &[usize]| {
        let mut sum = vec![0.0f64; ng];
        let mut cnt = vec![0usize; ng];
        for layer in layers {
            for &v in layer {
                let mut c = vs[v].cluster;
                while let Some(gid) = c {
                    sum[gid] += norm(pos[v], layer.len());
                    cnt[gid] += 1;
                    c = g.groups[gid].parent;
                }
            }
        }
        (0..ng)
            .map(|i| {
                if cnt[i] == 0 {
                    0.0
                } else {
                    sum[i] / cnt[i] as f64
                }
            })
            .collect::<Vec<f64>>()
    };
    let crossings = |layers: &[Vec<usize>], pos: &[usize]| {
        let mut total = 0usize;
        for layer in layers.iter().take(nl.saturating_sub(1)) {
            let pairs: Vec<(usize, usize)> = layer
                .iter()
                .flat_map(|&a| down[a].iter().map(move |&b| (a, b)))
                .map(|(a, b)| (pos[a], pos[b]))
                .collect();
            for i in 0..pairs.len() {
                for j in i + 1..pairs.len() {
                    let (a1, b1) = pairs[i];
                    let (a2, b2) = pairs[j];
                    if (a1 < a2 && b1 > b2) || (a1 > a2 && b1 < b2) {
                        total += 1;
                    }
                }
            }
        }
        total
    };
    let chains_of: Vec<Vec<usize>> = vs.iter().map(|v| g.chain(v.cluster)).collect();
    let mut best = (crossings(&layers, &pos), layers.clone());
    for sweep in 0..4 {
        let downward = sweep % 2 == 0;
        let order: Vec<usize> = if downward {
            (1..nl).collect()
        } else {
            (0..nl.saturating_sub(1)).rev().collect()
        };
        let gkeys = group_keys(&layers, &pos);
        for l in order {
            let nb_len = if downward {
                layers[l - 1].len()
            } else {
                layers[l + 1].len()
            };
            let own_len = layers[l].len();
            let keyed: Vec<(Vec<(f64, usize)>, usize)> = layers[l]
                .iter()
                .map(|&v| {
                    let nb = if downward { &up[v] } else { &down[v] };
                    let bary = if nb.is_empty() {
                        norm(pos[v], own_len)
                    } else {
                        nb.iter().map(|&w| norm(pos[w], nb_len)).sum::<f64>() / nb.len() as f64
                    };
                    let mut key: Vec<(f64, usize)> = chains_of[v]
                        .iter()
                        .map(|&gid| (gkeys[gid], nv + gid))
                        .collect();
                    key.push((bary, v));
                    (key, v)
                })
                .collect();
            let mut keyed = keyed;
            keyed.sort_by(|(ka, _), (kb, _)| {
                for (x, y) in ka.iter().zip(kb.iter()) {
                    let o = cmp_f(x.0, y.0).then(x.1.cmp(&y.1));
                    if o != std::cmp::Ordering::Equal {
                        return o;
                    }
                }
                ka.len().cmp(&kb.len())
            });
            layers[l] = keyed.into_iter().map(|(_, v)| v).collect();
            for (i, &v) in layers[l].iter().enumerate() {
                pos[v] = i;
            }
        }
        let c = crossings(&layers, &pos);
        if c < best.0 {
            best = (c, layers.clone());
        }
    }
    layers = best.1;
    set_pos(&layers, &mut pos);
    let gkeys = group_keys(&layers, &pos);

    // ---- placement across the layers ----
    let gap = if across { 1 } else { 2 };
    let inner = if across { 1 } else { 2 };
    let mut rel = vec![0usize; nv];
    let mut crel = vec![0usize; ng];
    let mut gwidth = vec![0usize; ng];
    let mut gorigin = vec![0usize; ng];
    let child_of = |c: Option<usize>, parent: Option<usize>| -> Option<usize> {
        let mut c = c;
        while let Some(gid) = c {
            if g.groups[gid].parent == parent {
                return Some(gid);
            }
            c = g.groups[gid].parent;
        }
        None
    };
    let in_cluster = |c: Option<usize>, cluster: Option<usize>| match cluster {
        None => true,
        Some(gid) => g.within(c, gid),
    };
    let mut clusters: Vec<Option<usize>> = by_depth.iter().map(|&gid| Some(gid)).collect();
    clusters.push(None);
    let mut root_width = 0;
    for cluster in clusters {
        let (lo, hi) = match cluster {
            Some(gid) => match span[gid] {
                Some(s) => s,
                None => continue,
            },
            None => (0, nl - 1),
        };
        let mut items: Vec<Vec<Item>> = Vec::new();
        for layer in &layers[lo..=hi] {
            let mut seq: Vec<Item> = Vec::new();
            for &v in layer {
                if !in_cluster(vs[v].cluster, cluster) {
                    continue;
                }
                let item = if vs[v].cluster == cluster {
                    Item::Node(v)
                } else {
                    match child_of(vs[v].cluster, cluster) {
                        Some(c) => Item::Group(c),
                        None => continue,
                    }
                };
                if seq.last() != Some(&item) || matches!(item, Item::Node(_)) {
                    seq.push(item);
                }
            }
            items.push(seq);
        }
        let mut children: Vec<usize> = (0..ng)
            .filter(|&c| g.groups[c].parent == cluster && span[c].is_some())
            .collect();
        children.sort_by(|a, b| cmp_f(gkeys[*a], gkeys[*b]).then(a.cmp(b)));
        let mut cur = vec![0usize; hi - lo + 1];
        let mut idx = vec![0usize; hi - lo + 1];
        let mut done: Vec<bool> = vec![false; ng];
        let place = |k: usize,
                     cur: &mut Vec<usize>,
                     idx: &mut Vec<usize>,
                     rel: &mut Vec<usize>,
                     until: Option<usize>,
                     done: &[bool]| {
            while idx[k] < items[k].len() {
                match items[k][idx[k]] {
                    Item::Node(v) => {
                        rel[v] = cur[k];
                        cur[k] += vs[v].cross + gap;
                        idx[k] += 1;
                    }
                    Item::Group(c) => {
                        if Some(c) == until || !done[c] {
                            break;
                        }
                        idx[k] += 1;
                    }
                }
            }
        };
        for &c in &children {
            let (clo, chi) = span[c].unwrap_or((lo, lo));
            for l in clo..=chi {
                place(l - lo, &mut cur, &mut idx, &mut rel, Some(c), &done);
            }
            let x = (clo..=chi).map(|l| cur[l - lo]).max().unwrap_or(0);
            crel[c] = x;
            for l in clo..=chi {
                let k = l - lo;
                cur[k] = x + gwidth[c] + gap;
                if idx[k] < items[k].len() && items[k][idx[k]] == Item::Group(c) {
                    idx[k] += 1;
                }
            }
            done[c] = true;
        }
        for k in 0..cur.len() {
            place(k, &mut cur, &mut idx, &mut rel, None, &done);
        }
        let content = cur.iter().map(|c| c.saturating_sub(gap)).max().unwrap_or(0);
        match cluster {
            Some(gid) => {
                let title = if across {
                    0
                } else {
                    display_width(&cut(&g.groups[gid].title)) + 6
                };
                let w = (content + 2 * inner).max(title);
                gwidth[gid] = w;
                gorigin[gid] = inner + (w - content - 2 * inner) / 2;
            }
            None => root_width = content,
        }
    }
    let mut base = vec![0usize; ng];
    let mut top_down: Vec<usize> = (0..ng).filter(|&c| span[c].is_some()).collect();
    top_down.sort_by_key(|&c| g.depth(c));
    for &c in &top_down {
        base[c] = match g.groups[c].parent {
            Some(p) => base[p] + gorigin[p] + crel[c],
            None => crel[c],
        };
    }
    let content_base = |c: Option<usize>, base: &[usize]| match c {
        Some(gid) => base[gid] + gorigin[gid],
        None => 0,
    };
    let mut abs: Vec<usize> = (0..nv)
        .map(|v| content_base(vs[v].cluster, &base) + rel[v])
        .collect();

    // ---- straighten: nudge each node toward its neighbours ----
    let frames: Vec<usize> = top_down.clone();
    for pass in 0..4 {
        let downward = pass % 2 == 0;
        let order: Vec<usize> = if downward {
            (1..nl).collect()
        } else {
            (0..nl.saturating_sub(1)).rev().collect()
        };
        for l in order {
            for i in 0..layers[l].len() {
                let v = layers[l][i];
                if vs[v].filler {
                    continue;
                }
                let nb = if downward { &up[v] } else { &down[v] };
                if nb.is_empty() {
                    continue;
                }
                let size = vs[v].cross as isize;
                let centre = nb
                    .iter()
                    .map(|&w| abs[w] as f64 + (vs[w].cross / 2) as f64)
                    .sum::<f64>()
                    / nb.len() as f64;
                let want = (centre.round() as isize - (vs[v].cross / 2) as isize).max(0);
                let mut lo = 0isize;
                let mut hi = root_width as isize - size;
                if i > 0 {
                    let p = layers[l][i - 1];
                    lo = lo.max((abs[p] + vs[p].cross + gap) as isize);
                }
                if i + 1 < layers[l].len() {
                    let q = layers[l][i + 1];
                    hi = hi.min(abs[q] as isize - gap as isize - size);
                }
                for &gid in &frames {
                    let Some((flo, fhi)) = span[gid] else {
                        continue;
                    };
                    if l < flo || l > fhi {
                        continue;
                    }
                    let f0 = base[gid] as isize;
                    let f1 = (base[gid] + gwidth[gid]) as isize - 1;
                    if g.within(vs[v].cluster, gid) {
                        lo = lo.max(f0 + inner as isize);
                        hi = hi.min(f1 + 1 - inner as isize - size);
                    } else if f1 < abs[v] as isize {
                        lo = lo.max(f1 + 1 + gap as isize);
                    } else if f0 > abs[v] as isize {
                        hi = hi.min(f0 - gap as isize - size);
                    }
                }
                if lo <= hi {
                    abs[v] = want.clamp(lo, hi) as usize;
                }
            }
        }
    }

    // ---- ports and fans ----
    let centre = |v: usize, abs: &[usize]| abs[v] + vs[v].cross / 2;
    let mut segs: Vec<Segment> = Vec::new();
    for (li, path) in chains.iter().enumerate() {
        for k in 0..path.len() - 1 {
            segs.push(Segment {
                a: path[k],
                b: path[k + 1],
                le: li,
                first: k == 0,
                last: k + 2 == path.len(),
            });
        }
    }
    let mut in_port = vec![0usize; segs.len()];
    let mut into: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for (si, s) in segs.iter().enumerate() {
        into[s.b].push(si);
    }
    for (b, list) in into.iter_mut().enumerate() {
        if list.is_empty() {
            continue;
        }
        list.sort_by_key(|&si| (centre(segs[si].a, &abs), si));
        let k = list.len();
        for (i, &si) in list.iter().enumerate() {
            in_port[si] = if vs[b].real.is_none() || vs[b].cross < 3 {
                centre(b, &abs)
            } else {
                let room = vs[b].cross - 2;
                abs[b] + 1 + ((2 * i + 1) * room) / (2 * k)
            };
        }
    }
    // Out of a node, every edge shares the middle of its far side (a fan
    // reads as a tree) unless one of them ends in a mark on this side (a
    // cycle's arrow coming back, a class's `◆`): then each gets a port of
    // its own, or the mark would sit on the stem the others share.
    let mut out_port: Vec<usize> = segs.iter().map(|s| centre(s.a, &abs)).collect();
    let mut leaving: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for (si, s) in segs.iter().enumerate() {
        leaving[s.a].push(si);
    }
    for (a, list) in leaving.iter_mut().enumerate() {
        let marked = list
            .iter()
            .any(|&si| segs[si].first && upper_mark(&ledges[segs[si].le]) != Mark::None);
        if list.len() < 2 || !marked || vs[a].real.is_none() || vs[a].cross < 3 {
            continue;
        }
        list.sort_by_key(|&si| (in_port[si], si));
        let k = list.len();
        let room = vs[a].cross - 2;
        for (i, &si) in list.iter().enumerate() {
            out_port[si] = abs[a] + 1 + ((2 * i + 1) * room) / (2 * k);
        }
    }

    // ---- gaps: tracks, labels, frame borders ----
    let mut gaps: Vec<Gap> = vec![Gap::default(); nl + 1];
    let mut fans_in: Vec<Vec<Fan>> = vec![Vec::new(); nl + 1];
    let mut labels: Vec<(usize, usize, usize, String)> = Vec::new(); // (seg, zone row, start, text)
    let mut frame_ids: Vec<usize> = top_down.clone();
    frame_ids.sort_by_key(|&c| g.depth(c));
    for &gid in &frame_ids {
        let (lo, hi) = span[gid].unwrap_or((0, 0));
        gaps[lo].starts.push(gid);
        gaps[hi + 1].ends.push(gid);
    }
    for gp in &mut gaps {
        gp.ends.reverse();
    }
    for gi in 1..nl {
        let mut fans: Vec<Fan> = Vec::new();
        let mut fan_of: HashMap<(usize, usize), usize> = HashMap::new();
        for (si, s) in segs.iter().enumerate() {
            if vs[s.a].layer + 1 != gi {
                continue;
            }
            let fi = *fan_of.entry((s.a, out_port[si])).or_insert_with(|| {
                fans.push(Fan {
                    out: out_port[si],
                    ins: Vec::new(),
                    segs: Vec::new(),
                    lo: out_port[si],
                    hi: out_port[si],
                    track: None,
                });
                fans.len() - 1
            });
            let f = &mut fans[fi];
            f.ins.push(in_port[si]);
            f.segs.push(si);
            f.lo = f.lo.min(in_port[si]);
            f.hi = f.hi.max(in_port[si]);
        }
        // Corners never land on another edge's line: a fan whose port is
        // where another fan enters runs its track above that fan's.
        let jog: Vec<usize> = (0..fans.len())
            .filter(|&i| fans[i].lo != fans[i].hi)
            .collect();
        let mut before: Vec<Vec<usize>> = vec![Vec::new(); fans.len()];
        let mut need = vec![0usize; fans.len()];
        for &a in &jog {
            for &b in &jog {
                if a != b && fans[b].ins.contains(&fans[a].out) {
                    before[a].push(b);
                    need[b] += 1;
                }
            }
        }
        let mut order: Vec<usize> = Vec::new();
        let mut ready: Vec<usize> = jog.iter().copied().filter(|&i| need[i] == 0).collect();
        while !ready.is_empty() {
            ready.sort_by_key(|&i| std::cmp::Reverse((fans[i].lo, i)));
            let a = ready.pop().unwrap_or_default();
            order.push(a);
            for &b in &before[a] {
                need[b] -= 1;
                if need[b] == 0 {
                    ready.push(b);
                }
            }
        }
        for &i in &jog {
            if !order.contains(&i) {
                order.push(i);
            }
        }
        let mut tracks: Vec<Vec<(usize, usize)>> = Vec::new();
        for &fi in &order {
            let min_t = jog
                .iter()
                .filter(|&&a| before[a].contains(&fi))
                .filter_map(|&a| fans[a].track)
                .map(|t| t + 1)
                .max()
                .unwrap_or(0);
            let (lo, hi) = (fans[fi].lo, fans[fi].hi);
            let mut t = min_t;
            loop {
                if t == tracks.len() {
                    tracks.push(Vec::new());
                }
                if tracks[t].iter().all(|&(l, h)| hi + 1 < l || h + 1 < lo) {
                    tracks[t].push((lo, hi));
                    fans[fi].track = Some(t);
                    break;
                }
                t += 1;
            }
        }
        let gp = &mut gaps[gi];
        gp.tracks = tracks.len();
        gp.arrow = 1;
        gp.fm = usize::from(
            fans.iter()
                .flat_map(|f| f.segs.iter())
                .any(|&si| segs[si].first && upper_mark(&ledges[segs[si].le]) != Mark::None),
        );
        // Labels: beside the vertical run into the target (TD), or on the
        // horizontal run (LR).
        let mut labelled: Vec<usize> = fans
            .iter()
            .flat_map(|f| f.segs.iter().copied())
            .filter(|&si| segs[si].first && g.edges[ledges[segs[si].le].edge].label.is_some())
            .collect();
        labelled.sort_by_key(|&si| (in_port[si], si));
        if across {
            let widest = labelled
                .iter()
                .map(|&si| {
                    display_width(&cut(g.edges[ledges[segs[si].le].edge]
                        .label
                        .as_deref()
                        .unwrap_or("")))
                })
                .max();
            if let Some(w) = widest {
                gp.zone = w + 2;
                for &si in &labelled {
                    let text = cut(g.edges[ledges[segs[si].le].edge]
                        .label
                        .as_deref()
                        .unwrap_or(""));
                    labels.push((si, 0, 1, text));
                }
            }
        } else {
            let mut blocked: Vec<usize> = fans.iter().flat_map(|f| f.ins.iter().copied()).collect();
            for &gid in &frame_ids {
                if let Some((lo, hi)) = span[gid]
                    && lo < gi
                    && hi >= gi
                {
                    blocked.push(base[gid]);
                    blocked.push(base[gid] + gwidth[gid] - 1);
                }
            }
            let mut zone_rows: Vec<Vec<(usize, usize)>> = Vec::new();
            for &si in &labelled {
                let text = cut(g.edges[ledges[segs[si].le].edge]
                    .label
                    .as_deref()
                    .unwrap_or(""));
                let tw = display_width(&text).max(1);
                let x = in_port[si];
                let clear = |s: usize, e: usize| !blocked.iter().any(|&b| b >= s && b <= e);
                let (rs, re) = (x + 2, x + 1 + tw);
                let left = (x >= tw + 2).then(|| (x - 1 - tw, x - 2));
                // Beside the line on the side no other line runs through —
                // the right by preference; when both are crossed, the
                // right anyway (the label then hides a cell of that line).
                let (s, e) = if clear(x + 1, re + 1) {
                    (rs, re)
                } else {
                    match left {
                        Some((ls, le)) if clear(ls.saturating_sub(1), x - 1) => (ls, le),
                        _ => (rs, re),
                    }
                };
                let r = zone_rows
                    .iter()
                    .position(|row| row.iter().all(|&(l, h)| e + 1 < l || h + 1 < s))
                    .unwrap_or(zone_rows.len());
                if r == zone_rows.len() {
                    zone_rows.push(Vec::new());
                }
                zone_rows[r].push((s, e));
                labels.push((si, r, s, text));
            }
            gp.zone = zone_rows.len();
        }
        let min: usize = if across { 3 } else { 2 };
        gp.extra = min.saturating_sub(gp.lines());
        fans_in[gi] = fans;
    }

    // ---- positions along the layers ----
    let thick: Vec<usize> = layers
        .iter()
        .map(|l| l.iter().map(|&v| vs[v].along).max().unwrap_or(1).max(1))
        .collect();
    let mut gstart = vec![0usize; nl + 1];
    let mut lstart = vec![0usize; nl];
    let mut a = 0;
    for gi in 0..=nl {
        gstart[gi] = a;
        a += gaps[gi].lines();
        if gi < nl {
            lstart[gi] = a;
            a += thick[gi];
        }
    }
    let nstart = |v: usize| {
        let l = vs[v].layer;
        if vs[v].real.is_some() {
            lstart[l] + (thick[l] - vs[v].along) / 2
        } else {
            lstart[l]
        }
    };
    let nend = |v: usize| {
        let l = vs[v].layer;
        if vs[v].real.is_some() {
            nstart(v) + vs[v].along - 1
        } else {
            lstart[l] + thick[l] - 1
        }
    };

    // ---- drawing ----
    let mut cv = Canvas::new();
    let to_screen = |al: usize, cr: usize| if across { (al, cr) } else { (cr, al) };
    let aline = |cv: &mut Canvas, cr: usize, a0: usize, a1: usize, s: Stroke| {
        if across {
            cv.hline(cr, a0, a1, s)
        } else {
            cv.vline(cr, a0, a1, s)
        }
    };
    let cline = |cv: &mut Canvas, al: usize, c0: usize, c1: usize, s: Stroke| {
        if across {
            cv.vline(al, c0, c1, s)
        } else {
            cv.hline(al, c0, c1, s)
        }
    };
    let (fwd, back) = if across { (RIGHT, LEFT) } else { (DOWN, UP) };
    let mark_glyph = |m: Mark, side: u8| match m {
        Mark::None => None,
        Mark::Arrow => Some(glyphs.arrow(side)),
        Mark::Triangle => Some(if glyphs.ascii {
            glyphs.arrow(side)
        } else {
            glyphs.open_arrow(side)
        }),
        Mark::Diamond => Some(glyphs.diamond()),
        Mark::Hollow => Some(glyphs.hollow_diamond()),
        Mark::Cross => Some(glyphs.cross()),
        Mark::Circle => Some('o'),
    };

    // Frames.
    let mut titles: Vec<(usize, usize, usize, String)> = Vec::new();
    for &gid in &frame_ids {
        let Some((lo, hi)) = span[gid] else { continue };
        let top_idx = gaps[lo].starts.iter().position(|&x| x == gid).unwrap_or(0);
        let bot_idx = gaps[hi + 1]
            .ends
            .iter()
            .position(|&x| x == gid)
            .unwrap_or(0);
        let a0 = gstart[lo] + gaps[lo].starts_off() + top_idx;
        let a1 = gstart[hi + 1] + gaps[hi + 1].ends_off() + bot_idx;
        let c0 = base[gid];
        let c1 = base[gid] + gwidth[gid] - 1;
        let (x0, y0) = to_screen(a0, c0);
        let (x1, y1) = to_screen(a1, c1);
        cv.rect(x0, y0, x1 - x0 + 1, y1 - y0 + 1, Stroke::Solid, false);
        let room = (x1 - x0 + 1).saturating_sub(6);
        let title = cut(&g.groups[gid].title);
        let title = if display_width(&title) > room {
            glyphs.text(truncate(&title, room))
        } else {
            title
        };
        if !title.is_empty() {
            titles.push((x0 + 2, y0, room, format!(" {title} ")));
        }
    }

    // Edges.
    for gi in 1..nl {
        for fan in &fans_in[gi] {
            for &si in &fan.segs {
                let s = &segs[si];
                let le = &ledges[s.le];
                let stroke = g.edges[le.edge].stroke;
                let um = if s.first { upper_mark(le) } else { Mark::None };
                let lm = if s.last { lower_mark(le) } else { Mark::None };
                let start = if vs[s.a].real.is_some() && um != Mark::None {
                    nend(s.a) + 1
                } else {
                    nend(s.a)
                };
                let stop = if vs[s.b].real.is_some() && lm != Mark::None {
                    nstart(s.b) - 1
                } else {
                    nstart(s.b)
                };
                let (out, inp) = (out_port[si], in_port[si]);
                match fan.track {
                    None => aline(&mut cv, out, start, stop, stroke),
                    Some(t) => {
                        let tl = gstart[gi] + gaps[gi].track_off() + t;
                        aline(&mut cv, out, start, tl, stroke);
                        cline(&mut cv, tl, out, inp, stroke);
                        aline(&mut cv, inp, tl, stop, stroke);
                    }
                }
            }
        }
    }
    for (v, node) in vs.iter().enumerate() {
        if let (None, Some(li)) = (node.real, node.ledge) {
            let stroke = g.edges[ledges[li].edge].stroke;
            let l = node.layer;
            aline(&mut cv, abs[v], lstart[l], lstart[l] + thick[l] - 1, stroke);
        }
    }

    // Nodes.
    for (v, node) in vs.iter().enumerate() {
        let Some(i) = node.real else { continue };
        let (x, y) = to_screen(nstart(v), abs[v]);
        let (w, h) = if across {
            (node.along, node.cross)
        } else {
            (node.cross, node.along)
        };
        let shape = g.nodes[i].shape;
        match shape {
            Shape::Start => {
                cv.put(x, y, glyphs.start());
            }
            Shape::End => {
                cv.put(x, y, glyphs.end());
            }
            Shape::Bar => {
                if w >= h {
                    cv.hline(y, x, x + w - 1, Stroke::Thick);
                } else {
                    cv.vline(x, y, y + h - 1, Stroke::Thick);
                }
            }
            Shape::Rect | Shape::Round | Shape::Diamond => {
                cv.clear(x + 1, y + 1, w - 2, h - 2);
                cv.rect(x, y, w, h, Stroke::Solid, shape == Shape::Round);
                let ruled = rows[i].contains(&Row::Rule);
                let inner_w = w - 4;
                let top = y + 1 + (h - 2 - rows[i].len()) / 2;
                let mut past_rule = false;
                for (r, row) in rows[i].iter().enumerate() {
                    match row {
                        Row::Rule => {
                            past_rule = true;
                            cv.untext(x + 1, top + r, w - 2);
                            cv.hline(top + r, x, x + w - 1, Stroke::Solid);
                        }
                        Row::Text(t) => {
                            let tw = display_width(t);
                            let off = if ruled && (past_rule || r > 0) {
                                0
                            } else {
                                (inner_w.saturating_sub(tw)) / 2
                            };
                            cv.text(x + 2 + off, top + r, t);
                        }
                    }
                }
                if shape == Shape::Diamond {
                    let mid = y + h / 2;
                    cv.put(x, mid, '<');
                    cv.put(x + w - 1, mid, '>');
                }
            }
        }
    }

    // Labels, frame titles and marks go on top.
    for (si, r, start, text) in &labels {
        let s = &segs[*si];
        let gi = vs[s.b].layer;
        let zone = gstart[gi] + gaps[gi].zone_off();
        if across {
            cv.text(zone + start, in_port[*si], text);
        } else {
            cv.text(*start, zone + r, text);
        }
    }
    for (x, y, _, text) in &titles {
        cv.text(*x, *y, text);
    }
    for fans in fans_in.iter().take(nl).skip(1) {
        for fan in fans {
            for &si in &fan.segs {
                let s = &segs[si];
                let le = &ledges[s.le];
                if s.first
                    && vs[s.a].real.is_some()
                    && let Some(c) = mark_glyph(upper_mark(le), back)
                {
                    let (x, y) = to_screen(nend(s.a) + 1, out_port[si]);
                    cv.put(x, y, c);
                }
                if s.last
                    && vs[s.b].real.is_some()
                    && let Some(c) = mark_glyph(lower_mark(le), fwd)
                {
                    let (x, y) = to_screen(nstart(s.b) - 1, in_port[si]);
                    cv.put(x, y, c);
                }
            }
        }
    }
    cv.lines(glyphs)
}
