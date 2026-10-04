//! A tab's panes, as a tree of splits. Each split cuts its room in two,
//! side by side or one above the other, at a ratio, and each side is a
//! pane or another split, as deep as the user likes. One pane follows the
//! sidebar's selection; each of the others keeps a session of its own on
//! screen.
//!
//! Everything here is pure: laying the panes out in an area, finding the
//! pane beside another on screen, moving the border between two, evening
//! them out. A tab keeps its tree in the tabs' file.

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;

/// The fewest columns a pane is left with: its header's mark and the start
/// of its session's name.
pub const MIN_COLUMNS: u16 = 12;

/// The fewest rows a pane is left with: its header line and two rows of its
/// session's screen.
pub const MIN_ROWS: u16 = 3;

/// What a pane shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pane {
    /// Whichever session the sidebar's selection is on.
    Selection,
    /// The session called this, split off into a pane of its own.
    Session(String),
}

/// Which way a split cuts its room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Way {
    /// Its second side to the right of its first, a rule between them.
    Right,
    /// Its second side below its first, which the second's header lines
    /// set apart.
    Down,
}

/// A way to go from a pane on screen: to the pane there, or to move a
/// border.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// How it's said: `left`, `up`.
    pub fn word(self) -> &'static str {
        match self {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }

    /// The way of the splits whose borders lie across this direction.
    fn way(self) -> Way {
        match self {
            Direction::Left | Direction::Right => Way::Right,
            Direction::Up | Direction::Down => Way::Down,
        }
    }

    /// Whether it goes toward a split's second side.
    fn onward(self) -> bool {
        matches!(self, Direction::Right | Direction::Down)
    }
}

/// The border between a split's two sides, laid out in an area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Border {
    /// Which split, counted in the order [`SplitTree::borders`] lists them.
    pub split: usize,
    pub way: Way,
    /// Where it is: the rule's column between two sides side by side, or
    /// the row of the header lines along the top of a second side below.
    pub line: Rect,
}

/// The panes of a tab: a pane that follows the selection, alone, or a
/// split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SplitTree {
    root: Node,
}

