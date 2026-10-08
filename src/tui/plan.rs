//! Proposed plans. The card's text follows the nightly desktop's `apps/web/src/proposedPlan.ts`
//! and the card follows `apps/web/src/components/chat/ProposedPlanCard.tsx`.
//!
//! T3 measures and trims plans in JavaScript, so these rules do too. They count UTF-16 code
//! units, split lines with `split("\n")` or `split(/\r?\n/)`, and trim the characters
//! JavaScript's `\s` and `trim` treat as space. Rust's versions differ on each: `len` counts
//! UTF-8 bytes, `lines` drops a last empty line, and `trim` keeps U+FEFF and removes U+0085.

use std::collections::HashSet;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::{RenderContext, fit, markdown};
use crate::transcript;

/// A plan longer than this many UTF-16 code units starts collapsed (`ProposedPlanCard.tsx:77`).
const COLLAPSE_UNITS: usize = 900;
/// So does a plan of more lines than this, counted with `split("\n")`.
const COLLAPSE_LINES: usize = 20;
/// The lines with text a collapsed plan shows (`ProposedPlanCard.tsx:80`).
const PREVIEW_LINES: usize = 10;

/// What JavaScript's `\s` matches, which is also what its `trim` removes. That is Unicode's
/// White_Space, which `char::is_whitespace` uses, less U+0085 and plus U+FEFF.
fn js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{FEFF}'
}

/// A JavaScript line terminator, where `.` stops and a multiline `^` or `$` matches.
fn line_end(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn trim(text: &str) -> &str {
    text.trim_matches(js_space)
}

fn trim_end(text: &str) -> &str {
    text.trim_end_matches(js_space)
}

/// `text.split(/\r?\n/)`: pieces between `\n`s, each losing one `\r` before its `\n`.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    let last = lines.len() - 1;
    for line in &mut lines[..last] {
        *line = line.strip_suffix('\r').unwrap_or(line);
    }
    lines
}

/// Whether `chars` starts the way `/^\s{0,3}#{1,6}\s+/` asks: at most three spaces, one to
/// six `#`s and a space. Returns the index just past the `#`s.
fn heading_marks(chars: &[char]) -> Option<usize> {
    let spaces = chars.iter().take(4).take_while(|c| js_space(**c)).count();
    if spaces > 3 {
        return None;
    }
    let marks = chars[spaces..]
        .iter()
        .take(7)
        .take_while(|c| **c == '#')
        .count();
    let after = spaces + marks;
    let spaced = chars.get(after).copied().is_some_and(js_space);
    ((1..=6).contains(&marks) && spaced).then_some(after)
}

/// What `\s+(.+)$` captures after the `#`s that end at `after`. `\s+` is greedy and crosses
/// line breaks, so a heading with nothing after it takes the next line. With `multiline`, `$`
/// matches at any line end. Without it, `$` matches only at the end of `chars`, so the capture
/// has to reach it.
fn heading_text(chars: &[char], after: usize, multiline: bool) -> Option<&[char]> {
    let rest = &chars[after..];
    let spaces = rest.iter().take_while(|c| js_space(**c)).count();
    let lowest = if multiline {
        1
    } else {
        // The capture has to run to the end, so it starts past the last line end.
        let last = rest.iter().rposition(|c| line_end(*c));
        last.map_or(1, |i| i + 1)
    };
    let skip = (lowest..=spaces)
        .rev()
        .find(|&skip| rest.get(skip).is_some_and(|c| !line_end(*c)))?;
    let text = &rest[skip..];
    let end = text.iter().position(|c| line_end(*c));
    Some(&text[..end.unwrap_or(text.len())])
}

/// The plan's title, as `proposedPlanTitle` finds it: the first heading on any line.
fn title(markdown: &str) -> Option<String> {
    let chars: Vec<char> = markdown.chars().collect();
    let found = (0..=chars.len())
        .filter(|&start| start == 0 || line_end(chars[start - 1]))
        .find_map(|start| {
            let after = start + heading_marks(&chars[start..])?;
            heading_text(&chars, after, true)
        })?;
    let text: String = found.iter().collect();
    let text = trim(&text);
    (!text.is_empty()).then(|| text.to_string())
}

