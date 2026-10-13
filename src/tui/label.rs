//! Text from T3 cut to one row of a frame, such as a project's or a thread's title or a
//! thread's branch in the breadcrumb and the sidebar's cards. T3 puts no limit on their length,
//! so a frame reads only as much of each as its row could show. For a row of `width` columns it
//! stops at the character after the first cluster past the row, and reads at most
//! `(width + 2) * PER_COLUMN + 1` characters. A row of no columns reads nothing. A frame never
//! copies, cleans or measures the rest.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The characters a frame may keep reading for each column of a row, counting two columns past
/// it. It reads one character more to learn whether the text goes on, so a row of `width`
/// columns reads at most `(width + 2) * PER_COLUMN + 1`. That is room for several accents on
/// every letter, or a joined emoji in every other column. Text that packs in more, such as a
/// long run of controls or marks, is cut where the reading stops.
const PER_COLUMN: usize = 16;

/// `text` in at most `width` columns as one row: whole when it fits, and otherwise cut with
/// `…` where `transcript::clusters` would cut it, so an accent stays on its letter and a
/// joined emoji stays whole, and a wide character that doesn't fit before the `…` is left out.
/// A line break or a tab reads as a space, `\r\n` as one, and other controls, C0, DEL or C1,
/// are dropped, as `transcript::without_controls` drops them. What followed an Esc stays as
/// plain text. No columns leave nothing at all, even of text that takes none.
pub(super) fn cut(text: &str, width: usize) -> String {
    let mut head = head(text.chars(), width);
    if !head.whole {
        head.text.truncate(head.short);
        if width > 0 {
            head.text.push('…');
        }
    }
    head.text
}

/// The columns `text` takes as `cut` writes it, or `max` when that is fewer.
pub(super) fn columns(text: &str, max: usize) -> usize {
    let head = head(text.chars(), max);
    if head.whole { head.width } else { max }
}

/// What a row can show of some text.
struct Head {
    /// The text's first whole clusters, cleaned, in at most the columns asked for.
    text: String,
    /// Its width in columns.
    width: usize,
    /// How many bytes at the start of `text` fit in one column fewer, which leaves the `…` its
    /// column.
    short: usize,
    /// Whether `text` is all of the text. It never is for a row of no columns, which reads none
    /// of it.
    whole: bool,
}

impl Head {
    /// Keeps the cluster from byte `start` to the end of `text` if it fits in `width` columns
    /// after the clusters before it, and otherwise drops it. Says whether it fit.
    fn close(&mut self, start: usize, width: usize) -> bool {
        let columns = self.text[start..].width();
        if self.width + columns > width {
            self.text.truncate(start);
            return false;
        }
        self.width += columns;
        if self.width < width {
            self.short = self.text.len();
        }
        true
    }
}