impl Default for SplitTree {
    fn default() -> SplitTree {
        SplitTree {
            root: Node::Pane(Pane::Selection),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum Node {
    Pane(Pane),
    Split(Box<Split>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Split {
    way: Way,
    /// The share of the room its first side has, from 0 to 1.
    ratio: f32,
    first: Node,
    second: Node,
}

/// One side of a split. A list of them, from the top of the tree, is the
/// way down to a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    First,
    Second,
}

impl Side {
    fn other(self) -> Side {
        match self {
            Side::First => Side::Second,
            Side::Second => Side::First,
        }
    }
}

impl SplitTree {
    /// `pane` alone, taking all the room.
    pub fn of(pane: Pane) -> SplitTree {
        SplitTree {
            root: Node::Pane(pane),
        }
    }

    /// `first` and `second` put together, `way`: `first` has `ratio` of the
    /// room, and `second` the rest. The tree may have no selection's pane,
    /// or several, until [`SplitTree::retain`] puts it right.
    pub fn joined(way: Way, ratio: f32, first: SplitTree, second: SplitTree) -> SplitTree {
        SplitTree {
            root: Node::split(way, ratio, first.root, second.root),
        }
    }

    /// `panes` in a line, `way`, each with the same room.
    pub fn in_line(panes: Vec<Pane>, way: Way) -> SplitTree {
        let mut nodes = panes.into_iter().rev().map(Node::Pane);
        let Some(last) = nodes.next() else {
            return SplitTree::default();
        };
        let root = nodes.fold(last, |rest, pane| Node::split(way, 0.5, pane, rest));
        let mut tree = SplitTree { root };
        tree.equalize();
        tree
    }

    /// Every pane, in the order they're drawn: down the tree, first sides
    /// first, which is left to right and top to bottom.
    pub fn panes(&self) -> Vec<&Pane> {
        let mut panes = Vec::new();
        self.root.gather_panes(&mut panes);
        panes
    }

    /// The sessions split off into panes of their own, in the order their
    /// panes are drawn.
    pub fn sessions(&self) -> Vec<&str> {
        let panes = self.panes().into_iter();
        panes
            .filter_map(|pane| match pane {
                Pane::Session(name) => Some(name.as_str()),
                Pane::Selection => None,
            })
            .collect()
    }

    pub fn contains(&self, pane: &Pane) -> bool {
        self.path_of(pane).is_some()
    }

    /// Where each pane goes in `area`, in the order they're drawn. A split
    /// side by side keeps a column between its sides for the rule.
    pub fn layout(&self, area: Rect) -> Vec<(&Pane, Rect)> {
        let mut placed = Vec::new();
        self.root.place(area, &mut placed);
        placed
    }

    /// Where `pane` goes in `area`, if it's one of these.
    pub fn area_of(&self, pane: &Pane, area: Rect) -> Option<Rect> {
        let placed = self.layout(area);
        let found = placed.into_iter().find(|(placed, _)| *placed == pane);
        found.map(|(_, area)| area)
    }

    /// Every split's border in `area`, down the tree, outer splits first.
    pub fn borders(&self, area: Rect) -> Vec<Border> {
        let splits = self.splits_in(area).into_iter().enumerate();
        splits
            .map(|(index, (_, split, room))| {
                let (first, second) = split.cut(room);
                let line = match split.way {
                    Way::Right => Rect::new(first.right(), room.y, room.width.min(1), room.height),
                    Way::Down => Rect::new(room.x, second.y, room.width, second.height.min(1)),
                };
                Border {
                    split: index,
                    way: split.way,
                    line,
                }
            })
            .collect()
    }

    /// Whether `pane`'s room in `area` holds two panes `way`, neither
    /// smaller than the fewest columns and rows a pane is left with.
    pub fn has_room(&self, pane: &Pane, way: Way, area: Rect) -> bool {
        self.area_of(pane, area)
            .is_some_and(|room| along(room, way) >= 2 * least(way) + rule(way))
    }

    /// Splits `at` in two, `way`: it keeps the first side, `ratio` of its
    /// room, and `new` takes the second. Says whether it did: `at` has to
    /// be here and `new` not, since a pane is in a tree once.
    pub fn split(&mut self, at: &Pane, way: Way, ratio: f32, new: Pane) -> bool {
        if self.contains(&new) {
            return false;
        }
        let Some(path) = self.path_of(at) else {
            return false;
        };
        let node = self.node_mut(&path);
        let old = std::mem::replace(node, Node::Pane(Pane::Selection));
        *node = Node::split(way, ratio, old, Node::Pane(new));
        true
    }

    /// Closes `pane`: the other side of its split takes the room. Says
    /// whether it did; the last pane stays.
    pub fn close(&mut self, pane: &Pane) -> bool {
        let Some(path) = self.path_of(pane) else {
            return false;
        };
        let Some((side, above)) = path.split_last() else {
            return false;
        };
        let parent = self.node_mut(above);
        if let Node::Split(split) = std::mem::replace(parent, Node::Pane(Pane::Selection)) {
            *parent = match side {
                Side::First => split.second,
                Side::Second => split.first,
            };
        }
        true
    }

    /// Shows `with` where `pane` is. Says whether it did: `with` mustn't
    /// be here already.
    pub fn replace(&mut self, pane: &Pane, with: Pane) -> bool {
        if pane != &with && self.contains(&with) {
            return false;
        }
        let Some(path) = self.path_of(pane) else {
            return false;
        };
        *self.node_mut(&path) = Node::Pane(with);
        true
    }

    /// Swaps `a` and `b` over, the splits and their ratios left as they
    /// are. Says whether both were here.
    pub fn swap(&mut self, a: &Pane, b: &Pane) -> bool {
        let (Some(at_a), Some(at_b)) = (self.path_of(a), self.path_of(b)) else {
            return false;
        };
        *self.node_mut(&at_a) = Node::Pane(b.clone());
        *self.node_mut(&at_b) = Node::Pane(a.clone());
        true
    }

    /// Closes the panes of the sessions `keep` doesn't keep, asked down
    /// the tree in order. The selection's pane stays: a tree always has
    /// one, and only one, so one is put beside the others if there's none,
    /// and a second is closed. A split's ratio is kept between 0 and 1.
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        let mut selection = false;
        let root = std::mem::replace(&mut self.root, Node::Pane(Pane::Selection));
        let mut wanted = |pane: &Pane| match pane {
            Pane::Selection => !std::mem::replace(&mut selection, true),
            Pane::Session(name) => keep(name),
        };
        let Some(root) = root.kept(&mut wanted) else {
            return;
        };
        self.root = if selection {
            root
        } else {
            Node::split(Way::Right, 0.5, root, Node::Pane(Pane::Selection))
        };
    }

    /// The pane next to `pane` on screen going `toward`, when the panes
    /// are laid out in `area`: of those that way and facing it, the
    /// nearest, then the one that faces it most, then the one most in
    /// line with it.
    pub fn neighbour(&self, pane: &Pane, toward: Direction, area: Rect) -> Option<&Pane> {
        let placed = self.layout(area);
        let (_, from) = *placed.iter().find(|(placed, _)| *placed == pane)?;
        let candidates = placed
            .iter()
            .enumerate()
            .filter_map(|(index, (other, to))| {
                let gap = gap(from, *to, toward)?;
                let facing = facing(from, *to, toward);
                let rank = (gap, Reverse(facing), off_centre(from, *to, toward), index);
                (*other != pane && facing > 0).then_some((rank, *other))
            });
        candidates
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, pane)| pane)
    }

    /// Moves a border of `pane`'s by `cells` columns or rows, `toward`:
    /// the one on that side of it if there is one, which it grows into,
    /// or else the one on its other side, which it shrinks from. Every
    /// pane keeps the fewest columns and rows it's left with. Says whether
    /// a border moved.
    pub fn resize(&mut self, pane: &Pane, toward: Direction, cells: u16, area: Rect) -> bool {
        let Some(path) = self.path_of(pane) else {
            return false;
        };
        let way = toward.way();
        let near = if toward.onward() {
            Side::First
        } else {
            Side::Second
        };
        let depth = (self.nearest_split(&path, way, near))
            .or_else(|| self.nearest_split(&path, way, near.other()));
        let Some(depth) = depth else {
            return false;
        };
        let at = &path[..depth];
        let room = self.area_at(at, area);
        let Node::Split(split) = self.node(at) else {
            return false;
        };
        let now = i32::from(share(along_split(room, way), split.ratio));
        let by = if toward.onward() {
            i32::from(cells)
        } else {
            -i32::from(cells)
        };
        self.put_border(at, now + by, room)
    }

    /// Gives `pane`'s side of the nearest split above it, or the nearest
    /// `way` when that's given, `share` of the split's room, whatever the
    /// panes on either side are left with. Says whether there's such a
    /// split.
    pub fn set_share(&mut self, pane: &Pane, way: Option<Way>, share: f32) -> bool {
        let Some(path) = self.path_of(pane) else {
            return false;
        };
        let depth = (0..path.len())
            .rev()
            .find(|&depth| match self.node(&path[..depth]) {
                Node::Split(split) => way.is_none_or(|way| split.way == way),
                Node::Pane(_) => false,
            });
        let Some(depth) = depth else {
            return false;
        };
        let Node::Split(split) = self.node_mut(&path[..depth]) else {
            return false;
        };
        let share = share.clamp(0.0, 1.0);
        split.ratio = match path[depth] {
            Side::First => share,
            Side::Second => 1.0 - share,
        };
        true
    }

    /// Puts the border of the split [`SplitTree::borders`] counts as
    /// `split` at column or row `to` of the screen, as near as every pane's
    /// fewest columns and rows let it. Says whether it moved.
    pub fn drag(&mut self, split: usize, to: u16, area: Rect) -> bool {
        let found = self.splits_in(area).into_iter().nth(split);
        let Some((path, split, room)) = found else {
            return false;
        };
        let start = match split.way {
            Way::Right => room.x,
            Way::Down => room.y,
        };
        self.put_border(&path, i32::from(to) - i32::from(start), room)
    }

    /// Evens the panes out: those in a line the same way each get the same
    /// room, a split the other way among them counting as one.
    pub fn equalize(&mut self) {
        self.root.equalize();
    }

    /// Folds the tree into one value, from the panes up: `pane` makes one
    /// of each pane, and `split` one of each split, from its way, its ratio
    /// and what its two sides came to.
    pub fn fold<T>(
        &self,
        pane: &mut impl FnMut(&Pane) -> T,
        split: &mut impl FnMut(Way, f32, T, T) -> T,
    ) -> T {
        self.root.fold(pane, split)
    }

    /// The way down to `pane`, if it's here.
    fn path_of(&self, pane: &Pane) -> Option<Vec<Side>> {
        let mut path = Vec::new();
        self.root.find(pane, &mut path).then_some(path)
    }

    fn node(&self, path: &[Side]) -> &Node {
        path.iter().fold(&self.root, |node, side| match node {
            Node::Split(split) => split.side(*side),
            Node::Pane(_) => node,
        })
    }

    fn node_mut(&mut self, path: &[Side]) -> &mut Node {
        let mut node = &mut self.root;
        for side in path {
            let Node::Split(split) = node else {
                break;
            };
            node = split.side_mut(*side);
        }
        node
    }

    /// The room the node at `path` has in `area`.
    fn area_at(&self, path: &[Side], area: Rect) -> Rect {
        let mut node = &self.root;
        let mut room = area;
        for side in path {
            let Node::Split(split) = node else {
                break;
            };
            let (first, second) = split.cut(room);
            (node, room) = match side {
                Side::First => (&split.first, first),
                Side::Second => (&split.second, second),
            };
        }
        room
    }

    /// Every split, down the tree, outer ones first: the way down to it,
    /// and its room in `area`.
    fn splits_in(&self, area: Rect) -> Vec<(Vec<Side>, &Split, Rect)> {
        let mut found = Vec::new();
        self.root.gather_splits(area, &mut Vec::new(), &mut found);
        found
    }

    /// How far down `path` the nearest split `way` above its pane is that
    /// has the pane on its `side`.
    fn nearest_split(&self, path: &[Side], way: Way, side: Side) -> Option<usize> {
        (0..path.len()).rev().find(|&depth| {
            let is_way =
                matches!(self.node(&path[..depth]), Node::Split(split) if split.way == way);
            is_way && path[depth] == side
        })
    }

    /// Puts the border of the split at `path`, which has `room`, `first`
    /// columns or rows into it, kept where both its sides still have the
    /// fewest columns and rows their panes are left with. Says whether it
    /// moved.
    fn put_border(&mut self, path: &[Side], first: i32, room: Rect) -> bool {
        let Node::Split(split) = self.node_mut(path) else {
            return false;
        };
        let length = along_split(room, split.way);
        let lowest = split.first.least(split.way);
        let highest = length.saturating_sub(split.second.least(split.way));
        if length == 0 || lowest > highest {
            return false;
        }
        let first = first.clamp(i32::from(lowest), i32::from(highest)) as u16;
        if first == share(length, split.ratio) {
            return false;
        }
        split.ratio = f32::from(first) / f32::from(length);
        true
    }
}

impl Node {
    fn split(way: Way, ratio: f32, first: Node, second: Node) -> Node {
        let ratio = if ratio.is_finite() {
            ratio.clamp(0.0, 1.0)
        } else {
            0.5
        };
        Node::Split(Box::new(Split {
            way,
            ratio,
            first,
            second,
        }))
    }

