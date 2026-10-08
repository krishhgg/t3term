//! A small Markdown renderer: headings, lists, quotes, rules, fenced code panels, `code`,
//! **bold**, *italic* and [links](url), word-wrapped to a width. It covers what agents write in
//! replies, not all of CommonMark.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::theme::Theme;

/// The styles one rendering uses, derived from the theme and the block's base text style.
#[derive(Clone, Copy, Debug)]
pub struct Styles {
    pub text: Style,
    pub heading: Style,
    pub code: Style,
    pub code_block: Style,
    pub code_label: Style,
    pub link: Style,
    pub quote: Style,
    pub quote_bar: Style,
    pub rule: Style,
    pub bullet: Style,
}

impl Styles {
    pub fn new(theme: &Theme, text: Style) -> Styles {
        Styles {
            text,
            heading: text.add_modifier(Modifier::BOLD),
            code: text.bg(theme.inline_code_bg),
            code_block: Style::new().fg(theme.fg).bg(theme.code_bg),
            code_label: Style::new().fg(theme.muted).bg(theme.code_bg),
            link: text.fg(theme.info_fg).add_modifier(Modifier::UNDERLINED),
            quote: Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC),
            quote_bar: Style::new().fg(theme.border_strong),
            rule: Style::new().fg(theme.border_strong),
            bullet: Style::new().fg(theme.muted),
        }
    }
}

/// Splits a line into styled segments for `code`, **bold**, *italic* and [text](url) markers.
fn inline_segments(text: &str, styles: &Styles, base: Style) -> Vec<(String, Style)> {
    let mut segments: Vec<(String, Style)> = Vec::new();
    let mut current = String::new();
    let (mut bold, mut italic, mut code) = (false, false, false);
    let chars: Vec<char> = text.chars().collect();
    let partners = Partners::scan(&chars);
    let style = |bold: bool, italic: bool, code: bool| {
        if code {
            return styles.code;
        }
        let mut style = base;
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        style
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let prev = if i == 0 { None } else { Some(chars[i - 1]) };
        if c == '`' {
            segments.push((std::mem::take(&mut current), style(bold, italic, code)));
            code = !code;
            i += 1;
            continue;
        }
        if code {
            current.push(c);
            i += 1;
            continue;
        }
        if (c == '*' || c == '_') && next == Some(c) {
            // An opening pair with no closing pair after it is literal text.
            if bold || partners.pair_at_or_after(c, i + 2) {
                segments.push((std::mem::take(&mut current), style(bold, italic, code)));
                bold = !bold;
                i += 2;
                continue;
            }
            current.push_str(&format!("{c}{c}"));
            i += 2;
            continue;
        }
        let boundary = |ch: Option<char>| ch.is_none_or(|ch| !ch.is_alphanumeric());
        if c == '*'
            || (c == '_'
                && (if italic {
                    boundary(next)
                } else {
                    boundary(prev)
                }))
        {
            // A lone star next to spaces is a bullet or math, not emphasis. So is one with no
            // closing star after it, as in `*.rs` or `2*3`.
            let opens =
                !italic && next.is_some_and(|n| !n.is_whitespace()) && partners.closes_single(c, i);
            let closes = italic && prev.is_some_and(|p| !p.is_whitespace());
            if opens || closes {
                segments.push((std::mem::take(&mut current), style(bold, italic, code)));
                italic = !italic;
                i += 1;
                continue;
            }
        }
        if c == '['
            && let Some((label, url, end)) = link_at(&chars, i, &partners)
        {
            segments.push((std::mem::take(&mut current), style(bold, italic, code)));
            // The terminal can't open the label, so the destination stays readable and
            // copyable after it.
            let shown = url
                .trim_start_matches("https://")
                .trim_start_matches("http://");
            let same = label == url || label == shown;
            segments.push((label, styles.link));
            if !same && !url.is_empty() {
                segments.push((format!(" ({url})"), styles.bullet));
            }
            i = end;
            continue;
        }
        current.push(c);
        i += 1;
    }
    segments.push((current, style(bold, italic, code)));

    // Bare URLs get the link color too.
    let mut out = Vec::new();
    for (text, style) in segments {
        if style == styles.code || !text.contains("http") {
            out.push((text, style));
            continue;
        }
        for word in text.split_inclusive(' ') {
            let trimmed = word.trim_end();
            if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                out.push((trimmed.to_string(), styles.link));
                out.push((word[trimmed.len()..].to_string(), style));
            } else {
                out.push((word.to_string(), style));
            }
        }
    }
    out.retain(|(text, _)| !text.is_empty());
    out
}

