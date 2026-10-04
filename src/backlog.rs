//! A project's backlog: things worth doing later that aren't anyone's task
//! yet. The daemon keeps one per project, in its database, and it's the only
//! one that writes it.
//!
//! Items are numbered per project, #1 on, and keep their number: it's how
//! a person or an agent names one, and how a task started for an item says
//! which item closing it ticks. An item is a line, and a body when one line
//! isn't enough.
//!
//! Everything the backlog adds to crystal goes through [`enabled`], so it
//! can be switched off as one.

use crate::config::Config;
use crate::plugins;
use crate::printable;
use crate::protocol::{BacklogItem, NewItem};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// The most an item's body may hold, in bytes: a few paragraphs, not a
/// document.
pub const MAX_BODY: usize = 8 * 1024;

/// Whether the backlog is on: the `backlog` plugin.
pub fn enabled(config: &Config) -> bool {
    plugins::enabled(config, "backlog")
}

/// Refuses a command that's only about the backlog while it's off.
pub fn ensure_enabled(config: &Config) -> Result<()> {
    plugins::ensure_enabled(config, "backlog")
}

/// A project's backlog as it's kept: its items, and the number the last one
/// got, which is never given again, not even after an item is removed. The
/// same as JSON is how each project's backlog was kept before the database.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    pub next: u64,
    pub items: Vec<BacklogItem>,
}

impl Store {
    /// Puts `text` on the backlog, with `body` under it, at `now`, and
    /// gives back its number. A `text` of several lines is the item's line
    /// and the start of its body.
    pub fn add(&mut self, text: &str, body: &str, tags: Vec<String>, now: u64) -> Result<u64> {
        let (text, body) = line_and_body(text, body)?;
        // A store from before `next` was kept starts after its last item.
        let last = self.items.iter().map(|item| item.number).max().unwrap_or(0);
        let number = self.next.max(last) + 1;
        self.next = number;
        self.items.push(BacklogItem {
            number,
            text,
            body,
            tags: tidy_tags(&tags),
            done: false,
            created: now,
            closed: None,
        });
        Ok(number)
    }

    /// Changes item `number`: its line, its body and its tags, each that's
    /// given, in place of what it was. A `text` of several lines brings its
    /// body with it.
    pub fn edit(
        &mut self,
        number: u64,
        text: Option<&str>,
        body: Option<&str>,
        tags: Option<Vec<String>>,
    ) -> Result<()> {
        let item = self.item(number)?;
        let (text, body) = match (text, body) {
            (Some(text), body) => {
                let (text, more) = line_and_body(text, body.unwrap_or_default())?;
                // A line of its own leaves the body as it was.
                let keep = body.is_none() && more.is_empty();
                (text, if keep { item.body.clone() } else { more })
            }
            (None, Some(body)) => line_and_body(&item.text, body)?,
            (None, None) => (item.text.clone(), item.body.clone()),
        };
        item.text = text;
        item.body = body;
        if let Some(tags) = tags {
            item.tags = tidy_tags(&tags);
        }
        Ok(())
    }

    /// Marks item `number` done, at `now`, or open again.
    pub fn mark(&mut self, number: u64, done: bool, now: u64) -> Result<()> {
        let item = self.item(number)?;
        item.done = done;
        item.closed = done.then_some(now);
        Ok(())
    }

    pub fn remove(&mut self, number: u64) -> Result<()> {
        self.item(number)?;
        self.items.retain(|item| item.number != number);
        Ok(())
    }

    /// Puts `items` on the backlog, at `now`, those done ticked off, but
    /// for those whose line it has already, done or not, or that came
    /// before in `items`: so importing an export into its own project adds
    /// nothing. Gives back the new items' numbers, and how many were passed
    /// over.
    pub fn import(&mut self, items: &[NewItem], now: u64) -> Result<(Vec<u64>, usize)> {
        let mut added = Vec::new();
        let mut skipped = 0;
        for new in items {
            let (text, _) = line_and_body(&new.text, "")?;
            let there = self.items.iter().any(|item| same_line(&item.text, &text));
            if there {
                skipped += 1;
                continue;
            }
            let number = self.add(&new.text, &new.body, new.tags.clone(), now)?;
            if new.done {
                self.mark(number, true, now)?;
            }
            added.push(number);
        }
        Ok((added, skipped))
    }

