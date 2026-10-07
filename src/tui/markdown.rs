//! A small Markdown renderer: headings, lists, quotes, fenced code, `code` and **bold** spans,
//! word-wrapped to a width. It covers what agents write in replies, not all of CommonMark.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const CODE: Style = Style::new().fg(Color::Cyan);

/// Splits a line into styled segments for `code` and **bold** markers.
fn inline_segments(text: &str, base: Style) -> Vec<(String, Style)> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let (mut bold, mut code) = (false, false);
    let mut chars = text.chars().peekable();
    let style = |bold: bool, code: bool| {
        if code {
            CODE
        } else if bold {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        }
    };
    while let Some(c) = chars.next() {
        if c == '`' {
            segments.push((std::mem::take(&mut current), style(bold, code)));
            code = !code;
        } else if !code && c == '*' && chars.peek() == Some(&'*') {
            chars.next();
            segments.push((std::mem::take(&mut current), style(bold, code)));
            bold = !bold;
        } else {
            current.push(c);
        }
    }
    segments.push((current, style(bold, code)));
    segments.retain(|(text, _)| !text.is_empty());
    segments
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
    lines.push(Line::from(spans));
    lines
}

pub fn render(text: &str, width: usize, base: Style, margin: usize) -> Vec<Line<'static>> {
    let pad = " ".repeat(margin);
    let mut lines = Vec::new();
    let mut in_code = false;
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            let mut row = String::new();
            for c in raw.chars() {
                if pad.len() + 2 + row.width() + c.width().unwrap_or(0) > width {
                    lines.push(Line::from(vec![
                        Span::raw(format!("{pad}  ")),
                        Span::styled(std::mem::take(&mut row), CODE),
                    ]));
                }
                row.push(c);
            }
            lines.push(Line::from(vec![
                Span::raw(format!("{pad}  ")),
                Span::styled(row, CODE),
            ]));
            continue;
        }
        if trimmed.is_empty() {
            lines.push(Line::default());
            continue;
        }
        let leading = raw.len() - trimmed.len();
        if let Some(heading) = trimmed.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            let style = base.add_modifier(Modifier::BOLD);
            lines.extend(wrap(
                inline_segments(heading, style),
                width,
                Span::raw(pad.clone()),
                margin,
            ));
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let bullet = format!("{pad}{}• ", " ".repeat(leading));
            let indent = bullet.width();
            lines.extend(wrap(
                inline_segments(item, base),
                width,
                Span::raw(bullet),
                indent,
            ));
        } else if let Some(quote) = trimmed
            .strip_prefix("> ")
            .or_else(|| trimmed.strip_prefix('>'))
        {
            let style = base.add_modifier(Modifier::DIM | Modifier::ITALIC);
            lines.extend(wrap(
                inline_segments(quote, style),
                width,
                Span::styled(format!("{pad}│ "), style),
                margin + 2,
            ));
        } else if let Some((number, rest)) = numbered(trimmed) {
            let label = format!("{pad}{}{number}. ", " ".repeat(leading));
            let indent = label.width();
            lines.extend(wrap(
                inline_segments(rest, base),
                width,
                Span::raw(label),
                indent,
            ));
        } else {
            lines.extend(wrap(
                inline_segments(trimmed, base),
                width,
                Span::raw(format!("{pad}{}", " ".repeat(leading))),
                margin,
            ));
        }
    }
    lines
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

    fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn wraps_words_and_keeps_inline_styles() {
        let lines = render(
            "Use **bold** and `code` here in a long sentence",
            20,
            Style::default(),
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
        let code = lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content == "code")
            .unwrap();
        assert_eq!(code.style, CODE);
    }

    #[test]
    fn renders_lists_headings_and_code_fences() {
        let text = plain(&render(
            "# Title\n- one\n1. first\n```\nlet x = 1;\n```",
            40,
            Style::default(),
            0,
        ));
        assert_eq!(text, vec!["Title", "• one", "1. first", "  let x = 1;"]);
    }
}
