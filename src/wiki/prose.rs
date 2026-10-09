//! The links and the code spans in a wiki's markdown, found where
//! CommonMark finds them (never in a fenced block, a link's code span
//! counted as the link's), each with where it is in the text, so that the
//! checks and the linker can change one without touching the rest.

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use std::ops::Range;

/// An inline link: `[label](dest)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Where it is in the text, brackets and all.
    pub range: Range<usize>,
    /// Its label as written, code spans and all.
    pub label: String,
    pub dest: String,
}

/// A code span outside any link: `` `like_this` ``.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    /// Where it is in the text, backticks and all.
    pub range: Range<usize>,
    /// What's between the backticks.
    pub text: String,
}

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// The inline links in `md`, in order.
pub fn links(md: &str) -> Vec<Link> {
    let mut found = Vec::new();
    for (event, range) in Parser::new_ext(md, options()).into_offset_iter() {
        if let Event::Start(Tag::Link {
            link_type: LinkType::Inline,
            dest_url,
            ..
        }) = event
        {
            let source = &md[range.clone()];
            let label = match source.rfind("](") {
                Some(end) if source.starts_with('[') => source[1..end].to_string(),
                _ => continue,
            };
            found.push(Link {
                range,
                label,
                dest: dest_url.to_string(),
            });
        }
    }
    found
}

/// The code spans in `md` that aren't in a link, in order.
pub fn code_spans(md: &str) -> Vec<Code> {
    let mut found = Vec::new();
    let mut in_link = 0;
    for (event, range) in Parser::new_ext(md, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Link { .. } | Tag::Image { .. }) => in_link += 1,
            Event::End(TagEnd::Link | TagEnd::Image) => in_link -= 1,
            Event::Code(text) if in_link == 0 => found.push(Code {
                range,
                text: text.to_string(),
            }),
            _ => {}
        }
    }
    found
}

/// `md` with each range in `edits` replaced by its text. The ranges don't
/// overlap.
pub fn splice(md: &str, mut edits: Vec<(Range<usize>, String)>) -> String {
    edits.sort_by_key(|(range, _)| range.start);
    let mut out = String::with_capacity(md.len());
    let mut at = 0;
    for (range, text) in edits {
        if range.start < at {
            continue;
        }
        out.push_str(&md[at..range.start]);
        out.push_str(&text);
        at = range.end;
    }
    out.push_str(&md[at..]);
    out
}

/// `md`'s headings made level four: a subsection's body sits under the
/// page's third.
pub fn demote_headings(md: &str) -> String {
    let mut fence: Option<String> = None;
    let mut out = Vec::new();
    for line in md.lines() {
        let trimmed = line.trim_start();
        if let Some(open) = &fence {
            if trimmed.starts_with(open.as_str()) {
                fence = None;
            }
            out.push(line.to_string());
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(trimmed[..3].to_string());
            out.push(line.to_string());
            continue;
        }
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=3).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            out.push(format!("####{}", &trimmed[hashes..]));
        } else {
            out.push(line.to_string());
        }
    }
    let mut text = out.join("\n");
    if md.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const MD: &str = "See [`spawn`](code:src/a.rs#L3) and `Session`, [the rest](#rest).\n\n```rust\nlet x = `not_code`;\n```\n\n- `a::b` in a list, [`c`](https://x.y)\n";

    #[test]
    fn links_are_found_with_their_labels_and_destinations() {
        let found = links(MD);
        let summary: Vec<(&str, &str)> = found
            .iter()
            .map(|link| (link.label.as_str(), link.dest.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                ("`spawn`", "code:src/a.rs#L3"),
                ("the rest", "#rest"),
                ("`c`", "https://x.y")
            ]
        );
        assert_eq!(&MD[found[0].range.clone()], "[`spawn`](code:src/a.rs#L3)");
    }

    #[test]
    fn code_spans_outside_links_and_fences_are_found() {
        let found: Vec<String> = code_spans(MD).into_iter().map(|code| code.text).collect();
        assert_eq!(found, ["Session", "a::b"]);
        let spans = code_spans(MD);
        assert_eq!(&MD[spans[0].range.clone()], "`Session`");
    }

    #[test]
    fn splicing_replaces_each_range() {
        let spans = code_spans(MD);
        let edits = spans
            .iter()
            .map(|code| (code.range.clone(), format!("[`{}`](code:x)", code.text)))
            .collect();
        let out = splice(MD, edits);
        assert!(out.contains("and [`Session`](code:x),"), "{out}");
        assert!(out.contains("- [`a::b`](code:x) in a list"), "{out}");
        assert!(out.contains("let x = `not_code`;"), "{out}");
    }

    #[test]
    fn headings_in_a_body_go_below_the_page_s() {
        let out = demote_headings(
            "# Big\n\ntext\n\n### Small\n```\n# a comment\n```\n#### Kept\n#hashtag\n",
        );
        assert_eq!(
            out,
            "#### Big\n\ntext\n\n#### Small\n```\n# a comment\n```\n#### Kept\n#hashtag\n"
        );
    }
}