/// Drops the blank lines at the start of `lines`.
fn skip_blank<'a, 'b>(lines: &'a [&'b str]) -> &'a [&'b str] {
    let blank = lines
        .iter()
        .take_while(|line| trim(line).is_empty())
        .count();
    &lines[blank..]
}

/// The plan as the card shows it, after `stripDisplayedPlanMarkdown`. A heading on the first
/// line is the title, which the card's header already shows, so it goes. So does a `Summary`
/// heading right after it, with the blank lines around both.
fn strip_displayed(markdown: &str) -> String {
    let lines = split_lines(trim_end(markdown));
    let first: Vec<char> = lines[0].chars().collect();
    let mut rest = if heading_marks(&first).is_some() {
        &lines[1..]
    } else {
        &lines[..]
    };
    rest = skip_blank(rest);
    if let Some(line) = rest.first() {
        let chars: Vec<char> = line.chars().collect();
        let heading = heading_marks(&chars)
            .and_then(|after| heading_text(&chars, after, false))
            .map(|text| text.iter().collect::<String>());
        if heading.is_some_and(|text| trim(&text).to_lowercase() == "summary") {
            rest = skip_blank(&rest[1..]);
        }
    }
    rest.join("\n")
}

/// What a collapsed card shows, after `buildCollapsedProposedPlanPreviewMarkdown`: the body
/// up to `max_lines` lines with text, then `...` when more follows. A plan with no body shows
/// its title instead.
fn collapsed_preview(markdown: &str, max_lines: usize) -> String {
    let body = strip_displayed(markdown);
    let mut preview: Vec<&str> = Vec::new();
    let mut shown = 0;
    let mut more = false;
    for line in split_lines(trim_end(&body)) {
        let line = trim_end(line);
        let has_text = !trim(line).is_empty();
        if has_text && shown >= max_lines {
            more = true;
            break;
        }
        preview.push(line);
        if has_text {
            shown += 1;
        }
    }
    while preview.last().is_some_and(|line| trim(line).is_empty()) {
        preview.pop();
    }
    if preview.is_empty() {
        return title(markdown).unwrap_or_else(|| "Plan preview unavailable.".into());
    }
    if more {
        preview.extend(["", "..."]);
    }
    preview.join("\n")
}

/// Whether the plan is long enough to start collapsed, by the card's own measure.
fn can_collapse(markdown: &str) -> bool {
    markdown.encode_utf16().count() > COLLAPSE_UNITS
        || markdown.split('\n').count() > COLLAPSE_LINES
}

/// The text with each control character but `\n`, such as a tab, turned into a space. The
/// terminal drops control characters, while `unicode-width` counts each as a column, so they
/// would pull the card's right edge out of line.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect()
}

/// A proposed plan drawn as a card.
pub struct Card {
    pub lines: Vec<Line<'static>>,
    /// The header row and the button's row, for a plan long enough to collapse.
    pub toggle: Option<(usize, usize)>,
}