    fn fold<T>(
        &self,
        pane: &mut impl FnMut(&Pane) -> T,
        split: &mut impl FnMut(Way, f32, T, T) -> T,
    ) -> T {
        match self {
            Node::Pane(found) => pane(found),
            Node::Split(node) => {
                let first = node.first.fold(pane, split);
                let second = node.second.fold(pane, split);
                split(node.way, node.ratio, first, second)
            }
        }
    }

    fn gather_panes<'a>(&'a self, panes: &mut Vec<&'a Pane>) {
        match self {
            Node::Pane(pane) => panes.push(pane),
            Node::Split(split) => {
                split.first.gather_panes(panes);
                split.second.gather_panes(panes);
            }
        }
    }

    fn place<'a>(&'a self, area: Rect, placed: &mut Vec<(&'a Pane, Rect)>) {
        match self {
            Node::Pane(pane) => placed.push((pane, area)),
            Node::Split(split) => {
                let (first, second) = split.cut(area);
                split.first.place(first, placed);
                split.second.place(second, placed);
            }
        }
    }

    /// Whether `pane` is under this node, with the way down to it added to
    /// `path` if it is.
    fn find(&self, pane: &Pane, path: &mut Vec<Side>) -> bool {
        let Node::Split(split) = self else {
            return matches!(self, Node::Pane(found) if found == pane);
        };
        for side in [Side::First, Side::Second] {
            path.push(side);
            if split.side(side).find(pane, path) {
                return true;
            }
            path.pop();
        }
        false
    }

