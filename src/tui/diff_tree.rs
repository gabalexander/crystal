//! The diff view's list of files folded into a directory tree, `t`: the
//! shape of a change, the directories it touches, at a glance, where the
//! flat list spreads it over every path. Every directory starts open, since
//! the list is only what changed already, and a chain of directories that
//! hold nothing but one another reads as one row, `src/tui`, so a change
//! three levels down doesn't cost three rows before its first file.
//!
//! The tree says which rows there are and which directories are folded;
//! which row is selected stays with the view, as it does for the flat list.

use std::collections::BTreeMap;

pub struct Tree {
    nodes: Vec<Node>,
    /// The nodes at the top, in order.
    top: Vec<usize>,
    /// The rows on show, top to bottom, as nodes: every node but those
    /// inside a folded directory.
    rows: Vec<usize>,
}

pub struct Node {
    /// What its row shows: a file's name, or a directory's with those of
    /// the chain it stands for, like `src/tui`.
    pub name: String,
    /// From the top of the worktree; for a chain, its deepest directory's.
    pub path: String,
    pub depth: usize,
    pub kind: Kind,
    parent: Option<usize>,
}

pub enum Kind {
    /// A changed file, by its place in the diff's list of files.
    File(usize),
    Dir {
        children: Vec<usize>,
        open: bool,
    },
}

/// The directories and files under one directory, while the tree is built.
#[derive(Default)]
struct Level<'a> {
    dirs: BTreeMap<&'a str, Level<'a>>,
    files: Vec<(&'a str, usize)>,
}

impl Level<'_> {
    /// The one directory in this one, when there's nothing else in it.
    fn only_dir(&self) -> Option<(&str, &Level<'_>)> {
        if !self.files.is_empty() || self.dirs.len() != 1 {
            return None;
        }
        let (name, level) = self.dirs.first_key_value()?;
        Some((name, level))
    }
}

impl Tree {
    /// The tree of `paths`, the diff's files in their order, every
    /// directory open: directories first at each level, then files, each
    /// by name.
    pub fn new(paths: &[&str]) -> Tree {
        let mut root = Level::default();
        for (file, path) in paths.iter().enumerate() {
            let (dirs, name) = match path.rsplit_once('/') {
                Some((dirs, name)) => (dirs.split('/').collect(), name),
                None => (Vec::new(), *path),
            };
            let level = dirs
                .into_iter()
                .fold(&mut root, |level, dir| level.dirs.entry(dir).or_default());
            level.files.push((name, file));
        }
        let mut tree = Tree {
            nodes: Vec::new(),
            top: Vec::new(),
            rows: Vec::new(),
        };
        tree.top = tree.add(&root, "", 0, None);
        tree.show();
        tree
    }

    /// Adds what's in `level`, the directory at `path`, as nodes `depth`
    /// down, and returns them.
    fn add(
        &mut self,
        level: &Level,
        path: &str,
        depth: usize,
        parent: Option<usize>,
    ) -> Vec<usize> {
        let mut added = Vec::new();
        for (name, mut inside) in &level.dirs {
            let mut name = name.to_string();
            // A directory with only a directory in it is one row with it.
            while let Some((only, deeper)) = inside.only_dir() {
                name = format!("{name}/{only}");
                inside = deeper;
            }
            let path = join(path, &name);
            let at = self.push(Node {
                name,
                path: path.clone(),
                depth,
                kind: Kind::Dir {
                    children: Vec::new(),
                    open: true,
                },
                parent,
            });
            let children = self.add(inside, &path, depth + 1, Some(at));
            if let Kind::Dir { children: kept, .. } = &mut self.nodes[at].kind {
                *kept = children;
            }
            added.push(at);
        }
        let mut files = level.files.clone();
        files.sort();
        for (name, file) in files {
            added.push(self.push(Node {
                name: name.to_string(),
                path: join(path, name),
                depth,
                kind: Kind::File(file),
                parent,
            }));
        }
        added
    }

