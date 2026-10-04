//! Finding a session by typing a little of it, for `/` in the sidebar.
//!
//! A query matches when its letters turn up in order, not necessarily side
//! by side, ignoring case: `rfx` finds `refund-fix`. Each word of the query
//! has to turn up in the session's name, project, branch or command, so
//! `pay fix` finds a fixer in the payments project. Where a word turns up
//! in the name, those letters are marked, so the eye can see why it
//! matched.

use crate::protocol::SessionInfo;

/// Whether `session` matches `query`, and if it does, which characters of
/// its name to mark, counted from 0. An empty query matches everything.
pub fn session_match(query: &str, session: &SessionInfo) -> Option<Vec<usize>> {
    let mut marked = Vec::new();
    for word in query.split_whitespace() {
        if let Some(found) = letters_in(word, &session.name) {
            marked.extend(found);
        } else if !others(session)
            .iter()
            .any(|text| letters_in(word, text).is_some())
        {
            return None;
        }
    }
    marked.sort_unstable();
    marked.dedup();
    Some(marked)
}

/// Where `word`'s letters turn up in `text`, ignoring case: the characters'
/// places in `text`, or `None` when they don't all turn up. Letters side by
/// side are found first, so `fix` marks the end of `refund-fix`, not its
/// first `f`; failing that, letters in order with others between them.
pub fn letters_in(word: &str, text: &str) -> Option<Vec<usize>> {
    side_by_side(word, text).or_else(|| in_order(word, text))
}

/// Where `word` turns up whole in `text`, ignoring case.
fn side_by_side(word: &str, text: &str) -> Option<Vec<usize>> {
    let word: Vec<char> = word.chars().collect();
    let text: Vec<char> = text.chars().collect();
    if word.is_empty() || word.len() > text.len() {
        return None;
    }
    let start = (0..=text.len() - word.len()).find(|&start| {
        let here = &text[start..start + word.len()];
        here.iter().zip(&word).all(|(a, b)| same_letter(*a, *b))
    })?;
    Some((start..start + word.len()).collect())
}

/// Where `word`'s letters turn up in `text`, in order, each as early as it
/// can.
fn in_order(word: &str, text: &str) -> Option<Vec<usize>> {
    let mut found = Vec::new();
    let mut letters = text.chars().enumerate();
    for wanted in word.chars() {
        let (place, _) = letters.find(|(_, letter)| same_letter(*letter, wanted))?;
        found.push(place);
    }
    Some(found)
}

/// What else a session can be found by: its project, its branch and its
/// command.
fn others(session: &SessionInfo) -> Vec<String> {
    let mut texts = vec![session.command.join(" ")];
    if let Some(worktree) = &session.worktree {
        texts.push(worktree.project.clone());
        if let Some(branch) = &worktree.branch {
            texts.push(branch.clone());
        }
    }
    texts
}

fn same_letter(a: char, b: char) -> bool {
    a.to_lowercase().eq(b.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{State, Worktree};
    use std::path::PathBuf;

    fn session(name: &str, project: &str, branch: &str, command: &str) -> SessionInfo {
        SessionInfo {
            stopped_idle: false,
            front: None,
            name: name.into(),
            id: name.into(),
            command: command.split(' ').map(String::from).collect(),
            cwd: PathBuf::from("/code"),
            pid: Some(1),
            state: State::Running,
            activity: None,
            worktree: Some(Worktree {
                project: project.into(),
                project_path: PathBuf::from(format!("/code/{project}")),
                path: PathBuf::from(format!("/code/{project}")),
                main: true,
                branch: Some(branch.into()),
                in_progress: None,
            }),
            changed: 0,
            task: None,
            asking: None,
            reporter: None,
            subagents: 0,
            bell: false,
        }
    }

    #[test]
    fn letters_turn_up_in_order_whatever_their_case() {
        assert_eq!(letters_in("rfx", "refund-fix"), Some(vec![0, 2, 9]));
        assert_eq!(letters_in("RF", "refund-fix"), Some(vec![0, 2]));
        assert_eq!(letters_in("xr", "refund-fix"), None);
    }

    #[test]
    fn letters_side_by_side_are_found_before_scattered_ones() {
        assert_eq!(letters_in("fix", "refund-fix"), Some(vec![7, 8, 9]));
        assert_eq!(letters_in("FUND", "refund-fix"), Some(vec![2, 3, 4, 5]));
    }

    #[test]
    fn a_match_in_the_name_marks_its_letters() {
        let fixer = session("refund-fix", "payments", "main", "claude");
        assert_eq!(session_match("fix", &fixer), Some(vec![7, 8, 9]));
    }

    #[test]
    fn a_session_is_found_by_its_project_branch_or_command_too() {
        let fixer = session("refund-fix", "payments", "feat/ledger", "claude");
        assert_eq!(session_match("pay", &fixer), Some(vec![]));
        assert_eq!(session_match("ledger", &fixer), Some(vec![]));
        assert_eq!(session_match("claude", &fixer), Some(vec![]));
        assert_eq!(session_match("codex", &fixer), None);
    }

    #[test]
    fn every_word_has_to_turn_up_somewhere() {
        let fixer = session("refund-fix", "payments", "main", "claude");
        assert_eq!(session_match("pay fix", &fixer), Some(vec![7, 8, 9]));
        assert_eq!(session_match("pay codex", &fixer), None);
    }

    #[test]
    fn an_empty_query_matches_everything() {
        let fixer = session("refund-fix", "payments", "main", "claude");
        assert_eq!(session_match("  ", &fixer), Some(vec![]));
    }
}