    fn gather_splits<'a>(
        &'a self,
        area: Rect,
        path: &mut Vec<Side>,
        found: &mut Vec<(Vec<Side>, &'a Split, Rect)>,
    ) {
        let Node::Split(split) = self else {
            return;
        };
        let split: &Split = split;
        found.push((path.clone(), split, area));
        let (first, second) = split.cut(area);
        for (side, room) in [(Side::First, first), (Side::Second, second)] {
            path.push(side);
            split.side(side).gather_splits(room, path, found);
            path.pop();
        }
    }

    /// This node with only the panes `keep` keeps, or nothing when it
    /// keeps none: a split that loses one side is its other side.
    fn kept(self, keep: &mut impl FnMut(&Pane) -> bool) -> Option<Node> {
        match self {
            Node::Pane(pane) => keep(&pane).then_some(Node::Pane(pane)),
            Node::Split(split) => {
                let Split {
                    way,
                    ratio,
                    first,
                    second,
                } = *split;
                match (first.kept(keep), second.kept(keep)) {
                    (Some(first), Some(second)) => Some(Node::split(way, ratio, first, second)),
                    (one, other) => one.or(other),
                }
            }
        }
    }

    /// The fewest columns (`Right`) or rows (`Down`) this node's panes fit
    /// in.
    fn least(&self, way: Way) -> u16 {
        match self {
            Node::Pane(_) => least(way),
            Node::Split(split) if split.way == way => {
                split.first.least(way) + rule(way) + split.second.least(way)
            }
            Node::Split(split) => split.first.least(way).max(split.second.least(way)),
        }
    }

    /// How many panes this node puts in a line `way`: those of splits the
    /// same way, and one for anything else.
    fn in_line(&self, way: Way) -> u16 {
        match self {
            Node::Split(split) if split.way == way => {
                split.first.in_line(way) + split.second.in_line(way)
            }
            _ => 1,
        }
    }

    fn equalize(&mut self) {
        let Node::Split(split) = self else {
            return;
        };
        split.first.equalize();
        split.second.equalize();
        let first = f32::from(split.first.in_line(split.way));
        let second = f32::from(split.second.in_line(split.way));
        split.ratio = first / (first + second);
    }
}

impl Split {
    fn side(&self, side: Side) -> &Node {
        match side {
            Side::First => &self.first,
            Side::Second => &self.second,
        }
    }

