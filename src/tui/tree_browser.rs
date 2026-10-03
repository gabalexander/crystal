//! The tree browser, `E` in the sidebar: a worktree's files as a tree on
//! the left, and the selected one previewed on the right, highlighted, or
//! a markdown file as its page. Whatever is typed filters the tree, down
//! to the files whose paths match and the directories they're in; `→` and
//! `←` open and fold a directory. The files are the ones the file finder
//! lists, git's: what git ignores isn't in the tree.
//!
//! Adapted from docket's tree browser.

use super::app::{Action, Hit, Loading, Outcome};
use super::finder::{self, QUERY_ROWS};
use super::fuzzy;
use super::preview::{self, Content, Preview};
use super::sidebar::fit;
use super::text_input::TextInput;
use super::ui::{self, Look, ViewAreas};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The narrowest the tree is, and the narrowest the preview beside it.
const NARROWEST_TREE: u16 = 20;
const NARROWEST_PREVIEW: u16 = 30;

/// A worktree's files, and the directories they're in.
#[derive(Debug)]
struct Tree {
    nodes: Vec<Node>,
    /// The nodes at the top, in order.
    top: Vec<usize>,
    /// How many of the nodes are files.
    files: usize,
    /// Which directories are open, by node. It holds while there's no
    /// filter; a filter opens every directory it keeps.
    open: Vec<bool>,
}

/// A file or a directory.
#[derive(Debug)]
struct Node {
    name: String,
    /// Its path from the top of the worktree.
    path: String,
    parent: Option<usize>,
    /// Directories first, then files, each in order of their names.
    children: Vec<usize>,
    directory: bool,
    /// How deep it is: 0 at the top.
    depth: usize,
}

/// A row of the tree on screen: a node, and the places of the letters in
/// its name the filter matched.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    node: usize,
    matched: Vec<usize>,
}

/// The rows the tree shows for a filter.
struct Visible {
    rows: Vec<Row>,
    /// The row of the file that matched best; `None` with no filter.
    best: Option<usize>,
    /// How many files match.
    matching: usize,
}

impl Tree {
    /// The tree of `files`, paths from the top of the worktree. The
    /// directories are the ones the paths go through.
    fn new(files: &[String]) -> Tree {
        let mut tree = Tree {
            nodes: Vec::new(),
            top: Vec::new(),
            files: 0,
            open: Vec::new(),
        };
        let mut directories: HashMap<String, usize> = HashMap::new();
        for path in files.iter().filter(|path| !path.is_empty()) {
            let mut parent = None;
            let parts: Vec<&str> = path.split('/').collect();
            for (depth, name) in parts.iter().enumerate() {
                let path = parts[..=depth].join("/");
                if depth + 1 == parts.len() {
                    tree.add(name, path, parent, false);
                    tree.files += 1;
                } else if let Some(&directory) = directories.get(&path) {
                    parent = Some(directory);
                } else {
                    let directory = tree.add(name, path.clone(), parent, true);
                    directories.insert(path, directory);
                    parent = Some(directory);
                }
            }
        }
        tree.sort();
        tree.open = vec![false; tree.nodes.len()];
        tree
    }

    fn add(&mut self, name: &str, path: String, parent: Option<usize>, directory: bool) -> usize {
        let index = self.nodes.len();
        self.nodes.push(Node {
            name: name.to_string(),
            path,
            parent,
            children: Vec::new(),
            directory,
            depth: parent.map_or(0, |parent| self.nodes[parent].depth + 1),
        });
        match parent {
            Some(parent) => self.nodes[parent].children.push(index),
            None => self.top.push(index),
        }
        index
    }

    /// Puts every directory's children in order: directories first, then
    /// files, each by name.
    fn sort(&mut self) {
        let key = |node: &Node| (!node.directory, node.name.clone());
        let keys: Vec<_> = self.nodes.iter().map(key).collect();
        for node in &mut self.nodes {
            node.children.sort_by(|a, b| keys[*a].cmp(&keys[*b]));
        }
        self.top.sort_by(|a, b| keys[*a].cmp(&keys[*b]));
    }

