//! Draws the visible part of `Term`'s grid into a ratatui buffer.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell as TermCell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as TermColor, CursorShape, NamedColor};
use ratatui::buffer::{Buffer, Cell, CellWidth};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use super::handler::ReplyListener;
use super::{TerminalCursor, TerminalCursorShape};

pub(super) fn render(
    term: &Term<ReplyListener>,
    area: Rect,
    buf: &mut Buffer,
) -> Option<TerminalCursor> {
    let clip = area.intersection(buf.area);
    if clip.is_empty() {
        return None;
    }
    let grid = term.grid();
    let colors = term.colors();
    let rows = grid.screen_lines();
    let columns = grid.columns();
    let offset = grid.display_offset();

    for y in clip.top()..clip.bottom() {
        let view_row = usize::from(y - area.y);
        let row = (view_row < rows).then(|| &grid[Line(view_row as i32 - offset as i32)]);
        // The previous cell drew a wide character, so this one is its hidden second column.
        let mut covered = false;
        for x in clip.left()..clip.right() {
            let Some(target) = buf.cell_mut((x, y)) else {
                continue;
            };
            target.reset();
            let view_column = usize::from(x - area.x);
            let Some(row) = row.filter(|_| view_column < columns) else {
                covered = false;
                continue;
            };
            if std::mem::take(&mut covered) {
                continue;
            }
            let cell = &row[Column(view_column)];
            style(target, cell, colors);
            let wide = cell.flags.contains(Flags::WIDE_CHAR)
                && x + 1 < clip.right()
                && view_column + 1 < columns
                && row[Column(view_column + 1)]
                    .flags
                    .contains(Flags::WIDE_CHAR_SPACER);
            covered = set_symbol(target, cell, if wide { 2 } else { 1 }) == 2;
        }
    }

    cursor(term, area, clip)
}

/// Writes the cell's character and its zero-width characters if together they take `width`
/// columns, else the character alone if it does, else a space. Returns the columns taken.
fn set_symbol(target: &mut Cell, cell: &TermCell, width: u16) -> u16 {
    let placeholder = cell
        .flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        || (width == 1 && cell.flags.contains(Flags::WIDE_CHAR));
    if placeholder || !printable(cell.c) {
        target.set_char(' ');
        return 1;
    }
    let mut base = [0; 4];
    let base = cell.c.encode_utf8(&mut base);
    if let Some(marks) = cell.zerowidth() {
        let mut symbol = String::from(&*base);
        symbol.extend(marks.iter().copied().filter(|&mark| printable(mark)));
        if symbol.len() > base.len() && symbol.cell_width() == width {
            target.set_symbol(&symbol);
            return width;
        }
    }
    if base.cell_width() == width {
        target.set_symbol(base);
        width
    } else {
        target.set_char(' ');
        1
    }
}

/// Whether a character can go into a buffer cell. `Term` writes a tab into the cells a tab
/// skips, and stores format characters, such as the ones that change text direction, as
/// zero-width characters. Neither should reach the host terminal.
fn printable(c: char) -> bool {
    !c.is_control()
        && !matches!(
            c,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
}

fn style(target: &mut Cell, cell: &TermCell, colors: &Colors) {
    target.fg = color(cell.fg, colors);
    target.bg = color(cell.bg, colors);
    target.underline_color = cell
        .underline_color()
        .map_or(Color::Reset, |c| color(c, colors));
    let flags = cell.flags;
    let mut modifier = Modifier::empty();
    for (flag, add) in [
        (Flags::BOLD, Modifier::BOLD),
        (Flags::DIM, Modifier::DIM),
        (Flags::ITALIC, Modifier::ITALIC),
        (Flags::ALL_UNDERLINES, Modifier::UNDERLINED),
        (Flags::INVERSE, Modifier::REVERSED),
        (Flags::HIDDEN, Modifier::HIDDEN),
        (Flags::STRIKEOUT, Modifier::CROSSED_OUT),
    ] {
        if flags.intersects(flag) {
            modifier |= add;
        }
    }
    target.modifier = modifier;
}

/// A color the program set with OSC 4, 10, 11 or 12 wins. Otherwise the 16 ANSI colors, and
/// their dim forms, map to ratatui's named ones so the host terminal's palette applies, and
/// the default colors map to `Reset`.
fn color(color: TermColor, colors: &Colors) -> Color {
    let index = match color {
        TermColor::Spec(rgb) => return Color::Rgb(rgb.r, rgb.g, rgb.b),
        TermColor::Indexed(index) => usize::from(index),
        TermColor::Named(name) => name as usize,
    };
    if let Some(rgb) = colors[index] {
        return Color::Rgb(rgb.r, rgb.g, rgb.b);
    }
    const DIM_BLACK: usize = NamedColor::DimBlack as usize;
    const DIM_WHITE: usize = NamedColor::DimWhite as usize;
    match (color, index) {
        (_, 0..=15) => ansi(index),
        (TermColor::Indexed(index), _) => Color::Indexed(index),
        (_, DIM_BLACK..=DIM_WHITE) => ansi(index - DIM_BLACK),
        _ => Color::Reset,
    }
}

fn ansi(index: usize) -> Color {
    match index {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

fn cursor(term: &Term<ReplyListener>, area: Rect, clip: Rect) -> Option<TerminalCursor> {
    if !term.mode().contains(TermMode::SHOW_CURSOR) {
        return None;
    }
    let style = term.cursor_style();
    let shape = match style.shape {
        CursorShape::Block | CursorShape::HollowBlock => TerminalCursorShape::Block,
        CursorShape::Underline => TerminalCursorShape::Underline,
        CursorShape::Beam => TerminalCursorShape::Bar,
        CursorShape::Hidden => return None,
    };
    let grid = term.grid();
    let point = grid.cursor.point;
    let view_row = usize::try_from(point.line.0 + grid.display_offset() as i32).ok()?;
    if view_row >= grid.screen_lines() {
        return None;
    }
    let mut column = point.column;
    if column.0 > 0
        && grid[point.line][column]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
    {
        column.0 -= 1;
    }
    let x = area.x.checked_add(u16::try_from(column.0).ok()?)?;
    let y = area.y.checked_add(u16::try_from(view_row).ok()?)?;
    (clip.left() <= x && x < clip.right() && clip.top() <= y && y < clip.bottom()).then_some(
        TerminalCursor {
            x,
            y,
            shape,
            blinking: style.blinking,
        },
    )
}