    fn side_mut(&mut self, side: Side) -> &mut Node {
        match side {
            Side::First => &mut self.first,
            Side::Second => &mut self.second,
        }
    }

    /// Its sides' rooms in `area`: the first `ratio` of the columns or
    /// rows, less the rule's column side by side, and the second the rest.
    fn cut(&self, area: Rect) -> (Rect, Rect) {
        let first = share(along_split(area, self.way), self.ratio);
        match self.way {
            Way::Right => {
                let rest = along_split(area, self.way) - first;
                let second_x = area.x + (first + 1).min(area.width);
                (
                    Rect {
                        width: first,
                        ..area
                    },
                    Rect {
                        x: second_x,
                        width: rest,
                        ..area
                    },
                )
            }
            Way::Down => (
                Rect {
                    height: first,
                    ..area
                },
                Rect {
                    y: area.y + first,
                    height: area.height - first,
                    ..area
                },
            ),
        }
    }
}

/// The fewest columns or rows a pane is left with, `way`.
fn least(way: Way) -> u16 {
    match way {
        Way::Right => MIN_COLUMNS,
        Way::Down => MIN_ROWS,
    }
}

/// The columns a split `way` keeps between its sides: one for the rule
/// side by side, none one above the other.
fn rule(way: Way) -> u16 {
    match way {
        Way::Right => 1,
        Way::Down => 0,
    }
}

/// `area`'s columns going `Right`, or its rows going `Down`.
fn along(area: Rect, way: Way) -> u16 {
    match way {
        Way::Right => area.width,
        Way::Down => area.height,
    }
}

/// What a split `way` in `area` shares between its sides: all of it but
/// the rule's column.
fn along_split(area: Rect, way: Way) -> u16 {
    along(area, way).saturating_sub(rule(way))
}

/// `ratio` of `length`, in whole columns or rows.
fn share(length: u16, ratio: f32) -> u16 {
    ((f32::from(length) * ratio).round() as u16).min(length)
}

/// How far `to` is from `from` going `toward`: the columns or rows between
/// their facing edges, if it's that way at all.
fn gap(from: Rect, to: Rect, toward: Direction) -> Option<u16> {
    match toward {
        Direction::Left => from.x.checked_sub(to.right()),
        Direction::Right => to.x.checked_sub(from.right()),
        Direction::Up => from.y.checked_sub(to.bottom()),
        Direction::Down => to.y.checked_sub(from.bottom()),
    }
}

/// The span `area` covers across the way `toward` goes: its rows going
/// left or right, its columns going up or down.
fn across(area: Rect, toward: Direction) -> (u32, u32) {
    let (start, end) = match toward.way() {
        Way::Right => (area.y, area.bottom()),
        Way::Down => (area.x, area.right()),
    };
    (u32::from(start), u32::from(end))
}

/// How much `from` and `to` face each other going `toward`: the rows, or
/// columns, they share.
fn facing(from: Rect, to: Rect, toward: Direction) -> u32 {
    let (from, to) = (across(from, toward), across(to, toward));
    from.1.min(to.1).saturating_sub(from.0.max(to.0))
}