    /// The rows shown for `filter`, in the tree's order. With no filter,
    /// the open directories' children; with one, every file whose path
    /// matches it, and the directories it's in, all open.
    fn visible(&self, filter: &str) -> Visible {
        if filter.trim().is_empty() {
            return Visible {
                rows: self.walk(|node| self.open[node], |_| Some(Vec::new())),
                best: None,
                matching: self.files,
            };
        }
        let mut kept = vec![false; self.nodes.len()];
        let mut found = HashMap::new();
        for (index, node) in self.nodes.iter().enumerate() {
            if node.directory {
                continue;
            }
            let Some(found_here) = fuzzy::score(&node.path, filter) else {
                continue;
            };
            found.insert(index, found_here);
            let mut at = Some(index);
            while let Some(node) = at.filter(|node| !kept[*node]) {
                kept[node] = true;
                at = self.nodes[node].parent;
            }
        }
        let rows = self.walk(
            |_| true,
            |node| {
                if !kept[node] {
                    return None;
                }
                let Some(found) = found.get(&node) else {
                    return Some(Vec::new());
                };
                // The letters matched in the directories above aren't on
                // the row; those in the name are, counted from its start.
                let path = &self.nodes[node];
                let name_starts = path.path.chars().count() - path.name.chars().count();
                let in_name = found.positions.iter().filter(|at| **at >= name_starts);
                Some(in_name.map(|at| at - name_starts).collect())
            },
        );
        let mut best: Option<(i64, usize)> = None;
        for (index, row) in rows.iter().enumerate() {
            if let Some(found) = found.get(&row.node)
                && best.is_none_or(|(score, _)| found.score > score)
            {
                best = Some((found.score, index));
            }
        }
        Visible {
            rows,
            best: best.map(|(_, row)| row),
            matching: found.len(),
        }
    }

    /// The rows down the tree, in order: each node `shown` says has a row,
    /// with the letters it matched, and the children of each directory
    /// `open` says is open.
    fn walk(
        &self,
        open: impl Fn(usize) -> bool,
        shown: impl Fn(usize) -> Option<Vec<usize>>,
    ) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut next: Vec<usize> = self.top.iter().rev().copied().collect();
        while let Some(node) = next.pop() {
            let Some(matched) = shown(node) else {
                continue;
            };
            rows.push(Row { node, matched });
            if self.nodes[node].directory && open(node) {
                next.extend(self.nodes[node].children.iter().rev());
            }
        }
        rows
    }
}

pub struct TreeBrowser {
    /// The worktree it shows.
    pub dir: PathBuf,
    /// The worktree's project and branch, for the header.
    pub place: String,
    pub filter: TextInput,
    tree: Loading<Tree>,
    rows: Vec<Row>,
    /// How many files match the filter.
    matching: usize,
    /// The selected row.
    selected: usize,
    pub preview: Preview,
    /// How wide the tree is, once its border has been dragged.
    width: Option<u16>,
    /// Whether the border between the tree and the preview is being
    /// dragged.
    pub dragging: bool,
}

impl TreeBrowser {
    /// A browser of the worktree at `dir`, until its files are listed.
    pub fn new(dir: PathBuf, place: String) -> TreeBrowser {
        TreeBrowser {
            preview: Preview::new(dir.clone()),
            dir,
            place,
            filter: TextInput::default(),
            tree: Loading::Reading,
            rows: Vec::new(),
            matching: 0,
            selected: 0,
            width: None,
            dragging: false,
        }
    }

    /// What listing the files takes, for the event loop to do.
    pub fn read(&self) -> Action {
        Action::ReadFiles(self.dir.clone())
    }