/// Reads `chars` until a cluster takes them past `width` columns, which it learns at the
/// character after that cluster, or until it reads character `(width + 2) * PER_COLUMN + 1`,
/// which it drops, whichever comes first. A row of no columns reads nothing. A cluster starts
/// where `transcript::clusters` starts one: at each character with a width, except a skin tone
/// and the character after a zero-width joiner. A cluster still open when the reading stops is
/// left out.
fn head(chars: impl Iterator<Item = char>, width: usize) -> Head {
    let limit = width.saturating_add(2).saturating_mul(PER_COLUMN);
    let mut head = Head {
        text: String::new(),
        width: 0,
        short: 0,
        whole: false,
    };
    // A mark or a joiner takes no columns, yet a row of none shows nothing, so it reads nothing.
    if width == 0 {
        return head;
    }
    // Where the open cluster starts in `head.text`.
    let mut start = 0;
    let mut joined = false;
    let mut after_cr = false;
    for (read, c) in chars.enumerate() {
        if read == limit {
            head.text.truncate(start);
            return head;
        }
        let cr = after_cr;
        after_cr = c == '\r';
        let c = match c {
            '\n' if cr => continue,
            '\n' | '\r' => ' ',
            c if c.is_control() && c.is_whitespace() => ' ',
            c if c.is_control() => continue,
            c => c,
        };
        let wide = UnicodeWidthChar::width(c).unwrap_or(0) > 0;
        let skin_tone = matches!(c, '\u{1F3FB}'..='\u{1F3FF}');
        if !head.text.is_empty() && wide && !joined && !skin_tone {
            if !head.close(start, width) {
                return head;
            }
            start = head.text.len();
        }
        head.text.push(c);
        joined = c == '\u{200D}';
    }
    head.whole = head.close(start, width);
    head
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::iter::{once, repeat};

    use super::*;
    use crate::transcript;

    #[test]
    fn text_that_fits_keeps_every_character() {
        for text in [
            "",
            "Fix the login page",
            "名前",
            "cafe\u{301}",
            "a👩\u{200d}💻b",
            "👍🏽 ok",
            "]52;c;[2J",
        ] {
            assert_eq!(cut(text, text.width()), text);
            assert_eq!(cut(text, 80), text);
            assert_eq!(columns(text, 80), text.width());
        }
    }

    #[test]
    fn ascii_is_cut_as_fit_cuts_it() {
        let texts = [
            "Fix the login page",
            "a",
            "ab",
            "feature/sidebar-badges",
            "  x  ",
        ];
        for text in texts {
            for width in 1..=text.len() + 2 {
                assert_eq!(cut(text, width), super::super::fit(text, width), "{width}");
            }
        }
        assert_eq!(cut("Fix the login page", 10), "Fix the l…");
        assert_eq!(columns("Fix the login page", 16), 16);
        assert_eq!(columns("Fix the login", 16), 13);
    }

    #[test]
    fn a_cut_never_passes_the_width_or_splits_a_cluster() {
        assert_eq!(cut("名名名名", 6), "名名…");
        assert_eq!(cut("名名名名", 5), "名名…");
        assert_eq!(cut("a名", 2), "a…");
        assert_eq!(cut("cafe\u{301}!", 5), "cafe\u{301}!");
        assert_eq!(cut("cafe\u{301}!", 4), "caf…");
        assert_eq!(cut("a👩\u{200d}💻b", 3), "a…");
        assert_eq!(cut("👍🏽x", 2), "…");
        let texts = [
            "名".repeat(30),
            "👩\u{200d}💻e\u{301}名".repeat(10),
            "e\u{301}\u{302}\u{303}".repeat(30),
        ];
        for text in &texts {
            let pieces = transcript::clusters(text);
            for width in 0..40 {
                let shown = cut(text, width);
                assert!(shown.width() <= width, "{width}: {shown:?}");
                // What shows before the `…` is whole clusters from the start of the text.
                let mut rest = shown.strip_suffix('…').unwrap_or(&shown);
                for piece in &pieces {
                    if rest.is_empty() {
                        break;
                    }
                    rest = rest
                        .strip_prefix(*piece)
                        .unwrap_or_else(|| panic!("{width}: {shown:?} splits {piece:?}"));
                }
                assert!(rest.is_empty(), "{width}: {shown:?}");
            }
        }
    }

    #[test]
    fn line_breaks_and_tabs_read_as_spaces_and_other_controls_go() {
        assert_eq!(cut("a\nb", 10), "a b");
        assert_eq!(cut("a\r\nb", 10), "a b");
        assert_eq!(cut("a\rb", 10), "a b");
        assert_eq!(cut("a\r\r\nb", 10), "a  b");
        assert_eq!(cut("a\tb", 10), "a b");
        assert_eq!(
            cut("\u{1b}]52;c;cHduZWQ=\u{7}\u{1b}[2Jok", 40),
            "]52;c;cHduZWQ=[2Jok"
        );
        assert_eq!(cut("a\u{9b}31mb\u{7f}", 10), "a31mb");
        // A joiner isn't a control, so a joined emoji keeps it.
        assert_eq!(cut("👩\u{200d}💻", 2), "👩\u{200d}💻");
        // Dropped controls take no columns, so the text after them still fits.
        assert_eq!(cut("\u{1b}\u{1b}\u{1b}abc", 3), "abc");
        assert_eq!(columns("\u{1b}\u{1b}\u{1b}abc", 16), 3);
        // A cut counts what shows, so a line break before the cut is a space in it.
        assert_eq!(cut("ab\ncdef", 5), "ab c…");
    }

    #[test]
    fn no_columns_show_nothing_and_one_shows_the_ellipsis() {
        // No columns show nothing, even of text that takes none, and read none of it.
        for text in [
            "abc",
            "",
            "\u{301}",
            "\u{200d}",
            "\u{1b}",
            "\u{7}\u{200d}\u{301}",
        ] {
            assert_eq!(cut(text, 0), "", "{text:?}");
            assert_eq!(columns(text, 0), 0, "{text:?}");
        }
        for c in ['a', '\u{301}', '\u{200d}', '\u{1b}'] {
            let (read, head) = reads(repeat(c), 0);
            assert_eq!((read, head.text.as_str()), (0, ""), "{c:?}");
        }
        assert_eq!(cut("abc", 1), "…");
        assert_eq!(cut("a", 1), "a");
        assert_eq!(cut("名", 1), "…");
        assert_eq!(cut("名", 2), "名");
    }

    /// The characters `head` reads from `chars` for a row of `width` columns, and what it
    /// keeps of them.
    fn reads(chars: impl Iterator<Item = char>, width: usize) -> (usize, Head) {
        let read = Cell::new(0);
        let head = head(chars.inspect(|_| read.set(read.get() + 1)), width);
        (read.get(), head)
    }

    #[test]
    fn a_row_reads_no_further_than_it_can_show() {
        // Text with no end stops at the character after the first cluster past the row.
        let (read, head) = reads(repeat('a'), 10);
        assert_eq!(
            (read, head.text.as_str(), head.whole),
            (12, "aaaaaaaaaa", false)
        );
        let (read, head) = reads(repeat('名'), 10);
        assert_eq!((read, head.width), (7, 10));
        // Text with no end that shows nothing stops too. It reads `PER_COLUMN` characters for
        // each column and two columns more, then the one that shows the text goes on, and keeps
        // none of them. The texts are a run of controls, a letter under a run of marks, and one
        // long chain of joined letters.
        fn endless<I: Iterator<Item = char>>(chars: impl Fn() -> I) {
            for width in [1, 10, 20] {
                let (read, head) = reads(chars(), width);
                assert_eq!((head.text.as_str(), head.whole), ("", false), "{width}");
                assert_eq!(read, (width + 2) * PER_COLUMN + 1, "{width}");
            }
        }
        endless(|| repeat('\u{1b}'));
        endless(|| once('e').chain(repeat('\u{301}')));
        endless(|| once('a').chain(repeat(['\u{200d}', 'a']).flatten()));
    }

    #[test]
    fn huge_text_shows_what_it_can_before_the_cut() {
        let marks = format!("ok e{}", "\u{301}".repeat(100_000));
        assert_eq!(cut(&marks, 10), "ok …");
        let joined = format!("a{}", "\u{200d}a".repeat(100_000));
        assert_eq!(cut(&joined, 10), "…");
        let controls = format!("{}visible", "\u{1b}".repeat(100_000));
        assert_eq!(cut(&controls, 10), "…");
        let title = format!(
            "\u{1b}]0;x\u{7}Fix {}\r\nevil",
            "👩\u{200d}💻e\u{301}名".repeat(20_000)
        );
        // Eight columns before the emoji, then five for each whole run of the three clusters.
        let shown = cut(&title, 30);
        let runs = "👩\u{200d}💻e\u{301}名".repeat(4);
        assert_eq!(shown, format!("]0;xFix {runs}…"));
        assert_eq!(shown.width(), 29);
        assert_eq!(columns(&title, 16), 16);
    }
}
