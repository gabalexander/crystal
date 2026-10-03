//! A project's backlog: things worth doing later that aren't anyone's task
//! yet. The daemon keeps one per project, in its database, and it's the only
//! one that writes it.
//!
//! Items are numbered per project, #1 on, and keep their number: it's how
//! a person or an agent names one, and how a task started for an item says
//! which item closing it ticks.
//!
//! Everything the backlog adds to crystal goes through [`enabled`], so it
//! can be switched off as one.

use crate::config::Config;
use crate::plugins;
use crate::protocol::BacklogItem;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

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
    /// Puts `text` on the backlog, at `now`, and gives back its number.
    pub fn add(&mut self, text: &str, tags: Vec<String>, now: u64) -> Result<u64> {
        let text = text.trim();
        if text.is_empty() {
            bail!("say what to put on the backlog");
        }
        // A store from before `next` was kept starts after its last item.
        let last = self.items.iter().map(|item| item.number).max().unwrap_or(0);
        let number = self.next.max(last) + 1;
        self.next = number;
        self.items.push(BacklogItem {
            number,
            text: text.to_string(),
            tags,
            done: false,
            created: now,
            closed: None,
        });
        Ok(number)
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

/// The items as markdown checkboxes, the way a README or an issue writes a
/// list of things to do.
pub fn markdown(project: &str, items: &[BacklogItem]) -> String {
    let mut text = format!("# {project} backlog\n\n");
    for item in items {
        let mark = if item.done { "x" } else { " " };
        let tags: String = item.tags.iter().map(|tag| format!(" #{tag}")).collect();
        text.push_str(&format!(
            "- [{mark}] {} (#{}){tags}\n",
            item.text, item.number
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(texts: &[&str]) -> Store {
        let mut store = Store::default();
        for (time, text) in texts.iter().enumerate() {
            store.add(text, Vec::new(), time as u64).unwrap();
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
        let third = store.add("third", Vec::new(), 9).unwrap();
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
        assert!(store.add("   ", Vec::new(), 0).is_err());
    }

    #[test]
    fn markdown_has_a_checkbox_per_item() {
        let mut store = store_with(&["write the docs", "fix the cart"]);
        store.mark(2, true, 3).unwrap();
        store.add("tagged", vec!["ui".into()], 4).unwrap();
        let text = markdown("payments", &store.items(true));
        assert_eq!(
            text,
            "# payments backlog\n\n\
             - [ ] write the docs (#1)\n\
             - [ ] tagged (#3) #ui\n\
             - [x] fix the cart (#2)\n"
        );
    }
}