    /// Takes the listed files, if they're this worktree's, and filters
    /// them by what's been typed meanwhile: what previewing the selected
    /// one takes.
    pub fn files_read(&mut self, dir: &Path, files: Result<Vec<String>, String>) -> Option<Action> {
        if dir != self.dir {
            return None;
        }
        self.tree = match files {
            Ok(files) => Loading::Read(Tree::new(&files)),
            Err(err) => Loading::Failed(err),
        };
        self.filter_again()
    }

    /// Takes a file read for the preview, if it's still the one shown.
    pub fn preview_read(&mut self, dir: &Path, path: &str, read: Result<Content, String>) {
        self.preview.read_done(dir, path, read);
    }

    /// The size of the preview's area, as `(rows, columns)`.
    pub fn set_size(&mut self, preview: (u16, u16)) {
        self.preview.set_size(preview);
    }

    /// How wide the tree is in a view `width` columns wide: as wide as it
    /// was dragged, or else a little under a third, leaving the preview
    /// room either way.
    pub fn list_width(&self, width: u16) -> u16 {
        let wanted = self.width.unwrap_or_else(|| (width * 3 / 10).clamp(24, 48));
        let most = width
            .saturating_sub(NARROWEST_PREVIEW)
            .max(NARROWEST_TREE)
            .min(width.saturating_sub(1));
        wanted.clamp(NARROWEST_TREE.min(most), most)
    }

    /// The selected file or directory.
    fn selected_node(&self) -> Option<&Node> {
        let Loading::Read(tree) = &self.tree else {
            return None;
        };
        Some(&tree.nodes[self.rows.get(self.selected)?.node])
    }

    /// The path of the selected file or directory.
    pub fn selected_path(&self) -> Option<&str> {
        self.selected_node().map(|node| node.path.as_str())
    }

    /// The path of the selected file, unless it's a directory.
    fn selected_file(&self) -> Option<&str> {
        self.selected_node()
            .filter(|node| !node.directory)
            .map(|node| node.path.as_str())
    }

    /// How many files there are, and how many match the filter, once
    /// they're listed.
    fn counts(&self) -> Option<(usize, usize)> {
        let Loading::Read(tree) = &self.tree else {
            return None;
        };
        Some((tree.files, self.matching))
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if self.preview.scroll_key(&key) {
            return Outcome::Stay;
        }
        match key.code {
            KeyCode::Esc if !self.filter.text().is_empty() => {
                self.filter = TextInput::default();
                doing(self.filter_again())
            }
            KeyCode::Esc => Outcome::Close,
            KeyCode::Enter => self.enter(),
            KeyCode::Up if !shift => doing(self.select_by(-1)),
            KeyCode::Down if !shift => doing(self.select_by(1)),
            KeyCode::Char('p') if ctrl => doing(self.select_by(-1)),
            KeyCode::Char('n') if ctrl => doing(self.select_by(1)),
            KeyCode::Right => doing(self.open_selected()),
            KeyCode::Left => doing(self.fold_selected()),
            KeyCode::Char('e') if ctrl => match self.selected_file() {
                Some(path) => Outcome::Edit {
                    line: self.preview.top_line(path),
                    path: path.to_string(),
                },
                None => Outcome::Stay,
            },
            KeyCode::Char('y') if ctrl => match self.selected_path() {
                Some(path) => Outcome::Do(Action::CopyPath(path.to_string())),
                None => Outcome::Stay,
            },
            KeyCode::Char('r') if ctrl => {
                self.preview.flip();
                Outcome::Stay
            }
            _ => {
                let before = self.filter.text().to_string();
                self.filter.on_key(&key);
                if self.filter.text() == before {
                    return Outcome::Stay;
                }
                doing(self.filter_again())
            }
        }
    }

    /// Pasted text goes into the filter, as if typed.
    pub fn on_paste(&mut self, text: &str) -> Outcome {
        self.filter.insert_str(text);
        doing(self.filter_again())
    }

