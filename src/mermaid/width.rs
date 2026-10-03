//! How many columns of a terminal text takes. A diagram is laid out on a
//! grid of cells, so a label two columns wider on screen than its count of
//! characters would push its box's border out and tear the drawing: wide
//! characters (CJK, most emoji) take two cells, combining marks none.

use unicode_width::UnicodeWidthChar;

/// The columns `c` takes: 0, 1 or 2. A control character takes none:
/// tabs are spaces by the time anything is measured, and nothing else of
/// the kind belongs in a diagram.
pub fn char_width(c: char) -> usize {
    c.width().unwrap_or(0).min(2)
}

/// The columns `s` takes, character by character, the way the grid places
/// them.
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// `s` cut to at most `max` columns, ending in `…` when anything was cut.
/// A wide character that would straddle the edge is left out whole, so
/// the result can be a column short of `max`.
pub fn truncate(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let width = char_width(c);
        if used + width + 1 > max {
            break;
        }
        out.push(c);
        used += width;
    }
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_box_drawing_and_arrows_are_one_column() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("┌─┐│└┘├┤┬┴┼"), 11);
        assert_eq!(display_width("▶▼◀▲●◉…"), 7);
        assert_eq!(display_width("café"), 4);
    }

    #[test]
    fn cjk_hangul_fullwidth_and_emoji_are_two_columns() {
        assert_eq!(display_width("日本語"), 6);
        assert_eq!(display_width("한국"), 4);
        assert_eq!(display_width("ＡＢ"), 4);
        assert_eq!(display_width("🚀"), 2);
        assert_eq!(display_width("a日b"), 4);
    }

    #[test]
    fn combining_marks_joiners_and_controls_take_nothing() {
        assert_eq!(display_width("e\u{301}"), 1);
        assert_eq!(display_width("a\u{200D}b"), 2);
        assert_eq!(display_width("\u{FE0F}"), 0);
        assert_eq!(display_width("\u{7}"), 0);
    }

    #[test]
    fn truncate_counts_columns_and_never_splits_a_wide_char() {
        assert_eq!(truncate("request", 10), "request");
        assert_eq!(truncate("request path", 8), "request…");
        assert_eq!(display_width(&truncate("request path", 8)), 8);
        // 日本語テキスト is 14 columns; at 6, two ideographs fit before the
        // ellipsis.
        let cut = truncate("日本語テキスト", 6);
        assert_eq!(cut, "日本…");
        assert!(display_width(&cut) <= 6);
        assert_eq!(truncate("日本語テキスト", 4), "日…");
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("abc", 1), "…");
    }
}
