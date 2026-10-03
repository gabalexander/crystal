//! Finding a file from a few letters of its path, the way an editor's quick
//! open does: `rfnd` finds `src/billing/refund.rs`.
//!
//! A path matches when every letter typed is in it, in order, ignoring
//! case. Its score says how well, by a few plain rules:
//! - every letter matched is worth a point;
//! - a letter in the file's own name, after the last `/`, is worth more
//!   than one in a directory, since the name is what people remember;
//! - a letter that starts a word (after `/`, `_`, `-`, `.` or a space, or a
//!   capital after a small letter) is worth more, since people type the
//!   first letters of words;
//! - a letter right after the one before it is worth more, so that `ref`
//!   prefers `refund.rs` to `r_e_f.rs`.
//!
//! Letters are matched from the end of the path backwards, each as far
//! right as it can be, which puts them in the file's name when they can be.
//! Between two paths that score the same, the shorter comes first.

/// How well a path matched, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub score: i64,
    /// The places of the matched letters in the path, counted in
    /// characters, from left to right: what gets highlighted.
    pub positions: Vec<usize>,
}

const IN_NAME: i64 = 4;
const WORD_START: i64 = 6;
/// The most: letters typed together are most often letters that are
/// together.
const AFTER_THE_LAST: i64 = 8;

/// How well `path` matches `query`, or `None` when it doesn't. Spaces in
/// the query don't count.
pub fn score(path: &str, query: &str) -> Option<Match> {
    let chars: Vec<char> = path.chars().collect();
    let wanted: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let positions = match_from_the_end(&chars, &wanted)?;

    let name_starts = chars.iter().rposition(|c| *c == '/').map_or(0, |at| at + 1);
    let mut score = 0;
    for (index, &at) in positions.iter().enumerate() {
        score += 1;
        if at >= name_starts {
            score += IN_NAME;
        }
        if starts_a_word(&chars, at) {
            score += WORD_START;
        }
        if index > 0 && positions[index - 1] + 1 == at {
            score += AFTER_THE_LAST;
        }
    }
    Some(Match { score, positions })
}

/// The paths that match `query`, best first, as their places in `paths`
/// with how they matched: no more than `limit` of them. With no query,
/// every path matches, in the order given.
pub fn filter(paths: &[String], query: &str, limit: usize) -> Vec<(usize, Match)> {
    if query.trim().is_empty() {
        let unmatched = Match {
            score: 0,
            positions: Vec::new(),
        };
        return (0..paths.len().min(limit))
            .map(|index| (index, unmatched.clone()))
            .collect();
    }
    let mut matches: Vec<(usize, Match)> = paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| Some((index, score(path, query)?)))
        .collect();
    matches.sort_by(|(a, a_match), (b, b_match)| {
        b_match
            .score
            .cmp(&a_match.score)
            .then(paths[*a].len().cmp(&paths[*b].len()))
            .then(paths[*a].cmp(&paths[*b]))
    });
    matches.truncate(limit);
    matches
}

/// Where each of `wanted` is in `chars`, matched from the end backwards,
/// each letter as far right as it can be and still leave room for the
/// letters before it. `None` when one isn't there.
fn match_from_the_end(chars: &[char], wanted: &[char]) -> Option<Vec<usize>> {
    let mut positions = Vec::with_capacity(wanted.len());
    let mut before = chars.len();
    for letter in wanted.iter().rev() {
        let at = chars[..before]
            .iter()
            .rposition(|c| c.to_lowercase().eq(std::iter::once(*letter)))?;
        positions.push(at);
        before = at;
    }
    positions.reverse();
    Some(positions)
}

/// Whether the character at `at` starts a word of the path.
fn starts_a_word(chars: &[char], at: usize) -> bool {
    let Some(&before) = at.checked_sub(1).and_then(|before| chars.get(before)) else {
        return true;
    };
    let separator = matches!(before, '/' | '_' | '-' | '.' | ' ');
    let capital_after_small = before.is_lowercase() && chars[at].is_uppercase();
    separator || capital_after_small
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn best(names: &[&str], query: &str) -> Vec<String> {
        let paths = paths(names);
        filter(&paths, query, 10)
            .into_iter()
            .map(|(index, _)| paths[index].clone())
            .collect()
    }

    #[test]
    fn letters_must_all_be_there_in_order() {
        assert!(score("src/refund.rs", "rfnd").is_some());
        assert!(score("src/refund.rs", "dnfr").is_none());
        assert!(score("src/refund.rs", "rfx").is_none());
    }

    #[test]
    fn case_and_spaces_dont_matter() {
        assert!(score("src/Refund.rs", "REF").is_some());
        assert!(score("src/refund.rs", "ref und").is_some());
    }

    #[test]
    fn letters_land_in_the_file_name_when_they_can() {
        let found = score("src/app/apple.rs", "app").unwrap();
        // `apple`, not the `app` directory.
        assert_eq!(found.positions, [8, 9, 10]);
    }

    #[test]
    fn a_match_in_the_name_beats_one_in_the_directories() {
        let found = best(&["refund/main.rs", "src/refund.rs"], "refund");
        assert_eq!(found[0], "src/refund.rs");
    }

    #[test]
    fn word_starts_beat_letters_in_the_middle() {
        let found = best(&["src/gearbox.rs", "src/get_bytes.rs"], "gb");
        assert_eq!(found[0], "src/get_bytes.rs");
    }

    #[test]
    fn letters_together_beat_letters_apart() {
        let found = best(&["r_e_f.rs", "ref.rs"], "ref");
        assert_eq!(found[0], "ref.rs");
    }

    #[test]
    fn a_tie_goes_to_the_shorter_path() {
        let found = best(&["src/deep/refund.rs", "src/refund.rs"], "refund");
        assert_eq!(found[0], "src/refund.rs");
    }

    #[test]
    fn no_query_keeps_every_path_in_its_order() {
        let found = best(&["b.rs", "a.rs"], "");
        assert_eq!(found, ["b.rs", "a.rs"]);
    }

    #[test]
    fn the_limit_keeps_the_best() {
        let paths = paths(&["a1", "a2", "a3"]);
        assert_eq!(filter(&paths, "a", 2).len(), 2);
    }
}