    /// A click on a row selects it, and opens or folds a directory; the
    /// wheel moves the selection over the tree and scrolls the preview
    /// over it; and the border between them can be dragged.
    pub fn on_mouse(&mut self, kind: MouseEventKind, hit: Hit) -> Outcome {
        match (kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Hit::ViewList(row)) => {
                let preview = self.select(row);
                if self.selected_node().is_some_and(|node| node.directory) {
                    return doing(self.toggle(row));
                }
                doing(preview)
            }
            (MouseEventKind::ScrollUp, Hit::ViewList(_)) => doing(self.select_by(-1)),
            (MouseEventKind::ScrollDown, Hit::ViewList(_)) => doing(self.select_by(1)),
            (MouseEventKind::ScrollUp, Hit::ViewContent) => {
                self.preview.wheel(true);
                Outcome::Stay
            }
            (MouseEventKind::ScrollDown, Hit::ViewContent) => {
                self.preview.wheel(false);
                Outcome::Stay
            }
            (MouseEventKind::Down(MouseButton::Left), Hit::ViewBorder(_)) => {
                self.dragging = true;
                Outcome::Stay
            }
            (MouseEventKind::Drag(MouseButton::Left), Hit::ViewBorder(column)) if self.dragging => {
                self.width = Some(column);
                Outcome::Stay
            }
            (MouseEventKind::Up(MouseButton::Left), _) => {
                self.dragging = false;
                Outcome::Stay
            }
            _ => Outcome::Stay,
        }
    }

    /// Enter: a directory opens or folds; a file is read into the preview
    /// again, with whatever has changed in it since.
    fn enter(&mut self) -> Outcome {
        match self.selected_node() {
            Some(node) if node.directory => doing(self.toggle(self.selected)),
            Some(_) => Outcome::Do(self.preview.read_again()),
            None => Outcome::Stay,
        }
    }

    /// Selects the row `by` rows from the selected one, kept in range.
    fn select_by(&mut self, by: isize) -> Option<Action> {
        self.select(self.selected.saturating_add_signed(by))
    }

    /// Selects row `row`, kept in range: what previewing it takes.
    fn select(&mut self, row: usize) -> Option<Action> {
        self.selected = row.min(self.rows.len().saturating_sub(1));
        self.preview_selected()
    }

    /// Shows the selected file in the preview, or the names in the
    /// selected directory: what reading the file takes.
    fn preview_selected(&mut self) -> Option<Action> {
        let Some(node) = self.selected_node() else {
            self.preview.clear();
            return None;
        };
        if !node.directory {
            let path = node.path.clone();
            return self.preview.show(&path);
        }
        let Loading::Read(tree) = &self.tree else {
            return None;
        };
        let names = node
            .children
            .iter()
            .map(|child| {
                let child = &tree.nodes[*child];
                if child.directory {
                    format!("{}/", child.name)
                } else {
                    child.name.clone()
                }
            })
            .collect();
        let path = node.path.clone();
        self.preview.show_listing(&path, names);
        None
    }

    /// Opens or folds the directory on row `row`, keeping the selection
    /// on the row it was on, or on the directory when that was inside it:
    /// what previewing the selection then takes. With a filter, every
    /// directory stays open.
    fn toggle(&mut self, row: usize) -> Option<Action> {
        if !self.filter.text().trim().is_empty() {
            return None;
        }
        let Loading::Read(tree) = &mut self.tree else {
            return None;
        };
        let node = self.rows.get(row)?.node;
        if !tree.nodes[node].directory {
            return None;
        }
        let selected = self.rows.get(self.selected).map(|row| row.node);
        tree.open[node] = !tree.open[node];
        self.rows = tree.visible("").rows;
        let find = |wanted: usize| self.rows.iter().position(|row| row.node == wanted);
        self.selected = selected.and_then(find).or_else(|| find(node)).unwrap_or(0);
        self.preview_selected()
    }

    /// `→`: opens the selected directory, or, when it's open, goes down to
    /// its first child.
    fn open_selected(&mut self) -> Option<Action> {
        let node = self.selected_node()?;
        if !node.directory {
            return None;
        }
        let filtering = !self.filter.text().trim().is_empty();
        let Loading::Read(tree) = &self.tree else {
            return None;
        };
        let index = self.rows[self.selected].node;
        if filtering || tree.open[index] {
            if node.children.is_empty() {
                return None;
            }
            return self.select_by(1);
        }
        self.toggle(self.selected)
    }

    /// `←`: folds the selected directory when it's open, or else goes up to
    /// the directory it's in.
    fn fold_selected(&mut self) -> Option<Action> {
        let node = self.selected_node()?;
        let Loading::Read(tree) = &self.tree else {
            return None;
        };
        let index = self.rows[self.selected].node;
        let filtering = !self.filter.text().trim().is_empty();
        if node.directory && tree.open[index] && !filtering {
            return self.toggle(self.selected);
        }
        let parent = node.parent?;
        let row = self.rows.iter().position(|row| row.node == parent)?;
        self.select(row)
    }

    /// Filters the tree again for the filter as it is, with the selection
    /// on the file that matches best, or on the first row with no filter:
    /// what previewing it takes.
    fn filter_again(&mut self) -> Option<Action> {
        let visible = match &self.tree {
            Loading::Read(tree) => tree.visible(self.filter.text()),
            _ => return None,
        };
        self.rows = visible.rows;
        self.matching = visible.matching;
        self.select(visible.best.unwrap_or(0))
    }
}

