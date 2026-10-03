//! Made-up names for new worktrees' branches, like `brave-otter`: short,
//! easy to say and to tell apart, and nothing to do with the task, which
//! can change while the branch can't.

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
    fn names_are_picked_afresh_each_time() {
        let names: HashSet<String> = (0..20).map(|_| random()).collect();
        assert!(names.len() > 1);
    }
}