/// Where each marker's possible partners sit on a line, found in one pass. Searching the rest
/// of the line from every opening marker instead takes quadratic time on a long line full of
/// markers that never close.
struct Partners {
    /// Start of the last `**` and of the last `__`.
    last_pair: [Option<usize>; 2],
    /// The last `*` and the last `_` that can close an italic: right after text, and for `_`
    /// not inside a word.
    last_single: [Option<usize>; 2],
    /// For each index, the first `]` and the first `)` at or after it.
    next_bracket: Vec<Option<usize>>,
    next_paren: Vec<Option<usize>>,
}

impl Partners {
    fn scan(chars: &[char]) -> Self {
        let mut partners = Partners {
            last_pair: [None; 2],
            last_single: [None; 2],
            next_bracket: vec![None; chars.len() + 1],
            next_paren: vec![None; chars.len() + 1],
        };
        for (j, &c) in chars.iter().enumerate() {
            let Some(slot) = emphasis_slot(c) else {
                continue;
            };
            if chars.get(j + 1) == Some(&c) {
                partners.last_pair[slot] = Some(j);
            }
            if j > 0
                && chars[j - 1] != c
                && !chars[j - 1].is_whitespace()
                && (c == '*' || chars.get(j + 1).is_none_or(|n| !n.is_alphanumeric()))
            {
                partners.last_single[slot] = Some(j);
            }
        }
        for j in (0..chars.len()).rev() {
            partners.next_bracket[j] = if chars[j] == ']' {
                Some(j)
            } else {
                partners.next_bracket[j + 1]
            };
            partners.next_paren[j] = if chars[j] == ')' {
                Some(j)
            } else {
                partners.next_paren[j + 1]
            };
        }
        partners
    }

    /// Whether a `**` or `__` starts at or after `from`.
    fn pair_at_or_after(&self, marker: char, from: usize) -> bool {
        emphasis_slot(marker)
            .and_then(|slot| self.last_pair[slot])
            .is_some_and(|j| j >= from)
    }

    /// Whether a single `*` or `_` opening at `open` has a closing one later on the line.
    fn closes_single(&self, marker: char, open: usize) -> bool {
        emphasis_slot(marker)
            .and_then(|slot| self.last_single[slot])
            .is_some_and(|j| j >= open + 2)
    }
}

fn emphasis_slot(c: char) -> Option<usize> {
    match c {
        '*' => Some(0),
        '_' => Some(1),
        _ => None,
    }
}

/// `[label](url)` starting at `start`: the label, the destination and the index after the
/// closing paren.
fn link_at(chars: &[char], start: usize, partners: &Partners) -> Option<(String, String, usize)> {
    let close = partners.next_bracket[start + 1]?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = partners.next_paren[close + 2]?;
    let label: String = chars[start + 1..close].iter().collect();
    if label.is_empty() {
        return None;
    }
    let url: String = chars[close + 2..end].iter().collect();
    Some((label, url.trim().to_string(), end + 1))
}

/// Greedy word wrap that keeps each word's style. `prefix` starts the first row, `indent` the rest.
fn wrap(
    segments: Vec<(String, Style)>,
    width: usize,
    prefix: Span<'static>,
    indent: usize,
) -> Vec<Line<'static>> {
    let width = width.max(indent + 8);
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = vec![prefix.clone()];
    let mut used = prefix.content.width();
    let mut line_has_word = false;
    for (text, style) in segments {
        // Split on spaces but keep them, so spans join back without losing spacing.
        for word in text.split_inclusive(' ') {
            let word_width = word.trim_end().width();
            if line_has_word && used + word_width > width {
                trim_end(&mut spans);
                lines.push(Line::from(std::mem::take(&mut spans)));
                spans.push(Span::raw(" ".repeat(indent)));
                used = indent;
                line_has_word = false;
            }
            let word = if line_has_word {
                word.to_string()
            } else {
                word.trim_start().to_string()
            };
            if word.is_empty() {
                continue;
            }
            // A single word wider than the row: hard-break it by characters.
            if word.width() > width - indent {
                let mut chunk = String::new();
                for c in word.chars() {
                    let w = c.width().unwrap_or(0);
                    if used + chunk.width() + w > width {
                        spans.push(Span::styled(std::mem::take(&mut chunk), style));
                        lines.push(Line::from(std::mem::take(&mut spans)));
                        spans.push(Span::raw(" ".repeat(indent)));
                        used = indent;
                    }
                    chunk.push(c);
                }
                used += chunk.width();
                spans.push(Span::styled(chunk, style));
            } else {
                used += word.width();
                spans.push(Span::styled(word, style));
            }
            line_has_word = true;
        }
    }
    if line_has_word {
        trim_end(&mut spans);
    }
    lines.push(Line::from(spans));
    lines
}