/// An outcome that does `action`, when there is one.
fn doing(action: Option<Action>) -> Outcome {
    action.map_or(Outcome::Stay, Outcome::Do)
}

/// The keys the footer shows while the tree browser is open.
pub fn hints(browser: &TreeBrowser) -> Vec<(&'static str, &'static str)> {
    let mut hints = vec![
        ("↑/↓", "select"),
        ("←/→", "fold"),
        ("ctrl+e", "edit"),
        ("ctrl+y", "copy path"),
    ];
    hints.extend(preview::flip_hint(&browser.preview));
    let esc = if browser.filter.text().is_empty() {
        "close"
    } else {
        "clear"
    };
    hints.push(("esc", esc));
    hints
}

/// Which row is on screen `row`, in a tree drawn in `area`.
pub fn list_hit(browser: &TreeBrowser, area: Rect, row: u16) -> Hit {
    let Some(row) = (row - area.y).checked_sub(QUERY_ROWS) else {
        return Hit::Elsewhere;
    };
    let height = area.height.saturating_sub(QUERY_ROWS);
    let index = list_offset(browser.selected, height) + usize::from(row);
    if index < browser.rows.len() {
        Hit::ViewList(index)
    } else {
        Hit::Elsewhere
    }
}

/// The first row on screen: the tree scrolls to keep the selection in
/// sight.
fn list_offset(selected: usize, height: u16) -> usize {
    (selected + 1).saturating_sub(usize::from(height.max(1)))
}

pub fn draw(frame: &mut Frame, browser: &TreeBrowser, look: &Look, areas: &ViewAreas) {
    let theme = look.theme;
    frame.render_widget(header(browser, look, areas.header.width), areas.header);
    ui::draw_rule(frame, look, areas.rule);
    finder::draw_query(frame, &browser.filter, look, areas.list);
    let list = Rect::new(
        areas.list.x,
        areas.list.y + QUERY_ROWS,
        areas.list.width,
        areas.list.height.saturating_sub(QUERY_ROWS),
    );
    match &browser.tree {
        Loading::Reading => ui::draw_message(frame, look, "listing files…", list),
        Loading::Failed(err) => {
            let line = Line::styled(format!(" {err}"), Style::new().fg(theme.failed));
            frame.render_widget(Paragraph::new(line), list);
        }
        Loading::Read(tree) if browser.rows.is_empty() => {
            let message = if tree.files == 0 {
                "no files"
            } else {
                "nothing matches"
            };
            ui::draw_message(frame, look, message, list);
        }
        Loading::Read(tree) => draw_rows(frame, browser, tree, look, list),
    }
    preview::draw(frame, &browser.preview, look, areas.content);
}

