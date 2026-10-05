//! Whether what an agent said as its turn ended asks the user something:
//! Claude Code's Stop hook gives the turn's last message, and a turn that
//! ends with a task open only pins the session for the user, and tells
//! them, when it does. An agent waiting on its tests, CI or a helper says
//! so ("I'm waiting on CI, not on you"), and nothing needs the user then.
//!
//! It goes by the words, with no model, and errs towards asking: a question
//! or a request anywhere in what it said is asking; a plain statement at
//! its end that it waits on something else isn't; anything else is unclear,
//! which a turn with nothing of its own still running takes for asking, so
//! the user is never left unalerted when the agent does ask. Measured on
//! the turns crystal had marked waiting on the user, every one of which
//! waited on something else, and on the turns a person answered next.
//!
//! Pure, so it's unit-tested.

use regex::{Regex, RegexSet};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// What the last message of a turn says of the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Said {
    /// It asks them something: a question, a choice, an approval, a file,
    /// access, or something only they can do.
    Asks,
    /// It says it waits on something else: its tests, CI, a helper.
    Waits,
    /// Neither, like a summary of what was done.
    Unclear,
}

/// Requests of the user, anywhere in what was said.
const ASKS: &[&str] = &[
    r"\blet me know\b",
    r"\b(should|shall|can|may) i\b",
    r"\bdo you (want|prefer|need|mean|agree)\b",
    r"\bwould you (like|prefer|rather)\b",
    r"\bwant me to\b",
    r"\byour call\b",
    r"\bup to you\b",
    r"\bplease (confirm|choose|pick|decide|approve|review|answer|tell|let|share|send|provide|run|grant|add|check|merge|install|restart|log ?in|sign in|reply)\b",
    r"\b(confirm|approve|choose|pick|decide) (whether|which|if|one|between)\b",
    r"\bwhich (one|option|of these|way|approach|do you)\b",
    r"\b(can|could|would) you\b",
    r"\bi need (you|your|access|permission)\b",
    r"\bneeds? your\b",
    r"\bwaiting (on|for) (you|your|the user)\b",
    r"\byour (answer|decision|go-ahead|approval|input|confirmation|ok)\b",
    r"\bif you('d| would)? (like|want|prefer)\b",
    r"\byou('ll| will)? (need|have) to\b",
    r"\byou can (run|merge|approve|try)\b",
    r"\byourself\b",
    r"\bpermission rule\b",
];

/// Statements that it waits on something other than the user, looked for
/// at the end of what was said.
const WAITS: &[&str] = &[
    r"\bnot (waiting )?(on|for) (you|the user)\b",
    r"\bnothing (is )?(needed )?from you\b",
    // On anything but `you`, `your` or `the user`.
    r"\bwaiting (on|for) (?:[^yt]|t[^h]|th[^e]|the[^ ]|the [^u]|y[^o])",
    r"\bin the background\b",
    r"\bstill (running|working|going|being built|in progress)\b",
    r"\b(will|'ll) (wake|notify|tell|ping) me\b",
    r"\bnotif(y|ies) me\b",
    r"\bi('ll| will) be (notified|woken)\b",
    r"\b(when|once|as soon as|after) (it|they|that|this|both|each|the [\w' -]+?|its [\w' -]+?) (finish|finishes|finished|complete|completes|pass|passes|end|ends|report|reports|come back|comes back|arrive|arrives|is done|are done|is green|are green|'s green|goes green|lands|land)\b",
    r"\bi('ll| will) (pick (it |this )?up|carry on|continue|resume|come back)\b",
    r"\bonce (it'?s|they'?re|that'?s|the \w+ (is|are)) (green|done|finished|back|in)\b",
];

/// Saying it isn't waiting on the user, which mustn't read as saying it is.
static NOT_ON_YOU: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(not|n't|never) (only |just )?(waiting |be waiting |be )?(on|for) (you|the user)\b",
    )
    .expect("a valid pattern")
});