/// Drops the space after a row's last word, which would otherwise stick out past the width.
fn trim_end(spans: &mut [Span<'static>]) {
    if let Some(last) = spans.last_mut() {
        let trimmed = last.content.trim_end();
        if trimmed.len() < last.content.len() {
            last.content = trimmed.to_string().into();
        }
    }
}

fn pad_to(text: &str, width: usize) -> String {
    let mut out = text.to_string();
    let short = width.saturating_sub(out.width());
    out.extend(std::iter::repeat_n(' ', short));
    out
}

/// Renders Markdown into rows no wider than `width`, with `margin` blank columns on the left.
pub fn render(text: &str, width: usize, styles: &Styles, margin: usize) -> Vec<Line<'static>> {
    let base = styles.text;
    let pad = " ".repeat(margin);
    let mut lines = Vec::new();
    let mut in_code = false;
    let panel_width = width.saturating_sub(margin).max(8);
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            if in_code {
                // The fence's language names the panel, like the GUI's code block header.
                let language = trimmed.trim_start_matches('`').trim();
                lines.push(Line::from(vec![
                    Span::styled(pad.clone(), base),
                    Span::styled(
                        pad_to(&format!(" {language}"), panel_width),
                        styles.code_label,
                    ),
                ]));
            }
            continue;
        }
        if in_code {
            let mut row = String::from(" ");
            for c in raw.chars() {
                if row.width() + c.width().unwrap_or(0) > panel_width {
                    lines.push(Line::from(vec![
                        Span::styled(pad.clone(), base),
                        Span::styled(
                            pad_to(&std::mem::replace(&mut row, " ".into()), panel_width),
                            styles.code_block,
                        ),
                    ]));
                }
                row.push(c);
            }
            lines.push(Line::from(vec![
                Span::styled(pad.clone(), base),
                Span::styled(pad_to(&row, panel_width), styles.code_block),
            ]));
            continue;
        }
        if trimmed.is_empty() {
            lines.push(Line::default());
            continue;
        }
        // Deep indentation keeps at least 8 columns for text, so no row grows past `width`.
        let leading = (raw.len() - trimmed.len()).min(width.saturating_sub(margin + 13));
        if is_rule(trimmed) {
            lines.push(Line::from(vec![
                Span::styled(pad.clone(), base),
                Span::styled("─".repeat(panel_width), styles.rule),
            ]));
        } else if let Some(heading) = trimmed.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            lines.extend(wrap(
                inline_segments(heading, styles, styles.heading),
                width,
                Span::raw(pad.clone()),
                margin,
            ));
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            let indent = margin + leading + 2;
            lines.extend(wrap(
                inline_segments(item, styles, base),
                width,
                Span::styled(format!("{pad}{}• ", " ".repeat(leading)), styles.bullet),
                indent,
            ));
        } else if let Some(quote) = trimmed
            .strip_prefix("> ")
            .or_else(|| trimmed.strip_prefix('>'))
        {
            lines.extend(wrap(
                inline_segments(quote, styles, styles.quote),
                width,
                Span::styled(format!("{pad}▎ "), styles.quote_bar),
                margin + 2,
            ));
        } else if let Some((number, rest)) = numbered(trimmed) {
            let label = format!("{pad}{}{number}. ", " ".repeat(leading));
            let indent = label.width();
            lines.extend(wrap(
                inline_segments(rest, styles, base),
                width,
                Span::styled(label, styles.bullet),
                indent,
            ));
        } else {
            lines.extend(wrap(
                inline_segments(trimmed, styles, base),
                width,
                Span::raw(format!("{pad}{}", " ".repeat(leading))),
                margin,
            ));
        }
    }
    lines
}

fn is_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3
        && ["---", "***", "___"]
            .iter()
            .any(|r| compact.chars().all(|c| r.starts_with(c)))
}