/// "≡ tree · 12 of 1204 files", and where on the right.
fn header<'a>(browser: &TreeBrowser, look: &Look, width: u16) -> Line<'a> {
    let mut notes = Vec::new();
    if let Some((files, matching)) = browser.counts() {
        let noun = if files == 1 { "file" } else { "files" };
        notes.push(if browser.filter.text().trim().is_empty() {
            format!("{files} {noun}")
        } else {
            format!("{matching} of {files} {noun}")
        });
    }
    ui::view_header("≡", "tree", &notes, &browser.place, look, width)
}

/// The rows, one a line: indented by how deep they are, a directory with
/// a mark for whether it's open, and the letters the filter matched in
/// the accent color.
fn draw_rows(frame: &mut Frame, browser: &TreeBrowser, tree: &Tree, look: &Look, area: Rect) {
    let theme = look.theme;
    let filtering = !browser.filter.text().trim().is_empty();
    let first = list_offset(browser.selected, area.height);
    let shown = browser
        .rows
        .iter()
        .enumerate()
        .skip(first)
        .take(area.height.into());
    for (index, row) in shown {
        let y = area.y + (index - first) as u16;
        let line_area = Rect::new(area.x, y, area.width, 1);
        if index == browser.selected {
            frame.buffer_mut().set_style(line_area, theme.selection);
        }
        let node = &tree.nodes[row.node];
        let indent = " ".repeat(1 + 2 * node.depth);
        let mark = if !node.directory {
            "  "
        } else if filtering || tree.open[row.node] {
            "▾ "
        } else {
            "▸ "
        };
        let room = usize::from(area.width).saturating_sub(indent.len() + 3);
        let name = fit(&node.name, room);
        let name_style = if node.directory {
            Style::new().fg(theme.accent)
        } else {
            Style::new().fg(theme.text)
        };
        let matched = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
        let mut spans = vec![
            Span::raw(indent),
            Span::styled(mark, Style::new().fg(theme.accent)),
        ];
        spans.extend(name.chars().enumerate().map(|(at, letter)| {
            let style = if row.matched.contains(&at) {
                matched
            } else {
                name_style
            };
            Span::styled(letter.to_string(), style)
        }));
        frame.render_widget(Line::from(spans), line_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(letter: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(letter), KeyModifiers::CONTROL)
    }

    fn browser_of(files: &[&str]) -> TreeBrowser {
        let mut browser = TreeBrowser::new(PathBuf::from("/code/app"), "app ⌂ main".into());
        let files = files.iter().map(|file| file.to_string()).collect();
        browser.files_read(Path::new("/code/app"), Ok(files));
        browser
    }

    fn rows(browser: &TreeBrowser) -> Vec<&str> {
        let Loading::Read(tree) = &browser.tree else {
            return Vec::new();
        };
        browser
            .rows
            .iter()
            .map(|row| tree.nodes[row.node].path.as_str())
            .collect()
    }

    fn type_text(browser: &mut TreeBrowser, text: &str) -> Outcome {
        let mut outcome = Outcome::Stay;
        for letter in text.chars() {
            outcome = browser.on_key(key(KeyCode::Char(letter)));
        }
        outcome
    }

    #[test]
    fn the_tree_starts_folded_with_directories_first() {
        let browser = browser_of(&["b.txt", "a/x.rs", "a/y.rs", "a/sub/z.rs"]);
        assert_eq!(browser.counts(), Some((4, 4)));
        assert_eq!(rows(&browser), ["a", "b.txt"]);
        assert_eq!(browser.preview.path(), Some("a"), "a directory's names");
    }

    #[test]
    fn right_and_left_open_and_fold_and_go_down_and_up() {
        let mut browser = browser_of(&["b.txt", "a/x.rs", "a/sub/z.rs"]);
        browser.on_key(key(KeyCode::Right));
        assert_eq!(rows(&browser), ["a", "a/sub", "a/x.rs", "b.txt"]);
        browser.on_key(key(KeyCode::Right));
        assert_eq!(browser.selected_path(), Some("a/sub"), "down into it");
        browser.on_key(key(KeyCode::Right));
        assert_eq!(
            rows(&browser),
            ["a", "a/sub", "a/sub/z.rs", "a/x.rs", "b.txt"]
        );
        browser.on_key(key(KeyCode::Down));
        assert_eq!(browser.selected_path(), Some("a/sub/z.rs"));
        browser.on_key(key(KeyCode::Left));
        assert_eq!(
            browser.selected_path(),
            Some("a/sub"),
            "up to its directory"
        );
        browser.on_key(key(KeyCode::Left));
        assert_eq!(rows(&browser), ["a", "a/sub", "a/x.rs", "b.txt"], "folded");
        browser.on_key(key(KeyCode::Left));
        assert_eq!(browser.selected_path(), Some("a"));
    }

    #[test]
    fn folding_a_directory_with_the_selection_inside_selects_it() {
        let mut browser = browser_of(&["a/x.rs", "a/y.rs"]);
        browser.on_key(key(KeyCode::Right));
        browser.select(2);
        browser.toggle(0);
        assert_eq!(rows(&browser), ["a"]);
        assert_eq!(browser.selected_path(), Some("a"));
    }

    #[test]
    fn enter_opens_a_directory_and_reads_a_file_again() {
        let mut browser = browser_of(&["a/x.rs", "b.rs"]);
        assert_eq!(browser.on_key(key(KeyCode::Enter)), Outcome::Stay);
        assert_eq!(rows(&browser), ["a", "a/x.rs", "b.rs"]);
        browser.on_key(key(KeyCode::Down));
        let read = Action::ReadPreview {
            dir: PathBuf::from("/code/app"),
            path: "a/x.rs".into(),
        };
        assert_eq!(browser.on_key(key(KeyCode::Enter)), Outcome::Do(read));
    }

    #[test]
    fn the_filter_keeps_the_files_that_match_and_their_directories() {
        let mut browser = browser_of(&["a/sub/z.rs", "a/x.rs", "other/w.rs"]);
        let outcome = type_text(&mut browser, "z");
        assert_eq!(rows(&browser), ["a", "a/sub", "a/sub/z.rs"]);
        assert_eq!(browser.counts(), Some((3, 1)));
        assert_eq!(browser.selected_path(), Some("a/sub/z.rs"), "on the file");
        assert!(matches!(outcome, Outcome::Do(Action::ReadPreview { .. })));
    }

    #[test]
    fn the_filter_matches_whole_paths_and_marks_letters_in_the_name() {
        let mut browser = browser_of(&["a/sub/z.rs", "a/x.rs"]);
        // `a` is in the directory, `z` in the name.
        type_text(&mut browser, "az");
        assert_eq!(browser.counts(), Some((2, 1)));
        let file = browser.rows.last().unwrap();
        assert_eq!(file.matched, [0]);
    }

    #[test]
    fn esc_clears_the_filter_then_closes() {
        let mut browser = browser_of(&["a/x.rs", "b.txt"]);
        type_text(&mut browser, "rs");
        assert_eq!(rows(&browser), ["a", "a/x.rs"]);
        assert_eq!(hints(&browser).last(), Some(&("esc", "clear")));
        assert_ne!(browser.on_key(key(KeyCode::Esc)), Outcome::Close);
        assert_eq!(rows(&browser), ["a", "b.txt"], "folded as before");
        assert_eq!(browser.on_key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn whats_typed_before_the_files_are_listed_filters_them() {
        let mut browser = TreeBrowser::new(PathBuf::from("/code/app"), "app".into());
        type_text(&mut browser, "main");
        let files = vec!["README.md".to_string(), "src/main.rs".to_string()];
        browser.files_read(Path::new("/code/app"), Ok(files));
        assert_eq!(browser.selected_path(), Some("src/main.rs"));
    }

    #[test]
    fn ctrl_e_edits_a_file_and_ctrl_y_copies_a_path() {
        let mut browser = browser_of(&["a/x.rs", "b.rs"]);
        assert_eq!(browser.on_key(ctrl('e')), Outcome::Stay, "a directory");
        assert_eq!(
            browser.on_key(ctrl('y')),
            Outcome::Do(Action::CopyPath("a".into()))
        );
        browser.on_key(key(KeyCode::Down));
        assert_eq!(
            browser.on_key(ctrl('e')),
            Outcome::Edit {
                path: "b.rs".into(),
                line: None,
            }
        );
        assert_eq!(browser.filter.text(), "", "nothing typed");
    }

    #[test]
    fn the_previews_keys_scroll_it_and_arent_typed() {
        let mut browser = browser_of(&["a.rs"]);
        let lines = (0..100).map(|n| format!("line {n}\n")).collect::<String>();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), lines).unwrap();
        browser.preview_read(
            Path::new("/code/app"),
            "a.rs",
            preview::read(dir.path(), "a.rs"),
        );
        browser.set_size((11, 60));
        browser.on_key(key(KeyCode::PageDown));
        browser.on_key(key(KeyCode::Char(' ')));
        assert_eq!(browser.preview.scroll, 18);
        // The editor opens the file where it's scrolled to.
        assert_eq!(
            browser.on_key(ctrl('e')),
            Outcome::Edit {
                path: "a.rs".into(),
                line: Some(19),
            }
        );
        browser.on_key(key(KeyCode::Home));
        assert_eq!(browser.preview.scroll, 0);
        assert_eq!(browser.filter.text(), "");
    }

    #[test]
    fn a_click_selects_a_row_and_opens_a_directory() {
        let mut browser = browser_of(&["a/x.rs", "b.rs"]);
        browser.on_mouse(MouseEventKind::Down(MouseButton::Left), Hit::ViewList(1));
        assert_eq!(browser.selected_path(), Some("b.rs"));
        browser.on_mouse(MouseEventKind::Down(MouseButton::Left), Hit::ViewList(0));
        assert_eq!(rows(&browser), ["a", "a/x.rs", "b.rs"]);
    }

    #[test]
    fn dragging_the_border_sets_the_trees_width_within_bounds() {
        let mut browser = browser_of(&["a.rs"]);
        assert_eq!(browser.list_width(100), 30);
        browser.on_mouse(MouseEventKind::Down(MouseButton::Left), Hit::ViewBorder(30));
        assert!(browser.dragging);
        browser.on_mouse(MouseEventKind::Drag(MouseButton::Left), Hit::ViewBorder(50));
        browser.on_mouse(MouseEventKind::Up(MouseButton::Left), Hit::ViewBorder(50));
        assert!(!browser.dragging);
        assert_eq!(browser.list_width(100), 50);
        // Too far either way, it stops where both sides keep their room.
        browser.on_mouse(MouseEventKind::Down(MouseButton::Left), Hit::ViewBorder(50));
        browser.on_mouse(MouseEventKind::Drag(MouseButton::Left), Hit::ViewBorder(95));
        assert_eq!(browser.list_width(100), 100 - NARROWEST_PREVIEW);
        browser.on_mouse(MouseEventKind::Drag(MouseButton::Left), Hit::ViewBorder(2));
        assert_eq!(browser.list_width(100), NARROWEST_TREE);
    }

    #[test]
    fn files_listed_for_another_worktree_are_dropped() {
        let mut browser = TreeBrowser::new(PathBuf::from("/code/app"), "app".into());
        let read = browser.files_read(Path::new("/code/other"), Ok(vec!["x.rs".into()]));
        assert_eq!(read, None);
        assert_eq!(browser.counts(), None);
    }
}