    fn push(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// The rows on show, as nodes.
    pub fn rows(&self) -> &[usize] {
        &self.rows
    }

    pub fn node(&self, node: usize) -> &Node {
        &self.nodes[node]
    }

    /// The row of `file`, opening the directories it's folded away in.
    pub fn reveal(&mut self, file: usize) -> Option<usize> {
        let node = self
            .nodes
            .iter()
            .position(|node| matches!(node.kind, Kind::File(at) if at == file))?;
        let mut parent = self.nodes[node].parent;
        while let Some(dir) = parent {
            self.set_open(dir, true);
            parent = self.nodes[dir].parent;
        }
        self.show();
        self.rows.iter().position(|row| *row == node)
    }

    /// Folds or opens the directory on `row`, and returns where the row
    /// `selected` went: it stays on its node, or, folded away, it's on the
    /// directory.
    pub fn toggle(&mut self, row: usize, selected: usize) -> usize {
        let Some(&dir) = self.rows.get(row) else {
            return selected;
        };
        let Kind::Dir { open, .. } = self.nodes[dir].kind else {
            return selected;
        };
        let was_on = self.rows.get(selected).copied();
        self.set_open(dir, !open);
        self.show();
        let row_of = |node| self.rows.iter().position(|row| *row == node);
        was_on.and_then(row_of).or_else(|| row_of(dir)).unwrap_or(0)
    }

    /// `→` on `row`: opens a folded directory, or goes into an open one.
    /// Returns the row selected then.
    pub fn open(&mut self, row: usize) -> usize {
        let Some(&node) = self.rows.get(row) else {
            return row;
        };
        match &self.nodes[node].kind {
            Kind::Dir { open: false, .. } => self.toggle(row, row),
            Kind::Dir { children, .. } if !children.is_empty() => row + 1,
            _ => row,
        }
    }

    /// `←` on `row`: folds an open directory, or goes to the directory
    /// it's in. Returns the row selected then.
    pub fn fold(&mut self, row: usize) -> usize {
        let Some(&node) = self.rows.get(row) else {
            return row;
        };
        if let Kind::Dir { open: true, .. } = self.nodes[node].kind {
            return self.toggle(row, row);
        }
        let parent = self.nodes[node].parent;
        parent
            .and_then(|dir| self.rows.iter().position(|row| *row == dir))
            .unwrap_or(row)
    }

    /// Every file under `node`, in the tree's order: just itself, for a
    /// file's node.
    pub fn files_under(&self, node: usize) -> Vec<usize> {
        match &self.nodes[node].kind {
            Kind::File(file) => vec![*file],
            Kind::Dir { children, .. } => children
                .iter()
                .flat_map(|child| self.files_under(*child))
                .collect(),
        }
    }

    fn set_open(&mut self, dir: usize, to: bool) {
        if let Kind::Dir { open, .. } = &mut self.nodes[dir].kind {
            *open = to;
        }
    }

    /// Works out the rows on show again, after a directory folded or
    /// opened.
    fn show(&mut self) {
        let mut rows = Vec::new();
        let mut next: Vec<usize> = self.top.iter().rev().copied().collect();
        while let Some(node) = next.pop() {
            rows.push(node);
            if let Kind::Dir {
                children,
                open: true,
            } = &self.nodes[node].kind
            {
                next.extend(children.iter().rev());
            }
        }
        self.rows = rows;
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each row as the list draws it: its depth, and its name.
    fn shown(tree: &Tree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|node| {
                let node = tree.node(*node);
                format!("{}:{}", node.depth, node.name)
            })
            .collect()
    }

    #[test]
    fn directories_come_first_and_a_chain_of_them_is_one_row() {
        let tree = Tree::new(&[
            "README.md",
            "crates/tui/src/app.rs",
            "crates/tui/src/ui.rs",
            "crates/tui/Cargo.toml",
            "docs/guide/keys.md",
        ]);
        assert_eq!(
            shown(&tree),
            [
                // `crates` holds only `tui`; `tui` holds a file beside
                // `src`, so the chain stops there.
                "0:crates/tui",
                "1:src",
                "2:app.rs",
                "2:ui.rs",
                "1:Cargo.toml",
                "0:docs/guide",
                "1:keys.md",
                "0:README.md",
            ]
        );
        assert_eq!(tree.node(tree.rows()[0]).path, "crates/tui");
        assert_eq!(tree.node(tree.rows()[2]).path, "crates/tui/src/app.rs");
    }

    #[test]
    fn each_files_row_knows_its_place_in_the_diff() {
        let tree = Tree::new(&["b/two.rs", "a/one.rs", "top.rs"]);
        let files: Vec<Option<usize>> = tree
            .rows()
            .iter()
            .map(|node| match tree.node(*node).kind {
                Kind::File(file) => Some(file),
                Kind::Dir { .. } => None,
            })
            .collect();
        assert_eq!(files, [None, Some(1), None, Some(0), Some(2)]);
    }

    #[test]
    fn folding_hides_a_directorys_files_and_keeps_the_selection_in_sight() {
        let mut tree = Tree::new(&["a/x.rs", "a/y.rs", "b.rs"]);
        // Folding `a` from inside it: the selection goes to `a`.
        assert_eq!(tree.toggle(0, 2), 0);
        assert_eq!(shown(&tree), ["0:a", "0:b.rs"]);
        // From below it, the selection stays on its file.
        tree.toggle(0, 0);
        assert_eq!(tree.toggle(0, 3), 1);
        // → opens it again without moving; a second → goes into it.
        assert_eq!(tree.open(0), 0);
        assert_eq!(shown(&tree), ["0:a", "1:x.rs", "1:y.rs", "0:b.rs"]);
        assert_eq!(tree.open(0), 1);
        // ← on a file goes to its directory; ← there folds it.
        assert_eq!(tree.fold(2), 0);
        assert_eq!(tree.fold(0), 0);
        assert_eq!(shown(&tree), ["0:a", "0:b.rs"]);
    }

    #[test]
    fn revealing_a_file_opens_what_hides_it() {
        let mut tree = Tree::new(&["a/sub/x.rs", "a/y.rs"]);
        tree.toggle(0, 0);
        assert_eq!(shown(&tree), ["0:a"]);
        assert_eq!(tree.reveal(0), Some(2));
        assert_eq!(tree.reveal(7), None);
    }

    #[test]
    fn a_directorys_files_are_everything_under_it() {
        let tree = Tree::new(&["a/x.rs", "a/sub/y.rs", "b.rs"]);
        assert_eq!(tree.files_under(tree.rows()[0]), [1, 0]);
    }
}