/// A sentence ending with a question mark, past what may close it.
static QUESTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\?[\s"')\]*_]*($|\n|\s+[A-Z])"#).expect("a valid pattern"));

/// Code, which says nothing to the user: fenced blocks, and what's in
/// backticks, kept as a mark that something was there.
static CODE: LazyLock<(Regex, Regex)> = LazyLock::new(|| {
    (
        Regex::new(r"(?s)```.*?```").expect("a valid pattern"),
        Regex::new(r"`[^`\n]*`").expect("a valid pattern"),
    )
});

static ASKS_SET: LazyLock<RegexSet> = LazyLock::new(|| set(ASKS));
static WAITS_SET: LazyLock<RegexSet> = LazyLock::new(|| set(WAITS));

fn set(patterns: &[&str]) -> RegexSet {
    let patterns = patterns.iter().map(|pattern| format!("(?i){pattern}"));
    RegexSet::new(patterns).expect("valid patterns")
}

/// What `text`, the last message of a turn, says of the user.
pub fn judge(text: &str) -> Said {
    let text = text.trim().replace('\u{2019}', "'");
    let (fenced, inline) = &*CODE;
    let prose = fenced.replace_all(&text, " ");
    let prose = inline.replace_all(&prose, "`x`");
    if prose.trim().is_empty() {
        return Said::Unclear;
    }
    // A command for the user to run here (`! gh pr merge`) asks, though
    // it's in backticks.
    let commands = text.contains("`!");
    if QUESTION.is_match(&prose) || commands {
        return Said::Asks;
    }
    let unasked = NOT_ON_YOU.replace_all(&prose, " ");
    if ASKS_SET.is_match(&unasked) {
        return Said::Asks;
    }
    if WAITS_SET.is_match(end_of(&prose)) {
        return Said::Waits;
    }
    Said::Unclear
}

/// The end of `prose`, where what it waits on is said: its last
/// paragraph, and the one before when that's only a few words.
fn end_of(prose: &str) -> &str {
    let prose = prose.trim_end();
    let mut breaks = paragraph_breaks(prose);
    let Some(last) = breaks.pop() else {
        return prose;
    };
    if prose.len() - last >= 80 {
        return &prose[last..];
    }
    let before = breaks.pop().unwrap_or(0);
    &prose[before..]
}

/// Where each paragraph after the first starts in `text`.
fn paragraph_breaks(text: &str) -> Vec<usize> {
    let mut breaks = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find("\n\n") {
        let start = at + found + 2;
        if !text[start..].trim().is_empty() {
            breaks.push(start);
        }
        at = start;
    }
    breaks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The last messages of turns crystal marked as waiting on the user,
    /// from its own event log: none of them asks anything.
    const WAITED_ON_SOMETHING_ELSE: &[&str] = &[
        "The task isn't finished yet. I'm waiting for the two comparison agents, one for herdr \
         and one for docket, so I'm leaving it open until their results come in.",
        "I've checked the herdr results against the code, and they hold up. The docket \
         comparison is still running, so the task stays open until it comes back.",
        "The task stays open. I'm not waiting on you; I'm waiting on the two helpers working on \
         items 3+6 and item 5. When they report back, I'll merge their branches, run the full \
         lint and test suite, and open and merge the PR.",
        "I'm not done yet, so I'm leaving the task open. The last part, agent detection rule \
         files with `crystal agent explain`, is still being built by a background agent. When \
         it finishes I'll merge it, run lint and the full tests, then close the task.",
        "Master is merged in and green. Items 4 and 5 are on `wild-koala`; the items 3+6 helper \
         is still working. Once it reports, I'll merge its branch, run the whole suite again, \
         then open and merge the PR.",
        "Notes saved. Waiting on CI for PR #58 (msrv, macOS, Ubuntu); I'll merge when it's green.",
        "msrv passed. Still waiting on the macOS and Ubuntu test jobs.",
        "I'll merge it as soon as the macOS and Ubuntu jobs pass; msrv already has.",
        "I'm waiting on the full test run (not on you); the task stays open until the PR is up.",
        "The task stays open: I'm not waiting on you, only on the full test suite, which is \
         still running in the background. When it passes I'll commit, push, open the PR and \
         close the task.",
        "The task is still open while I wait on the full test run, not on the user; I'll pick \
         up when it finishes.",
        "I'm waiting for the test suite to finish. The Monitor and the background wait will \
         both tell me when it does, and then I'll push and open the PR.",
        "The task isn't finished, so I'm leaving it open. `make lint` passed, and the full suite \
         (`cargo test -- --test-threads=4`) is still running in the background. I'll be \
         notified when it ends. If it's green I'll commit, push, open the PR and run `crystal \
         done`. If something fails, I'll rerun it alone first to rule out a timeout under load.",
        "PR #118 is open. Its CI (macOS, Ubuntu and the minimum Rust version) is still running, \
         so the task stays open until the Ubuntu build confirms the Linux reader, which I only \
         read and never compiled. I'm waiting on CI, not on you.",
    ];

    #[test]
    fn a_turn_waiting_on_something_else_doesn_t_ask() {
        for said in WAITED_ON_SOMETHING_ELSE {
            assert_eq!(judge(said), Said::Waits, "{said}");
        }
    }

    #[test]
    fn a_question_or_a_request_asks_wherever_it_is() {
        let asking = [
            "Tests pass. Should I open the PR?",
            "Two ways to do this: keep the old flag, or drop it. Which do you prefer?",
            "The migration is ready. Let me know when the database is backed up.",
            "I need access to the staging bucket to go on.",
            "Do you want me to merge it?\n\nCI is still running in the background.",
            "I didn't merge #57: the permission check blocked it. To merge it yourself, run \
             `! gh pr ready 57 && gh pr merge 57 --squash` here.",
            "Could you send me the failing log?",
            "**Which one should I keep?**",
            "I'm waiting on your answer before I go on.",
            "Pick one: `a`, `b` or `c`.\n\nThe tests are still running in the background.",
        ];
        for said in asking {
            assert_eq!(judge(said), Said::Asks, "{said}");
        }
    }

    #[test]
    fn saying_it_isn_t_waiting_on_the_user_isn_t_asking_them() {
        assert_eq!(
            judge("I'm waiting on that run, not on you; it notifies me when it finishes."),
            Said::Waits
        );
        assert_eq!(judge("Nothing is needed from you."), Said::Waits);
    }

    #[test]
    fn what_s_neither_is_unclear() {
        let unclear = [
            "The fix is now in `master`. I committed it, opened PR #54 and squash-merged it.",
            "Ubuntu passed too. Only macOS is left.",
            "Done.",
            "",
            "```\ncargo test\n```",
        ];
        for said in unclear {
            assert_eq!(judge(said), Said::Unclear, "{said:?}");
        }
    }

    #[test]
    fn code_says_nothing_to_the_user() {
        // A question mark in code isn't a question.
        assert_eq!(
            judge("The `?` overlay is paged now; the suite is still running in the background."),
            Said::Waits
        );
        assert_eq!(
            judge("```rust\nlet x = y?;\n```\nI'll pick up when the tests finish."),
            Said::Waits
        );
    }

    #[test]
    fn what_it_waits_on_is_looked_for_at_the_end() {
        // A long report that says something ran in the background early on,
        // and ends with no word of waiting, is unclear.
        let report = "The suite ran in the background and passed.\n\n\
                      The README points at the new pages now, and links from outside to an \
                      old anchor land at the top of the README.";
        assert_eq!(judge(report), Said::Unclear);
        // A short last paragraph is read with the one before it.
        let short = "The full suite is still running in the background.\n\nThen the PR.";
        assert_eq!(judge(short), Said::Waits);
    }
}