/// The card: a frame with a Plan chip and the plan's title on top and the plan inside as
/// Markdown. A long plan shows its preview until it is expanded, with an Expand plan or
/// Collapse plan button in the bottom edge. The GUI also fades and clips the preview, which
/// a terminal can't, so the `...` line marks the cut.
pub fn card(markdown: &str, width: usize, context: &RenderContext, expanded: bool) -> Card {
    let t = context.theme;
    let edge = Style::new().fg(t.border_strong);
    let chip = Style::new().fg(t.fg).bg(t.chip_bg);
    let collapsible = can_collapse(markdown);
    let body = if collapsible && !expanded {
        collapsed_preview(markdown, PREVIEW_LINES)
    } else {
        strip_displayed(markdown)
    };
    let mut lines = vec![Line::default()];

    // The title is cut to leave room for a space, one dash and the corner after it.
    let name = title(markdown).unwrap_or_else(|| "Proposed plan".into());
    let mut top = vec![
        Span::styled("╭─", edge),
        Span::styled(" Plan ", chip),
        Span::raw(" "),
    ];
    let room = width.saturating_sub(12);
    if room > 0 {
        top.push(Span::styled(
            fit(&printable(&name), room),
            Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        ));
        top.push(Span::raw(" "));
    }
    let used: usize = top.iter().map(|span| span.content.width()).sum();
    let dashes = "─".repeat(width.saturating_sub(used + 1));
    top.push(Span::styled(format!("{dashes}╮"), edge));
    lines.push(Line::from(top));

    let inner = width.saturating_sub(4);
    for line in markdown::render(&printable(&body), inner, &context.text, 0) {
        let used = line.width();
        let mut spans = vec![Span::styled("│ ", edge)];
        spans.extend(line.spans);
        spans.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        spans.push(Span::styled(" │", edge));
        lines.push(Line::from(spans));
    }

    if collapsible {
        let label = if expanded {
            " Collapse plan "
        } else {
            " Expand plan "
        };
        let label = fit(label, width.saturating_sub(2));
        let side = width.saturating_sub(label.width() + 2);
        lines.push(Line::from(vec![
            Span::styled(format!("╰{}", "─".repeat(side / 2)), edge),
            Span::styled(label, chip),
            Span::styled(format!("{}╯", "─".repeat(side - side / 2)), edge),
        ]));
    } else {
        let bottom = format!("╰{}╯", "─".repeat(width.saturating_sub(2)));
        lines.push(Line::styled(bottom, edge));
    }
    let toggle = collapsible.then_some((1, lines.len() - 1));
    lines.push(Line::default());
    Card { lines, toggle }
}

/// A card long enough to collapse, as the last frame drew it.
pub struct Drawn {
    pub id: String,
    /// The header's row counted from the first transcript row on screen, negative once the
    /// reader has scrolled past it.
    pub header: isize,
    /// The screen rows of the header and the button, where each is in view.
    pub rows: [Option<u16>; 2],
}

/// The card `id` when a frame draws rows `from..to` of it, the first of them on transcript row
/// `base` of a transcript that starts on screen row `y`. `toggle` holds the card's header and
/// button rows. A card counts as in view while any row from its header to its button shows,
/// even when that is only part of the body. The blank rows around it don't count.
pub fn drawn(
    id: &str,
    (header, button): (usize, usize),
    from: usize,
    to: usize,
    base: usize,
    y: u16,
) -> Option<Drawn> {
    if header >= to || button < from {
        return None;
    }
    let row = |index: usize| {
        if (from..to).contains(&index) {
            Some(y + (base + index - from) as u16)
        } else {
            None
        }
    };
    Some(Drawn {
        id: id.to_string(),
        header: (base + header) as isize - from as isize,
        rows: [row(header), row(button)],
    })
}

/// The card whose header or button is on screen row `row`.
pub fn at(cards: &[Drawn], row: u16) -> Option<&Drawn> {
    cards.iter().find(|card| card.rows.contains(&Some(row)))
}

/// Expands or collapses the card `id` and returns the row its header should keep. An
/// expanding card keeps its header where it is, on screen or above it, so the preview the
/// reader was looking at stays put. A collapsing card whose header has scrolled above the
/// screen brings it to the top row instead, since the shorter card would otherwise end above
/// the screen and leave an unrelated block in view.
pub fn flip(expanded: &mut HashSet<String>, id: &str, header: isize) -> isize {
    if expanded.remove(id) {
        header.max(0)
    } else {
        expanded.insert(id.to_string());
        header
    }
}