    /// The items to show: open ones first, oldest first, then, with `all`,
    /// those done, the latest done first.
    pub fn items(&self, all: bool) -> Vec<BacklogItem> {
        let mut open: Vec<BacklogItem> = self
            .items
            .iter()
            .filter(|item| !item.done)
            .cloned()
            .collect();
        open.sort_by_key(|item| item.number);
        if all {
            let mut done: Vec<BacklogItem> = self
                .items
                .iter()
                .filter(|item| item.done)
                .cloned()
                .collect();
            done.sort_by_key(|item| std::cmp::Reverse(item.closed.unwrap_or(0)));
            open.extend(done);
        }
        open
    }

    pub fn get(&self, number: u64) -> Option<&BacklogItem> {
        self.items.iter().find(|item| item.number == number)
    }

    pub fn open_count(&self) -> usize {
        self.items.iter().filter(|item| !item.done).count()
    }

    fn item(&mut self, number: u64) -> Result<&mut BacklogItem> {
        self.items
            .iter_mut()
            .find(|item| item.number == number)
            .with_context(|| format!("there's no #{number} on the backlog"))
    }
}

/// An item's line and body as they're kept, from what was said of them:
/// without what a terminal would take as an order (an agent may say
/// anything, and the user reads it in a terminal), the line one line, what
/// `text` says after its first line ahead of `body`.
fn line_and_body(text: &str, body: &str) -> Result<(String, String)> {
    let text = printable::text(text);
    let text = text.trim();
    let (line, rest) = text.split_once('\n').unwrap_or((text, ""));
    let line = line.trim();
    if line.is_empty() {
        bail!("say what to put on the backlog");
    }
    let body = printable::text(body);
    let parts: Vec<&str> = [rest.trim(), body.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();
    let body = parts.join("\n\n");
    if body.len() > MAX_BODY {
        bail!(
            "an item's body is {} KiB at most; this one is {} bytes",
            MAX_BODY / 1024,
            body.len()
        );
    }
    Ok((line.to_string(), body))
}

/// Tags as they're kept: each on one line, without the `#` lists write in
/// front of it, none empty and none twice.
fn tidy_tags(tags: &[String]) -> Vec<String> {
    let mut tidy: Vec<String> = Vec::new();
    for tag in tags {
        let tag = printable::line(tag);
        let tag = tag.trim().trim_start_matches('#').trim();
        if !tag.is_empty() && !tidy.iter().any(|kept| kept == tag) {
            tidy.push(tag.to_string());
        }
    }
    tidy
}

/// Whether two items' lines say the same: their words, whatever the case.
fn same_line(a: &str, b: &str) -> bool {
    let words = |text: &str| {
        text.split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    words(a) == words(b)
}

/// What a task started for `item` is asked to do: its line, and its body
/// under it.
pub fn goal(item: &BacklogItem) -> String {
    match item.body.trim() {
        "" => item.text.clone(),
        body => format!("{}\n\n{body}", item.text),
    }
}

/// Whether `item` carries every one of `tags`.
pub fn has_tags(item: &BacklogItem, tags: &[String]) -> bool {
    tags.iter().all(|tag| {
        let tag = tag.trim_start_matches('#');
        item.tags.iter().any(|has| has.eq_ignore_ascii_case(tag))
    })
}

/// The items as markdown checkboxes, the way a README or an issue writes a
/// list of things to do: each one's body indented under it, which
/// [`read_markdown`] reads back.
pub fn markdown(project: &str, items: &[BacklogItem]) -> String {
    let mut text = format!("# {project} backlog\n\n");
    for item in items {
        let mark = if item.done { "x" } else { " " };
        let tags: String = item.tags.iter().map(|tag| format!(" #{tag}")).collect();
        text.push_str(&format!(
            "- [{mark}] {} (#{}){tags}\n",
            item.text, item.number
        ));
        for line in item.body.lines() {
            match line.trim_end() {
                "" => text.push('\n'),
                line => text.push_str(&format!("  {line}\n")),
            }
        }
    }
    text
}

/// The items a markdown list of checkboxes holds, as [`markdown`] writes
/// them or a README's list of things to do does: every `- [ ]` or `- [x]`
/// at the start of a line is an item, done when it's ticked, its `(#12)`
/// left out, the `#tags` at its end its tags (one that starts with a
/// digit, like an issue's `#12`, stays in its line), and the indented lines
/// under it its body. Everything else is passed over.
pub fn read_markdown(text: &str) -> Vec<NewItem> {
    let mut items: Vec<NewItem> = Vec::new();
    let mut body: Vec<&str> = Vec::new();
    // Whether the lines read now are an item's body.
    let mut in_item = false;
    let flush = |items: &mut Vec<NewItem>, body: &mut Vec<&str>| {
        if let Some(item) = items.last_mut() {
            item.body = dedent(body);
        }
        body.clear();
    };
    for line in text.lines() {
        if let Some((done, rest)) = checkbox(line) {
            if in_item {
                flush(&mut items, &mut body);
            }
            let (text, tags) = line_and_tags(rest);
            if !text.is_empty() {
                items.push(NewItem {
                    text,
                    body: String::new(),
                    tags,
                    done,
                });
                in_item = true;
                continue;
            }
        }
        let indented = line.starts_with([' ', '\t']);
        if in_item && (indented || line.trim().is_empty()) {
            body.push(line);
        } else if in_item {
            flush(&mut items, &mut body);
            in_item = false;
        }
    }
    if in_item {
        flush(&mut items, &mut body);
    }
    items
}

/// What follows a checkbox at the start of `line`, and whether it's
/// ticked: `- [x] fix it` is `(true, "fix it")`.
fn checkbox(line: &str) -> Option<(bool, &str)> {
    let rest = line
        .strip_prefix("- [")
        .or_else(|| line.strip_prefix("* ["))?;
    let mut chars = rest.chars();
    let done = match chars.next()? {
        ' ' => false,
        'x' | 'X' => true,
        _ => return None,
    };
    let rest = chars.as_str().strip_prefix(']')?;
    Some((done, rest.trim()))
}

/// An item's line as a list writes it, without its number, `(#12)`, and
/// the `#tags` at its end, and those tags.
fn line_and_tags(text: &str) -> (String, Vec<String>) {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    let mut tags = Vec::new();
    while let Some(word) = words.last() {
        let tag = word
            .strip_prefix('#')
            .filter(|tag| tag.chars().next().is_some_and(char::is_alphabetic));
        let Some(tag) = tag else { break };
        tags.insert(0, tag.to_string());
        words.pop();
    }
    let is_number = |word: &&str| {
        word.strip_prefix("(#")
            .and_then(|word| word.strip_suffix(')'))
            .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
    };
    if words.last().is_some_and(is_number) {
        words.pop();
    }
    (words.join(" "), tags)
}

/// `lines` without the indentation they all share, and the blank lines at
/// either end.
fn dedent(lines: &[&str]) -> String {
    let indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let lines: Vec<&str> = lines
        .iter()
        .map(|line| line.get(indent..).unwrap_or("").trim_end())
        .collect();
    lines.join("\n").trim_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(texts: &[&str]) -> Store {
        let mut store = Store::default();
        for (time, text) in texts.iter().enumerate() {
            store.add(text, "", Vec::new(), time as u64).unwrap();
        }
        store
    }

    fn numbers(items: &[BacklogItem]) -> Vec<u64> {
        items.iter().map(|item| item.number).collect()
    }

    #[test]
    fn items_are_numbered_in_order_and_keep_their_number() {
        let mut store = store_with(&["first", "second"]);
        store.remove(2).unwrap();
        let third = store.add("third", "", Vec::new(), 9).unwrap();
        assert_eq!(third, 3, "a removed item's number isn't used again");
        assert_eq!(numbers(&store.items(false)), [1, 3]);
    }

    #[test]
    fn done_items_leave_the_open_list_and_come_back_when_reopened() {
        let mut store = store_with(&["a", "b", "c"]);
        store.mark(2, true, 50).unwrap();
        assert_eq!(numbers(&store.items(false)), [1, 3]);
        assert_eq!(numbers(&store.items(true)), [1, 3, 2]);
        assert_eq!(store.open_count(), 2);

        store.mark(2, false, 60).unwrap();
        assert_eq!(numbers(&store.items(false)), [1, 2, 3]);
        assert_eq!(store.items(false)[1].closed, None);
    }

    #[test]
    fn the_latest_done_comes_first_among_those_done() {
        let mut store = store_with(&["a", "b"]);
        store.mark(1, true, 10).unwrap();
        store.mark(2, true, 20).unwrap();
        assert_eq!(numbers(&store.items(true)), [2, 1]);
    }

    #[test]
    fn a_missing_item_or_an_empty_text_is_an_error() {
        let mut store = store_with(&["a"]);
        assert!(store.mark(7, true, 0).is_err());
        assert!(store.remove(7).is_err());
        assert!(store.add("   ", "", Vec::new(), 0).is_err());
    }

    #[test]
    fn an_item_is_kept_without_what_a_terminal_would_take_as_an_order() {
        let mut store = Store::default();
        let tags = vec!["ui\x1b[2J".into()];
        store
            .add("fix\x1b]0;pwned\x07 it\r\nlater \u{202e}", "", tags, 0)
            .unwrap();
        let item = &store.items(true)[0];
        assert_eq!(
            (&item.text[..], &item.body[..]),
            ("fix]0;pwned it", "later")
        );
        assert_eq!(item.tags, ["ui[2J"]);
    }

    #[test]
    fn a_text_of_several_lines_is_a_line_and_a_body() {
        let mut store = Store::default();
        store
            .add(
                "  write the docs\nthe guide\n",
                "and the reference",
                Vec::new(),
                0,
            )
            .unwrap();
        let item = store.get(1).unwrap();
        assert_eq!(item.text, "write the docs");
        assert_eq!(item.body, "the guide\n\nand the reference");

        let long = "x".repeat(MAX_BODY + 1);
        let refused = store.add("too long", &long, Vec::new(), 0).unwrap_err();
        assert!(refused.to_string().contains("8 KiB at most"), "{refused}");
    }

    #[test]
    fn an_item_s_goal_is_its_line_and_its_body() {
        let mut store = store_with(&["write the docs"]);
        assert_eq!(goal(store.get(1).unwrap()), "write the docs");
        store.edit(1, None, Some("the guide"), None).unwrap();
        assert_eq!(goal(store.get(1).unwrap()), "write the docs\n\nthe guide");
    }

    #[test]
    fn tags_are_kept_without_their_hash_each_once() {
        let mut store = Store::default();
        let tags = vec!["#ui".into(), " ui ".into(), "".into(), "ci".into()];
        store.add("a", "", tags, 0).unwrap();
        let item = store.get(1).unwrap();
        assert_eq!(item.tags, ["ui", "ci"]);
        assert!(has_tags(item, &["#UI".into()]));
        assert!(has_tags(item, &["ui".into(), "ci".into()]));
        assert!(!has_tags(item, &["ui".into(), "docs".into()]));
    }

    #[test]
    fn an_edit_changes_what_it_says_and_leaves_the_rest() {
        let mut store = Store::default();
        store
            .add("write the docs", "the guide", vec!["docs".into()], 0)
            .unwrap();
        store.edit(1, Some("write the guide"), None, None).unwrap();
        let item = store.get(1).unwrap();
        assert_eq!(
            (&item.text[..], &item.body[..]),
            ("write the guide", "the guide")
        );
        assert_eq!(item.tags, ["docs"]);

        store
            .edit(1, None, Some("all of it"), Some(Vec::new()))
            .unwrap();
        let item = store.get(1).unwrap();
        assert_eq!(
            (&item.text[..], &item.body[..]),
            ("write the guide", "all of it")
        );
        assert!(item.tags.is_empty());

        // A line of several lines brings its body, and an empty body is none.
        store
            .edit(1, Some("ship it\nonce it's green"), None, None)
            .unwrap();
        assert_eq!(store.get(1).unwrap().body, "once it's green");
        store.edit(1, None, Some(""), None).unwrap();
        assert_eq!(store.get(1).unwrap().body, "");

        assert!(store.edit(1, Some(" "), None, None).is_err());
        assert!(store.edit(7, Some("x"), None, None).is_err());
    }

    #[test]
    fn markdown_has_a_checkbox_per_item() {
        let mut store = store_with(&["write the docs", "fix the cart"]);
        store.mark(2, true, 3).unwrap();
        store.add("tagged", "", vec!["ui".into()], 4).unwrap();
        let text = markdown("payments", &store.items(true));
        assert_eq!(
            text,
            "# payments backlog\n\n\
             - [ ] write the docs (#1)\n\
             - [ ] tagged (#3) #ui\n\
             - [x] fix the cart (#2)\n"
        );
    }

    #[test]
    fn an_export_reads_back_as_it_was_and_adds_nothing_to_its_own_project() {
        let mut store = store_with(&["write the docs", "fix the cart"]);
        store
            .edit(
                1,
                None,
                Some("the guide\n\n- and the reference"),
                Some(vec!["docs".into(), "ui".into()]),
            )
            .unwrap();
        store.mark(2, true, 3).unwrap();
        let text = markdown("payments", &store.items(true));
        assert_eq!(
            text,
            "# payments backlog\n\n\
             - [ ] write the docs (#1) #docs #ui\n\
             \x20 the guide\n\
             \n\
             \x20 - and the reference\n\
             - [x] fix the cart (#2)\n"
        );
        let read = read_markdown(&text);
        assert_eq!(
            read,
            [
                NewItem {
                    text: "write the docs".into(),
                    body: "the guide\n\n- and the reference".into(),
                    tags: vec!["docs".into(), "ui".into()],
                    done: false,
                },
                NewItem {
                    text: "fix the cart".into(),
                    body: String::new(),
                    tags: Vec::new(),
                    done: true,
                },
            ]
        );
        assert_eq!(store.import(&read, 9).unwrap(), (Vec::new(), 2));

        let mut other = Store::default();
        assert_eq!(other.import(&read, 9).unwrap(), (vec![1, 2], 0));
        assert_eq!(other.items(true), {
            let mut items = store.items(true);
            for item in &mut items {
                item.created = 9;
                item.closed = item.closed.map(|_| 9);
            }
            items
        });
    }

    #[test]
    fn a_readme_s_list_of_things_to_do_reads_as_items() {
        let text = "# TODO\n\nSome prose.\n\n\
                    - [ ] Fix bug #12 #payments\n\
                    * [X] Ship it\n\
                    \x20 - [ ] a nested one is its body\n\
                    - plain, not an item\n\
                    \x20 nor is this\n\
                    - [ ] (#4)\n\
                    - [?] not a box\n\
                    - [ ] fix bug #12 \n";
        let read = read_markdown(text);
        let lines: Vec<(&str, &str, bool)> = read
            .iter()
            .map(|item| (&item.text[..], &item.body[..], item.done))
            .collect();
        assert_eq!(
            lines,
            [
                ("Fix bug #12", "", false),
                ("Ship it", "- [ ] a nested one is its body", true),
                ("fix bug #12", "", false),
            ]
        );
        assert_eq!(read[0].tags, ["payments"]);

        // The same line twice is one item.
        let mut store = Store::default();
        assert_eq!(store.import(&read, 0).unwrap(), (vec![1, 2], 1));
    }

    #[test]
    fn an_import_that_fails_part_way_is_an_error() {
        let mut store = Store::default();
        let items = [NewItem {
            text: "fine".into(),
            body: "x".repeat(MAX_BODY + 1),
            tags: Vec::new(),
            done: false,
        }];
        assert!(store.import(&items, 0).is_err());
    }
}
