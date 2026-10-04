//! Names: made-up ones for new worktrees' branches, like `brave-otter`:
//! short, easy to say and to tell apart, and nothing to do with the task,
//! which can change while the branch can't; and a session's, from the
//! first thing it's asked, like `fix-login-redirect`, or from the name
//! Claude Code gave its conversation, which a session can change.

use std::hash::{BuildHasher, RandomState};
use std::time::SystemTime;

const ADJECTIVES: [&str; 64] = [
    "amber", "bold", "brave", "bright", "brisk", "calm", "clever", "cosmic", "cozy", "crisp",
    "curious", "dapper", "eager", "fancy", "fearless", "fluffy", "gentle", "giddy", "glad",
    "golden", "grand", "happy", "hidden", "humble", "jolly", "keen", "kind", "lively", "lucky",
    "mellow", "merry", "mighty", "misty", "nimble", "noble", "plucky", "polished", "proud",
    "quick", "quiet", "rapid", "rosy", "rustic", "shiny", "silent", "silver", "sleek", "snappy",
    "solid", "spry", "steady", "stellar", "sunny", "swift", "tidy", "tranquil", "vivid", "warm",
    "wild", "wise", "witty", "zany", "zesty", "breezy",
];

const ANIMALS: [&str; 64] = [
    "badger", "beaver", "bison", "cheetah", "condor", "coyote", "crane", "dingo", "dolphin",
    "eagle", "falcon", "ferret", "finch", "fox", "gecko", "heron", "hippo", "ibis", "jackal",
    "jaguar", "koala", "lemur", "lynx", "magpie", "marmot", "mink", "moose", "narwhal", "newt",
    "ocelot", "otter", "owl", "panda", "panther", "parrot", "pelican", "penguin", "puffin",
    "quail", "rabbit", "raven", "robin", "salmon", "seal", "shark", "sloth", "sparrow", "squid",
    "stork", "swan", "tapir", "tiger", "toucan", "turtle", "walrus", "weasel", "whale", "wombat",
    "wren", "yak", "zebra", "alpaca", "bobcat", "osprey",
];

/// The most words a name from a prompt takes.
const PROMPT_WORDS: usize = 3;

/// The longest a name from a prompt gets, in characters.
const PROMPT_LONGEST: usize = 30;

/// The longest a name from a conversation's name gets, in characters.
const TITLE_LONGEST: usize = 40;

/// Words that say little of what a prompt asks: asking nicely, who's to do
/// it, and the small words between the ones that matter.
const FILLER: &[&str] = &[
    "a", "about", "after", "all", "also", "am", "an", "and", "any", "are", "as", "at", "be",
    "before", "but", "by", "can", "could", "do", "does", "for", "from", "go", "have", "hello",
    "help", "hey", "hi", "how", "i", "if", "in", "into", "is", "it", "its", "just", "let", "lets",
    "look", "me", "my", "need", "now", "of", "ok", "okay", "on", "or", "our", "please", "should",
    "so", "some", "that", "the", "their", "them", "then", "there", "these", "this", "those", "to",
    "up", "us", "want", "was", "we", "what", "when", "where", "which", "why", "will", "with",
    "would", "you", "your",
];

/// A session's name from `prompt`, the first thing it was asked: its first
/// few words that say what it's about, in lower case, joined by dashes.
/// `None` for a prompt with no such words, or a slash command, which asks
/// the agent itself for something.
pub fn from_prompt(prompt: &str) -> Option<String> {
    let prompt = prompt.trim_start();
    if prompt.starts_with('/') {
        return None;
    }
    let mut name = String::new();
    // An apostrophe joins a word rather than ending it: `don't`, `user's`.
    let words = prompt
        .split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’')
        .map(|word| word.replace(['\'', '’'], "").to_lowercase())
        .filter(|word| !word.is_empty() && !FILLER.contains(&word.as_str()));
    for word in words.take(PROMPT_WORDS) {
        let longer = name.chars().count() + word.chars().count() + 1;
        if !name.is_empty() && longer > PROMPT_LONGEST {
            break;
        }
        if !name.is_empty() {
            name.push('-');
        }
        name.push_str(&word);
    }
    let name: String = name.chars().take(PROMPT_LONGEST).collect();
    (!name.is_empty()).then_some(name)
}