/// Forgets the expanded cards whose plans have left the thread, the way the GUI's card loses
/// its state when it unmounts.
pub fn keep_present(expanded: &mut HashSet<String>, blocks: &[transcript::Block]) {
    if !expanded.is_empty() {
        expanded.retain(|id| {
            blocks
                .iter()
                .any(|block| block.item_id == *id && block.item_type == "proposed_plan")
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::BlockKind;
    use crate::tui::markdown::Styles;
    use crate::tui::theme::{Depth, Theme};

    fn context(theme: &Theme) -> RenderContext<'_> {
        RenderContext {
            theme,
            text: Styles::new(theme, theme.text()),
            bubble: Styles::new(theme, theme.text()),
            reasoning: Styles::new(theme, Style::new().fg(theme.muted)).dimmed(),
        }
    }

    fn rows(card: &Card) -> Vec<String> {
        card.lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// Thirty lines: a title, a Summary heading, an intro and 24 steps.
    fn long_plan() -> String {
        let steps: Vec<String> = (1..=24).map(|i| format!("- Step {i}")).collect();
        format!(
            "# Ship the plan card\n\n## Summary\n\nRead plans in the terminal.\n\n{}",
            steps.join("\n")
        )
    }

    #[test]
    fn the_title_is_the_first_heading_on_any_line() {
        // The nightly's own cases, from proposedPlan.test.ts.
        assert_eq!(
            title("# Integrate RPC\n\nBody").as_deref(),
            Some("Integrate RPC")
        );
        assert_eq!(title("- step 1"), None);
        // Any line can hold it, after up to three spaces.
        assert_eq!(
            title("Intro\n\n   ### Later heading  \nBody").as_deref(),
            Some("Later heading")
        );
        assert_eq!(title("    # Four spaces make code"), None);
        assert_eq!(title("####### Seven marks"), None);
        assert_eq!(title("#Tight"), None);
        assert_eq!(title(""), None);
        assert_eq!(title(" \n\t"), None);
        // `.` stops at `\r`, so a CRLF title loses its carriage return.
        assert_eq!(title("# Ship it\r\nBody").as_deref(), Some("Ship it"));
        // JavaScript also ends lines at U+2028, which `str::lines` doesn't.
        assert_eq!(title("Intro\u{2028}## After").as_deref(), Some("After"));
        // An empty heading's `\s+` runs on through the line break and takes the next line.
        assert_eq!(title("#   \n## Scope").as_deref(), Some("## Scope"));
        // JavaScript trims U+FEFF and keeps U+0085, the reverse of `str::trim`.
        assert_eq!(title("Body\n# \u{FEFF}"), None);
        assert_eq!(title("# Title\u{85}").as_deref(), Some("Title\u{85}"));
    }

    #[test]
    fn the_body_leaves_out_the_title_line_and_a_summary_heading() {
        // The nightly's own cases.
        assert_eq!(
            strip_displayed("# Integrate RPC\n\n## Summary\n\n- step 1\n"),
            "- step 1"
        );
        assert_eq!(
            strip_displayed("# Integrate RPC\n\n## Scope\n\n- step 1\n"),
            "## Scope\n\n- step 1"
        );
        // Only the first line can be the title, so a blank line before it keeps it.
        assert_eq!(strip_displayed("- step 1\n- step 2"), "- step 1\n- step 2");
        assert_eq!(strip_displayed("\n# Title\nBody"), "# Title\nBody");
        // Summary goes in any case, but only right after the title and only by that name.
        assert_eq!(strip_displayed("# Plan\n### SUMMARY\nBody"), "Body");
        assert_eq!(
            strip_displayed("# Plan\n## Summary of changes\nBody"),
            "## Summary of changes\nBody"
        );
        assert_eq!(
            strip_displayed("# Plan\nIntro\n## Summary\nBody"),
            "Intro\n## Summary\nBody"
        );
        // A title with nothing under it leaves nothing.
        assert_eq!(strip_displayed("# Only a title"), "");
        assert_eq!(strip_displayed(""), "");
        assert_eq!(strip_displayed(" \n\t "), "");
        // CRLF lines lose their `\r`, and the body comes back joined with `\n`.
        assert_eq!(
            strip_displayed("# Title\r\n\r\n## Summary\r\n\r\nStep one\r\nStep two\r\n"),
            "Step one\nStep two"
        );
        // Blank lines and indentation inside the body stay.
        assert_eq!(strip_displayed("# T\n\nA\n\n  B  \nC"), "A\n\n  B  \nC");
    }

    #[test]
    fn the_preview_keeps_the_first_lines_with_text_and_marks_the_rest() {
        // The nightly's own cases.
        assert_eq!(
            collapsed_preview("# Integrate RPC\n\n## Summary\n\n- step 1\n- step 2", 4),
            "- step 1\n- step 2"
        );
        assert_eq!(
            collapsed_preview("# Integrate RPC\n\n- step 1\n- step 2\n- step 3", 2),
            "- step 1\n- step 2\n\n..."
        );
        // Ten lines with text fit. The blank lines between them don't count.
        let ten = (1..=10)
            .map(|i| format!("Step {i}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let plan = format!("# Plan\n\n{ten}");
        assert_eq!(collapsed_preview(&plan, PREVIEW_LINES), ten);
        // An eleventh is cut, with the blank line before it.
        let eleven = format!("{plan}\n\nStep 11");
        assert_eq!(
            collapsed_preview(&eleven, PREVIEW_LINES),
            format!("{ten}\n\n...")
        );
        // Every line loses its trailing spaces, and CRLF reads as LF.
        assert_eq!(
            collapsed_preview("# T\r\n\r\nOne  \r\nTwo\t\r\n", 10),
            "One\nTwo"
        );
        // With no body the preview falls back to the title, then to a note.
        assert_eq!(collapsed_preview("# Only a title", 10), "Only a title");
        assert_eq!(collapsed_preview("", 10), "Plan preview unavailable.");
        assert_eq!(collapsed_preview(" \n ", 10), "Plan preview unavailable.");
    }

    #[test]
    fn a_plan_collapses_past_900_utf16_units_or_20_lines() {
        assert!(!can_collapse(&"a".repeat(900)));
        assert!(can_collapse(&"a".repeat(901)));
        // Length is JavaScript's: UTF-16 code units, not UTF-8 bytes or chars.
        let accents = "\u{E9}".repeat(900);
        assert_eq!(accents.len(), 1800);
        assert!(!can_collapse(&accents));
        assert!(!can_collapse(&"\u{1F600}".repeat(450)));
        let faces = "\u{1F600}".repeat(451);
        assert_eq!(faces.chars().count(), 451);
        assert!(can_collapse(&faces));
        // Lines are the pieces of `split("\n")`, so a last newline starts one more.
        let twenty = ["x"; 20].join("\n");
        assert!(!can_collapse(&twenty));
        assert!(can_collapse(&format!("{twenty}\ny")));
        let ended = format!("{twenty}\n");
        assert_eq!(ended.lines().count(), 20);
        assert!(can_collapse(&ended));
        // CRLF ends a line once. A lone `\r` or U+2028 doesn't end one.
        assert!(!can_collapse(&["x"; 20].join("\r\n")));
        assert!(!can_collapse(&"x\r".repeat(30)));
        assert!(!can_collapse(&"x\u{2028}".repeat(30)));
    }

    #[test]
    fn a_long_plan_shows_its_preview_until_expanded() {
        let theme = Theme::new(Depth::TrueColor);
        let context = context(&theme);
        let plan = long_plan();

        let collapsed = card(&plan, 40, &context, false);
        let text = rows(&collapsed);
        let last = text.len() - 1;
        assert_eq!(collapsed.toggle, Some((1, last - 1)));
        assert_eq!((text[0].as_str(), text[last].as_str()), ("", ""));
        assert!(
            text[1].starts_with("╭─ Plan  Ship the plan card ─") && text[1].ends_with("─╮"),
            "{text:?}"
        );
        // The title and the Summary heading are in the header, not the body.
        assert!(!text[2..].concat().contains("Ship the plan card"));
        assert!(!text.concat().contains("Summary"));
        assert!(
            text[2].starts_with("│ Read plans in the terminal. "),
            "{text:?}"
        );
        // The intro and nine steps make ten lines with text, then the cut.
        assert!(text.iter().any(|row| row.starts_with("│ • Step 9 ")));
        assert!(!text.iter().any(|row| row.contains("Step 10")));
        assert!(text.iter().any(|row| row.starts_with("│ ... ")));
        assert!(
            text[last - 1].starts_with("╰─") && text[last - 1].contains(" Expand plan "),
            "{text:?}"
        );
        for row in &text[1..last] {
            assert_eq!(row.width(), 40, "{row:?}");
            assert!(row.ends_with(['│', '╮', '╯']), "{row:?}");
        }

        let expanded = card(&plan, 40, &context, true);
        let text = rows(&expanded);
        let last = text.len() - 1;
        assert_eq!(expanded.toggle, Some((1, last - 1)));
        assert!(text[last - 1].contains(" Collapse plan "), "{text:?}");
        // Nothing is lost: every step is there and the cut is gone.
        for i in 1..=24 {
            let step = format!("│ • Step {i} ");
            assert!(text.iter().any(|row| row.starts_with(&step)), "{step}");
        }
        assert!(!text.iter().any(|row| row.starts_with("│ ... ")));
        for row in &text[1..last] {
            assert_eq!(row.width(), 40, "{row:?}");
        }
    }

    #[test]
    fn a_short_plan_reads_in_full_with_no_button() {
        let theme = Theme::new(Depth::TrueColor);
        let context = context(&theme);
        let source = "# Fix the bug\n\n- Read the log\n- Patch it";
        let short = card(source, 30, &context, false);
        let text = rows(&short);
        assert_eq!(short.toggle, None);
        assert!(text[1].starts_with("╭─ Plan  Fix the bug ─"), "{text:?}");
        assert!(text[2].starts_with("│ • Read the log "), "{text:?}");
        assert!(text[3].starts_with("│ • Patch it "), "{text:?}");
        assert_eq!(text[4], format!("╰{}╯", "─".repeat(28)));
        assert_eq!(text.len(), 6);
        // Expanded or not, a short plan draws the same.
        assert_eq!(rows(&card(source, 30, &context, true)), text);

        // With no heading the card says Proposed plan, as the GUI does.
        let untitled = rows(&card("- one\n- two", 30, &context, false));
        assert!(untitled[1].contains(" Proposed plan "), "{untitled:?}");
        assert!(untitled[2].starts_with("│ • one "), "{untitled:?}");
        // A title alone leaves an empty card.
        let bare = rows(&card("# Only a title", 30, &context, false));
        assert_eq!(bare.len(), 4, "{bare:?}");
        assert!(bare[1].contains(" Only a title "), "{bare:?}");
    }

    #[test]
    fn narrow_cards_cut_the_title_and_keep_their_edges() {
        let theme = Theme::new(Depth::TrueColor);
        let context = context(&theme);
        let plan = long_plan();
        for width in [17, 24] {
            for expanded in [false, true] {
                let text = rows(&card(&plan, width, &context, expanded));
                for row in &text[1..text.len() - 1] {
                    assert_eq!(row.width(), width, "{row:?} at {width}");
                }
            }
        }
        let text = rows(&card(&plan, 17, &context, false));
        assert_eq!(text[1], "╭─ Plan  Ship… ─╮");
        assert_eq!(text[text.len() - 2], "╰─ Expand plan ─╯");
        let text = rows(&card(&plan, 17, &context, true));
        assert_eq!(text[text.len() - 2], "╰ Collapse plan ╯");
        // Tabs and other control characters become spaces, so the right edge stays in line.
        let tabbed = rows(&card("# A\tB\n\nx\ty", 20, &context, false));
        assert!(tabbed[1].contains(" A B "), "{tabbed:?}");
        assert!(tabbed[2].starts_with("│ x y "), "{tabbed:?}");
        assert_eq!(tabbed[2].width(), 20);
    }

    #[test]
    fn a_card_counts_as_in_view_while_any_row_from_header_to_button_shows() {
        // A card whose header is row 1 and whose button is row 30, in a transcript that starts
        // on screen row 5.
        let toggle = (1, 30);
        // All of it, drawn from transcript row 2.
        let card = drawn("p", toggle, 0, 32, 2, 5).expect("in view");
        assert_eq!(card.header, 3);
        assert_eq!(card.rows, [Some(8), Some(37)]);
        // Scrolled so only the body shows. It still counts, with its header above the screen.
        let card = drawn("p", toggle, 10, 25, 0, 5).expect("body in view");
        assert_eq!(card.header, -9);
        assert_eq!(card.rows, [None, None]);
        // Only the button.
        let card = drawn("p", toggle, 30, 32, 0, 5).expect("button in view");
        assert_eq!(card.header, -29);
        assert_eq!(card.rows, [None, Some(5)]);
        // Only a blank row around it.
        assert!(drawn("p", toggle, 31, 32, 0, 5).is_none());
        assert!(drawn("p", toggle, 0, 1, 0, 5).is_none());
    }

    #[test]
    fn of_several_cards_the_first_in_view_is_the_target() {
        // Three long cards of 33 rows each, header at 1 and button at 31, stacked from
        // transcript row 0. The frame shows transcript rows 40..60 from screen row 0: the
        // first card is above it, the second shows its rows 7..27 and the third none.
        let toggle = (1, 31);
        let cards: Vec<Drawn> = [("a", 33, 33), ("b", 7, 27), ("c", 0, 0)]
            .into_iter()
            .filter_map(|(id, from, to)| drawn(id, toggle, from, to, 0, 0))
            .collect();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, "b");
        assert_eq!(cards[0].header, -6);

        // Two cards in view: `p` takes the first, and a click finds each by its rows only.
        let cards: Vec<Drawn> = [("a", 20, 33, 0), ("b", 0, 7, 13)]
            .into_iter()
            .filter_map(|(id, from, to, base)| drawn(id, toggle, from, to, base, 0))
            .collect();
        assert_eq!(
            cards.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(at(&cards, 11).map(|c| c.id.as_str()), Some("a"));
        assert_eq!(at(&cards, 14).map(|c| c.id.as_str()), Some("b"));
        assert!(at(&cards, 3).is_none(), "a body row toggles nothing");
    }

    #[test]
    fn each_card_flips_on_its_own_and_keeps_its_place() {
        let mut expanded = HashSet::new();
        assert_eq!(flip(&mut expanded, "a", 4), 4);
        // Expanding keeps a header that is above the screen where it is.
        assert_eq!(flip(&mut expanded, "b", -3), -3);
        assert!(expanded.contains("a") && expanded.contains("b"));
        assert_eq!(flip(&mut expanded, "a", 6), 6);
        assert!(!expanded.contains("a") && expanded.contains("b"));
        // Collapsing brings it back to the top row.
        assert_eq!(flip(&mut expanded, "b", -12), 0);
        assert!(expanded.is_empty());
    }

    #[test]
    fn plans_that_leave_the_thread_are_forgotten() {
        let block = |id: &str, item_type: &str| transcript::Block {
            item_id: id.into(),
            kind: BlockKind::Plan,
            header: String::new(),
            body: String::new(),
            streaming: false,
            status: String::new(),
            item_type: item_type.into(),
            detail: String::new(),
            title: String::new(),
            output_omitted: false,
            updated_at: String::new(),
            exit_code: None,
            run_id: String::new(),
            tool_name: String::new(),
            request_id: String::new(),
        };
        let mut expanded: HashSet<String> = ["kept", "gone", "todo"]
            .into_iter()
            .map(String::from)
            .collect();
        let blocks = [block("kept", "proposed_plan"), block("todo", "todo_list")];
        keep_present(&mut expanded, &blocks);
        assert_eq!(expanded, HashSet::from(["kept".to_string()]));
    }
}