/// How far apart the middles of `from` and `to` are across the way
/// `toward` goes, doubled to stay in whole cells.
fn off_centre(from: Rect, to: Rect, toward: Direction) -> u32 {
    let (from, to) = (across(from, toward), across(to, toward));
    (from.0 + from.1).abs_diff(to.0 + to.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str) -> Pane {
        Pane::Session(name.into())
    }

    /// The panes by name, `*` for the selection's, in the order drawn.
    fn names(tree: &SplitTree) -> Vec<&str> {
        let panes = tree.panes().into_iter();
        panes
            .map(|pane| match pane {
                Pane::Selection => "*",
                Pane::Session(name) => name.as_str(),
            })
            .collect()
    }

    /// Where the pane called `name` goes in `area`, `*` being the
    /// selection's.
    fn area(tree: &SplitTree, name: &str, area: Rect) -> Rect {
        let pane = match name {
            "*" => Pane::Selection,
            name => session(name),
        };
        tree.area_of(&pane, area).unwrap()
    }

    const ROOM: Rect = Rect::new(0, 0, 101, 40);

    /// `a` | (`*` over `b`): a on the left, the selection's pane over b on
    /// the right.
    fn three() -> SplitTree {
        let mut tree = SplitTree::default();
        assert!(tree.split(&Pane::Selection, Way::Right, 0.5, session("a")));
        tree.swap(&Pane::Selection, &session("a"));
        assert!(tree.split(&Pane::Selection, Way::Down, 0.5, session("b")));
        tree
    }

    #[test]
    fn a_new_tree_is_the_selections_pane_taking_all_the_room() {
        let tree = SplitTree::default();
        assert_eq!(names(&tree), ["*"]);
        assert_eq!(tree.layout(ROOM), [(&Pane::Selection, ROOM)]);
        assert!(tree.borders(ROOM).is_empty());
    }

    #[test]
    fn a_split_side_by_side_keeps_a_column_for_the_rule() {
        let mut tree = SplitTree::default();
        tree.split(&Pane::Selection, Way::Right, 0.5, session("a"));
        assert_eq!(names(&tree), ["*", "a"]);
        assert_eq!(area(&tree, "*", ROOM), Rect::new(0, 0, 50, 40));
        assert_eq!(area(&tree, "a", ROOM), Rect::new(51, 0, 50, 40));
        let border = tree.borders(ROOM)[0];
        assert_eq!(border.line, Rect::new(50, 0, 1, 40));
    }

    #[test]
    fn a_split_one_above_the_other_shares_every_row() {
        let mut tree = SplitTree::default();
        tree.split(&Pane::Selection, Way::Down, 0.25, session("a"));
        assert_eq!(area(&tree, "*", ROOM), Rect::new(0, 0, 101, 10));
        assert_eq!(area(&tree, "a", ROOM), Rect::new(0, 10, 101, 30));
        // The border is the row of a's header line.
        assert_eq!(tree.borders(ROOM)[0].line, Rect::new(0, 10, 101, 1));
    }

    #[test]
    fn splits_go_as_deep_as_wanted_and_each_has_its_border() {
        let mut tree = three();
        tree.split(&session("b"), Way::Right, 0.5, session("c"));
        assert_eq!(names(&tree), ["a", "*", "b", "c"]);
        assert_eq!(area(&tree, "b", ROOM), Rect::new(51, 20, 25, 20));
        assert_eq!(area(&tree, "c", ROOM), Rect::new(77, 20, 24, 20));
        let borders: Vec<(usize, Way, Rect)> = (tree.borders(ROOM).into_iter())
            .map(|border| (border.split, border.way, border.line))
            .collect();
        assert_eq!(
            borders,
            [
                (0, Way::Right, Rect::new(50, 0, 1, 40)),
                (1, Way::Down, Rect::new(51, 20, 50, 1)),
                (2, Way::Right, Rect::new(76, 20, 1, 20)),
            ]
        );
    }

    #[test]
    fn a_pane_is_in_a_tree_once() {
        let mut tree = three();
        assert!(!tree.split(&session("a"), Way::Down, 0.5, session("b")));
        assert!(!tree.split(&session("gone"), Way::Down, 0.5, session("c")));
        assert!(!tree.replace(&session("a"), session("b")));
        assert!(tree.replace(&session("a"), session("c")));
        assert_eq!(names(&tree), ["c", "*", "b"]);
    }

    #[test]
    fn closing_a_pane_gives_its_room_to_the_other_side_of_its_split() {
        let mut tree = three();
        assert!(tree.close(&Pane::Selection));
        assert_eq!(names(&tree), ["a", "b"]);
        assert_eq!(area(&tree, "b", ROOM), Rect::new(51, 0, 50, 40));
        assert!(tree.close(&session("a")));
        assert_eq!(tree.layout(ROOM), [(&session("b"), ROOM)]);
        // The last pane stays.
        assert!(!tree.close(&session("b")));
        assert!(!tree.close(&session("gone")));
    }

    #[test]
    fn closing_deep_down_keeps_the_rest_of_the_tree_as_it_was() {
        let mut tree = three();
        tree.split(&session("b"), Way::Right, 0.5, session("c"));
        tree.close(&session("b"));
        assert_eq!(names(&tree), ["a", "*", "c"]);
        assert_eq!(area(&tree, "c", ROOM), Rect::new(51, 20, 50, 20));
        assert_eq!(area(&tree, "a", ROOM), Rect::new(0, 0, 50, 40));
    }

    #[test]
    fn swapping_two_panes_leaves_the_splits_where_they_were() {
        let mut tree = three();
        tree.drag(0, 30, ROOM);
        assert!(tree.swap(&session("a"), &session("b")));
        assert_eq!(names(&tree), ["b", "*", "a"]);
        assert_eq!(area(&tree, "b", ROOM), Rect::new(0, 0, 30, 40));
        assert!(!tree.swap(&session("a"), &session("gone")));
    }

    #[test]
    fn the_neighbour_is_the_pane_that_way_facing_it_most() {
        let tree = three();
        let next = |name: &str, toward| {
            let pane = if name == "*" {
                Pane::Selection
            } else {
                session(name)
            };
            tree.neighbour(&pane, toward, ROOM).cloned()
        };
        assert_eq!(next("*", Direction::Left), Some(session("a")));
        assert_eq!(next("b", Direction::Left), Some(session("a")));
        assert_eq!(next("*", Direction::Down), Some(session("b")));
        assert_eq!(next("b", Direction::Up), Some(Pane::Selection));
        // At the edge there's none.
        assert_eq!(next("a", Direction::Left), None);
        assert_eq!(next("a", Direction::Up), None);
        assert_eq!(next("*", Direction::Right), None);
        // a faces both on its right equally: the first, the upper.
        assert_eq!(next("a", Direction::Right), Some(Pane::Selection));
    }

    #[test]
    fn going_across_uneven_panes_lands_on_the_one_facing_most() {
        // a over b on the left, the lower taking three quarters; c on the
        // right, beside both.
        let mut tree = SplitTree::in_line(vec![session("a"), session("c")], Way::Right);
        tree.split(&session("a"), Way::Down, 0.25, session("b"));
        assert_eq!(
            tree.neighbour(&session("c"), Direction::Left, ROOM),
            Some(&session("b"))
        );
    }

    #[test]
    fn a_resize_moves_the_border_on_that_side_or_else_the_other() {
        let mut tree = three();
        // a has a border on its right: it grows into it.
        assert!(tree.resize(&session("a"), Direction::Right, 4, ROOM));
        assert_eq!(area(&tree, "a", ROOM).width, 54);
        // b has none on its right: its left one moves right, and it
        // shrinks.
        assert!(tree.resize(&session("b"), Direction::Right, 4, ROOM));
        assert_eq!(area(&tree, "b", ROOM), Rect::new(59, 20, 42, 20));
        // Up and down move the border between the selection's pane and b.
        assert!(tree.resize(&Pane::Selection, Direction::Down, 5, ROOM));
        assert_eq!(area(&tree, "*", ROOM).height, 25);
        assert!(tree.resize(&session("b"), Direction::Up, 10, ROOM));
        assert_eq!(area(&tree, "b", ROOM).height, 25);
        // a has nothing above or below it.
        assert!(!tree.resize(&session("a"), Direction::Up, 2, ROOM));
    }

    #[test]
    fn a_resize_stops_where_a_pane_would_get_too_small() {
        let mut tree = three();
        assert!(tree.resize(&session("a"), Direction::Left, 100, ROOM));
        assert_eq!(area(&tree, "a", ROOM).width, MIN_COLUMNS);
        assert!(!tree.resize(&session("a"), Direction::Left, 1, ROOM));
        assert!(tree.resize(&Pane::Selection, Direction::Up, 100, ROOM));
        assert_eq!(area(&tree, "*", ROOM).height, MIN_ROWS);
    }

    #[test]
    fn a_resize_leaves_room_for_every_pane_on_the_other_side() {
        // a | (b | c): moving a's border right stops where b and c and the
        // rule between them still fit.
        let panes = vec![session("a"), session("b"), session("c")];
        let mut tree = SplitTree::in_line(panes, Way::Right);
        tree.resize(&session("a"), Direction::Right, 100, ROOM);
        assert_eq!(area(&tree, "b", ROOM).width, MIN_COLUMNS);
        assert_eq!(area(&tree, "c", ROOM).width, MIN_COLUMNS);
    }

    #[test]
    fn a_border_dragged_goes_where_the_mouse_is_as_far_as_there_is_room() {
        let mut tree = three();
        assert!(tree.drag(0, 70, ROOM));
        assert_eq!(tree.borders(ROOM)[0].line.x, 70);
        assert_eq!(area(&tree, "a", ROOM).width, 70);
        // The rows of a border one above the other: the second's header.
        assert!(tree.drag(1, 30, ROOM));
        assert_eq!(area(&tree, "b", ROOM).y, 30);
        assert!(tree.drag(0, 0, ROOM));
        assert_eq!(area(&tree, "a", ROOM).width, MIN_COLUMNS);
        assert!(!tree.drag(0, 0, ROOM), "it's as far as it goes");
        assert!(!tree.drag(7, 10, ROOM), "there's no such split");
    }

    #[test]
    fn evening_out_gives_panes_in_a_line_the_same_room() {
        let mut tree = SplitTree::default();
        tree.split(&Pane::Selection, Way::Right, 0.8, session("a"));
        tree.split(&session("a"), Way::Right, 0.2, session("b"));
        tree.split(&session("b"), Way::Down, 0.9, session("c"));
        tree.equalize();
        let room = Rect::new(0, 0, 92, 40);
        let widths: Vec<u16> = (tree.layout(room).into_iter())
            .map(|(_, area)| area.width)
            .collect();
        // Three across, b over c counting as one.
        assert_eq!(widths, [30, 30, 30, 30]);
        assert_eq!(area(&tree, "c", room).height, 20);
    }

    #[test]
    fn panes_in_a_line_start_out_even() {
        let panes = vec![session("a"), Pane::Selection, session("b")];
        let tree = SplitTree::in_line(panes, Way::Down);
        let heights: Vec<u16> = (tree.layout(Rect::new(0, 0, 80, 30)).into_iter())
            .map(|(_, area)| area.height)
            .collect();
        assert_eq!(heights, [10, 10, 10]);
        assert_eq!(names(&tree), ["a", "*", "b"]);
    }

    #[test]
    fn a_pane_has_room_to_split_while_both_halves_fit() {
        let tree = SplitTree::default();
        let room = Rect::new(0, 0, 2 * MIN_COLUMNS + 1, 2 * MIN_ROWS);
        assert!(tree.has_room(&Pane::Selection, Way::Right, room));
        assert!(tree.has_room(&Pane::Selection, Way::Down, room));
        let small = Rect::new(0, 0, 2 * MIN_COLUMNS, 2 * MIN_ROWS - 1);
        assert!(!tree.has_room(&Pane::Selection, Way::Right, small));
        assert!(!tree.has_room(&Pane::Selection, Way::Down, small));
        assert!(!tree.has_room(&session("gone"), Way::Down, room));
    }

    #[test]
    fn retaining_closes_the_panes_of_sessions_not_kept_but_the_selections() {
        let mut tree = three();
        tree.split(&session("b"), Way::Right, 0.5, session("c"));
        tree.retain(|name| name != "b");
        assert_eq!(names(&tree), ["a", "*", "c"]);
        tree.retain(|_| false);
        assert_eq!(tree, SplitTree::default());
    }

    #[test]
    fn a_share_goes_to_the_panes_side_of_the_nearest_split_or_the_nearest_that_way() {
        let mut tree = three();
        // b is the second side of the split one above the other.
        assert!(tree.set_share(&session("b"), None, 0.25));
        assert_eq!(area(&tree, "b", ROOM), Rect::new(51, 30, 50, 10));
        assert_eq!(area(&tree, "*", ROOM), Rect::new(51, 0, 50, 30));
        // Side by side, its side is the second of the split at the top.
        assert!(tree.set_share(&session("b"), Some(Way::Right), 0.7));
        assert_eq!(area(&tree, "a", ROOM).width, 30);
        assert_eq!(area(&tree, "b", ROOM), Rect::new(31, 30, 70, 10));

        assert!(!tree.set_share(&session("a"), Some(Way::Down), 0.5));
        assert!(!tree.set_share(&session("gone"), None, 0.5));
        assert!(!SplitTree::default().set_share(&Pane::Selection, None, 0.5));
    }

    #[test]
    fn trees_join_into_one_split_each_side_kept_whole() {
        let below = SplitTree::joined(
            Way::Down,
            0.25,
            SplitTree::of(Pane::Selection),
            SplitTree::of(session("b")),
        );
        let tree = SplitTree::joined(Way::Right, 0.5, SplitTree::of(session("a")), below);
        assert_eq!(names(&tree), ["a", "*", "b"]);
        assert_eq!(area(&tree, "*", ROOM), Rect::new(51, 0, 50, 10));
        assert_eq!(area(&tree, "b", ROOM), Rect::new(51, 10, 50, 30));
    }

    #[test]
    fn a_tree_keeps_one_selections_pane_whatever_it_was_read_as() {
        let none: SplitTree = serde_json::from_str(
            r#"{"way": "down", "ratio": 0.5, "first": {"session": "a"}, "second": {"session": "b"}}"#,
        )
        .unwrap();
        let mut tree = none;
        tree.retain(|_| true);
        assert_eq!(names(&tree), ["a", "b", "*"]);

        let two: SplitTree = serde_json::from_str(
            r#"{"way": "right", "ratio": 7, "first": "selection", "second": "selection"}"#,
        )
        .unwrap();
        let mut tree = two;
        tree.retain(|_| true);
        assert_eq!(tree, SplitTree::default());
    }

    #[test]
    fn a_ratio_read_out_of_range_is_put_right() {
        let mut tree: SplitTree = serde_json::from_str(
            r#"{"way": "right", "ratio": 3.5, "first": "selection", "second": {"session": "a"}}"#,
        )
        .unwrap();
        tree.retain(|_| true);
        assert_eq!(area(&tree, "*", ROOM).width, 100);
    }

    #[test]
    fn a_tree_folds_from_its_panes_up() {
        let mut tree = three();
        tree.split(&session("b"), Way::Right, 0.25, session("c"));
        let drawn = tree.fold(
            &mut |pane| match pane {
                Pane::Selection => "*".to_string(),
                Pane::Session(name) => name.clone(),
            },
            &mut |way, ratio, first, second| {
                let way = if way == Way::Right { "|" } else { "/" };
                format!("({first} {way}{ratio} {second})")
            },
        );
        assert_eq!(drawn, "(a |0.5 (* /0.5 (b |0.25 c)))");
    }

    #[test]
    fn a_tree_written_down_reads_back_the_same() {
        let mut tree = three();
        tree.split(&session("b"), Way::Right, 0.3, session("c"));
        tree.equalize();
        tree.drag(0, 33, ROOM);
        let text = serde_json::to_string(&tree).unwrap();
        assert!(text.contains(r#""way":"right""#), "{text}");
        assert!(text.contains(r#"{"session":"a"}"#), "{text}");
        assert!(text.contains(r#""selection""#), "{text}");
        let read: SplitTree = serde_json::from_str(&text).unwrap();
        assert_eq!(read, tree);
        assert_eq!(read.layout(ROOM), tree.layout(ROOM));
    }
}
