//! The desktop's Change theme list, which its Cmd+Option+A opens in the command palette, as a
//! dialog that Ctrl+Alt+A opens. It lists T3 Code's own colors and then the built-in themes, in
//! the desktop's order. The row of the theme that draws says Current, and each row ends with a
//! dot for each of the theme's halves, light then dark, as the desktop ends it with a ball for
//! each. A dot is the half's focus color on its canvas, where the desktop's ball is the canvas
//! lit by its accent, which is the focus color in all but T3 Chat's dark half.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::theme::{Appearance, Theme};
use super::{clip, row, with_bg};
use crate::settings::ThemeName;

/// The widest the dialog draws, its border included.
const WIDTH: u16 = 40;
/// The narrowest and shortest dialog that draws a border. A smaller one draws without.
const BORDER_MIN_WIDTH: u16 = 20;
const BORDER_MIN_HEIGHT: u16 = 5;
/// What follows the row of the theme that draws.
const CURRENT: &str = "Current";

/// The open list.
#[derive(Debug, Default)]
pub struct ThemeList {
    /// The highlighted row, a place in `ThemeName::ALL`. It opens on the first, as the
    /// desktop's palette does.
    selected: usize,
    /// Where the last frame drew the list, for the mouse. `None` until a frame draws it, and
    /// after a resize until the next one does.
    pub drawn: Option<Drawn>,
}

/// Where a frame drew the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drawn {
    /// The whole dialog.
    pub area: Rect,
    /// The × and the blank column on each side of it. Empty when there was no room for it.
    pub close: Rect,
    /// The rows of themes.
    rows: Rect,
    /// The place in `ThemeName::ALL` of the theme on the first of `rows`.
    first: usize,
}

impl ThemeList {
    /// The highlighted theme.
    pub fn selected(&self) -> ThemeName {
        ThemeName::ALL[self.selected]
    }

    /// Moves the highlight `delta` rows, stopping at the first and the last.
    pub fn select_by(&mut self, delta: isize) {
        let last = ThemeName::ALL.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    /// The theme on the row at `column` and `row` of the screen, as the last frame drew it.
    pub fn at(&self, column: u16, row: u16) -> Option<ThemeName> {
        let drawn = self.drawn?;
        let rows = drawn.rows;
        let inside =
            (rows.x..rows.right()).contains(&column) && (rows.y..rows.bottom()).contains(&row);
        inside
            .then(|| ThemeName::ALL.get(drawn.first + usize::from(row - rows.y)))?
            .copied()
    }

    /// Draws the list over `screen`, centered across it and a third of the way down, in
    /// `theme`'s colors, with `theme`'s own row marked Current.
    pub fn draw(&mut self, frame: &mut Frame, screen: Rect, theme: &Theme) {
        let count = ThemeName::ALL.len() as u16;
        let width = if screen.width >= BORDER_MIN_WIDTH + 2 {
            (screen.width - 2).min(WIDTH)
        } else {
            screen.width
        };
        let bordered = width >= BORDER_MIN_WIDTH && screen.height >= BORDER_MIN_HEIGHT;
        // The border's rows and the title row.
        let frame_rows = if bordered { 3 } else { 1 };
        let height = (count + frame_rows).min(screen.height);
        let area = Rect {
            x: screen.x + (screen.width - width) / 2,
            y: screen.y + (screen.height - height) / 3,
            width,
            height,
        };
        // A column of space inside each side of the border.
        let inner = if bordered {
            Rect {
                x: area.x + 2,
                y: area.y + 1,
                width: area.width - 4,
                height: area.height - 2,
            }
        } else {
            area
        };
        if inner.width == 0 || inner.height == 0 {
            self.drawn = None;
            return;
        }
        let text_width = usize::from(inner.width);
        let muted = Style::new().fg(theme.muted);

        let mut lines = Vec::with_capacity(usize::from(inner.height));
        let close = if text_width >= 4 { " ×" } else { "" };
        let title = clip("Change theme", text_width.saturating_sub(close.width()));
        lines.push(row(
            vec![Span::styled(
                title,
                Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
            )],
            vec![Span::styled(close, muted)],
            text_width,
        ));
        // The highlighted row stays in view on a screen too short for all of them.
        let visible = usize::from(inner.height - 1);
        let first = (self.selected + 1).saturating_sub(visible);
        let shown = ThemeName::ALL.iter().enumerate().skip(first).take(visible);
        for (index, &name) in shown {
            let line = theme_row(name, name == theme.name, text_width, theme);
            if index == self.selected {
                lines.push(Line::from(with_bg(line.spans, theme.highlight)));
            } else {
                lines.push(line);
            }
        }

        frame.render_widget(Clear, area);
        let style = Style::new().fg(theme.fg).bg(theme.popover);
        let block = Block::new().style(style);
        let block = match bordered {
            true => block
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(theme.border_strong)),
            false => block,
        };
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(lines).style(style), inner);
        let close = if text_width >= 4 {
            Rect::new(inner.right() - 2, inner.y, 3, 1).intersection(area)
        } else {
            Rect::default()
        };
        let rows = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
        self.drawn = Some(Drawn {
            area,
            close,
            rows,
            first,
        });
    }
}

/// `name`'s row, `width` columns wide: its name, then Current where `current`, and a dot for
/// each half. On a row too narrow for all of it, Current goes first and then the dots.
fn theme_row(name: ThemeName, current: bool, width: usize, theme: &Theme) -> Line<'static> {
    let dot = |appearance| {
        let half = Theme::of(name, appearance, theme.depth);
        Span::styled(" ● ", Style::new().fg(half.focus).bg(half.bg))
    };
    let dots = vec![
        dot(Appearance::Light),
        Span::raw(" "),
        dot(Appearance::Dark),
    ];
    let mut right = Vec::new();
    if current {
        right.push(Span::styled(CURRENT, Style::new().fg(theme.muted)));
        right.push(Span::raw("  "));
    }
    right.extend(dots.iter().cloned());
    let label = name.label();
    let room = |right: &[Span]| label.width() + 1 + right.iter().map(|s| s.width()).sum::<usize>();
    if room(&right) > width {
        right = dots;
    }
    if room(&right) > width {
        right = Vec::new();
    }
    let left = vec![Span::styled(clip(label, width), Style::new().fg(theme.fg))];
    row(left, right, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_highlight_stops_at_the_ends() {
        let mut list = ThemeList::default();
        assert_eq!(list.selected(), ThemeName::T3Code);
        list.select_by(-1);
        assert_eq!(list.selected(), ThemeName::T3Code);
        list.select_by(2);
        assert_eq!(list.selected(), ThemeName::Grove);
        list.select_by(isize::MAX);
        assert_eq!(list.selected(), ThemeName::Iris);
        list.select_by(1);
        assert_eq!(list.selected(), ThemeName::Iris);
        list.select_by(-isize::MAX);
        assert_eq!(list.selected(), ThemeName::T3Code);
    }

    #[test]
    fn a_narrow_row_drops_current_and_then_the_dots() {
        let theme = Theme::new(super::super::theme::Depth::TrueColor);
        let text = |width| {
            let line = theme_row(ThemeName::Ember, true, width, &theme);
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert_eq!(text(30), "Ember         Current   ●   ● ");
        assert_eq!(text(20), "Ember         ●   ● ");
        assert_eq!(text(12), "Ember       ");
        assert_eq!(text(3), "Em…");
        for width in [3, 12, 20, 30] {
            assert_eq!(text(width).width(), width);
        }
    }
}