/// A session's name from `title`, the name Claude Code gave its
/// conversation: every word of it, in lower case, joined by dashes, as a
/// session's name has no spaces. `None` for one with no words.
pub fn from_title(title: &str) -> Option<String> {
    let words = title
        .split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’')
        .map(|word| word.replace(['\'', '’'], "").to_lowercase())
        .filter(|word| !word.is_empty());
    let mut name = String::new();
    for word in words {
        let longer = name.chars().count() + word.chars().count() + 1;
        if !name.is_empty() && longer > TITLE_LONGEST {
            break;
        }
        if !name.is_empty() {
            name.push('-');
        }
        name.push_str(&word);
    }
    let name: String = name.chars().take(TITLE_LONGEST).collect();
    (!name.is_empty()).then_some(name)
}

/// A new made-up name, picked at random.
pub fn random() -> String {
    // std's hash keys are random for each process, and differ for each
    // `RandomState` made in it, so this is a new number every time.
    name(RandomState::new().hash_one(SystemTime::now()))
}

/// The name `number` picks: an adjective, then an animal.
fn name(number: u64) -> String {
    let count = ADJECTIVES.len() as u64;
    let adjective = ADJECTIVES[(number % count) as usize];
    let animal = ANIMALS[(number / count % ANIMALS.len() as u64) as usize];
    format!("{adjective}-{animal}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_conversation_s_name_keeps_every_word_joined_by_dashes() {
        assert_eq!(
            from_title("Fix the refund rounding").as_deref(),
            Some("fix-the-refund-rounding")
        );
        assert_eq!(
            from_title("API v2: don't break it").as_deref(),
            Some("api-v2-dont-break-it")
        );
        assert_eq!(
            from_title("payments-refactor").as_deref(),
            Some("payments-refactor")
        );
        assert_eq!(from_title(" — ").as_deref(), None);
        let long = from_title(&"word ".repeat(20)).unwrap();
        assert!(long.chars().count() <= TITLE_LONGEST, "{long}");
    }

    #[test]
    fn the_words_are_plain_and_each_is_there_once() {
        for words in [&ADJECTIVES, &ANIMALS] {
            assert!(
                words
                    .iter()
                    .all(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase()))
            );
            assert_eq!(words.iter().collect::<HashSet<_>>().len(), words.len());
        }
    }

    #[test]
    fn every_pair_of_words_can_be_picked() {
        let names: HashSet<String> = (0..64 * 64).map(name).collect();
        assert_eq!(names.len(), ADJECTIVES.len() * ANIMALS.len());
        assert_eq!(name(0), "amber-badger");
        assert_eq!(name(1), "bold-badger");
        assert_eq!(name(64), "amber-beaver");
    }

    #[test]
    fn a_prompt_names_a_session_by_its_first_words_that_matter() {
        assert_eq!(
            from_prompt("Fix the login redirect after OAuth").as_deref(),
            Some("fix-login-redirect")
        );
        assert_eq!(
            from_prompt("Can you please review the diff on this branch for bugs?").as_deref(),
            Some("review-diff-branch")
        );
        assert_eq!(
            from_prompt("  don't retry #412\nmore").as_deref(),
            Some("dont-retry-412")
        );
        assert_eq!(from_prompt("Résumé café").as_deref(), Some("résumé-café"));
    }

    #[test]
    fn a_name_from_a_prompt_is_kept_short() {
        let name = from_prompt("internationalization localization accessibility").unwrap();
        assert_eq!(name, "internationalization");
        let one_long_word = "x".repeat(50);
        assert_eq!(from_prompt(&one_long_word).unwrap().len(), PROMPT_LONGEST);
    }

    #[test]
    fn a_prompt_with_nothing_to_go_on_names_nothing() {
        assert_eq!(from_prompt(""), None);
        assert_eq!(from_prompt("can you?"), None);
        assert_eq!(from_prompt("/compact keep the tests"), None);
    }

    #[test]
    fn names_are_picked_afresh_each_time() {
        let names: HashSet<String> = (0..20).map(|_| random()).collect();
        assert!(names.len() > 1);
    }
}