fn numbered(line: &str) -> Option<(&str, &str)> {
    let digits = line.find(|c: char| !c.is_ascii_digit())?;
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = line[digits..].strip_prefix(". ")?;
    Some((&line[..digits], rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Depth;

    fn styles() -> Styles {
        Styles::new(&Theme::new(Depth::TrueColor), Style::default())
    }

    fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn span_style(lines: &[Line], text: &str) -> Style {
        lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content == text)
            .unwrap_or_else(|| panic!("no span {text:?}"))
            .style
    }

    #[test]
    fn wraps_words_and_keeps_inline_styles() {
        let styles = styles();
        let lines = render(
            "Use **bold** and `code` here in a long sentence",
            20,
            &styles,
            0,
        );
        let text = plain(&lines);
        assert!(text.iter().all(|l| l.width() <= 20), "{text:?}");
        assert_eq!(
            text.join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            "Use bold and code here in a long sentence"
        );
        assert_eq!(span_style(&lines, "code"), styles.code);
        assert!(
            span_style(&lines, "bold").add_modifier(Modifier::BOLD) == span_style(&lines, "bold")
        );
    }

    #[test]
    fn renders_lists_headings_and_code_fences() {
        let styles = styles();
        let lines = render(
            "# Title\n- one\n1. first\n```python\nlet x = 1;\n```\n---",
            40,
            &styles,
            0,
        );
        let text: Vec<String> = plain(&lines)
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        let rule = "─".repeat(40);
        assert_eq!(
            text,
            vec![
                "Title",
                "• one",
                "1. first",
                " python",
                " let x = 1;",
                &rule
            ]
        );
        // Code panels paint their background across the full width.
        assert_eq!(lines[4].spans[1].content.width(), 40);
        assert_eq!(lines[4].spans[1].style, styles.code_block);
        assert_eq!(lines[3].spans[1].style, styles.code_label);
    }

    #[test]
    fn styles_links_italics_and_quotes() {
        let styles = styles();
        let lines = render(
            "See [docs](https://example.com) or https://t3.gg now, *soft* snake_case\n> quoted",
            80,
            &styles,
            0,
        );
        assert_eq!(span_style(&lines, "docs"), styles.link);
        assert_eq!(span_style(&lines, "https://t3.gg"), styles.link);
        assert!(
            span_style(&lines, "soft").add_modifier(Modifier::ITALIC) == span_style(&lines, "soft")
        );
        assert_eq!(span_style(&lines, "snake_case"), styles.text);
        assert_eq!(
            plain(&lines)[0],
            "See docs (https://example.com) or https://t3.gg now, soft snake_case"
        );
        assert_eq!(plain(&lines)[1], "▎ quoted");
        assert_eq!(span_style(&lines, "quoted"), styles.quote);
    }

    #[test]
    fn keeps_link_destinations_and_unmatched_markers() {
        let styles = styles();
        let lines = render(
            "Read [the docs](https://example.com/a) or [t3.gg](https://t3.gg)",
            80,
            &styles,
            0,
        );
        assert_eq!(
            plain(&lines)[0],
            "Read the docs (https://example.com/a) or t3.gg"
        );
        assert_eq!(span_style(&lines, "docs"), styles.link);

        let lines = render(
            "Match *.rs files\n2*3 is six\n**open bold\nbut *this* and ***both***",
            80,
            &styles,
            0,
        );
        assert_eq!(
            plain(&lines),
            [
                "Match *.rs files",
                "2*3 is six",
                "**open bold",
                "but this and both"
            ]
        );
        let italic = |text| span_style(&lines, text).add_modifier(Modifier::ITALIC);
        assert_eq!(italic("this"), span_style(&lines, "this"));
        assert_eq!(
            span_style(&lines, "both"),
            styles.text.add_modifier(Modifier::BOLD | Modifier::ITALIC)
        );
    }

    #[test]
    fn long_lines_of_unmatched_markers_render_in_linear_time() {
        // Each opening marker used to search the rest of the line for a partner, so a line
        // like this took minutes.
        let text = "_name *star [link ".repeat(20_000);
        let started = std::time::Instant::now();
        let lines = render(&text, 80, &styles(), 0);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(plain(&lines)[0].starts_with("_name *star [link _name"));
    }

    #[test]
    fn deep_indentation_stays_inside_the_width() {
        let text = format!("{}- a deeply nested list item with words", " ".repeat(24));
        for width in [20, 28, 40] {
            let lines = render(&text, width, &styles(), 0);
            assert!(
                lines.iter().all(|l| l.width() <= width),
                "{width}: {:?}",
                plain(&lines)
            );
        }
    }
}
