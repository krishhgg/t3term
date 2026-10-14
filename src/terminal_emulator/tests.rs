//! Tests that feed long or arbitrary output check it with `assert!`, so a failure names the
//! case and the position instead of printing the output.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Cell as TermCell;
use alacritty_terminal::term::color::COUNT;
use alacritty_terminal::term::{Osc52, TermMode};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use super::guard::{MAX_OSC_PAYLOAD, OscGuard};
use super::handler::{
    GuardedTerm, MAX_PENDING_REPLIES, MAX_PENDING_REPLY_BYTES, MAX_ZERO_WIDTH_PER_CELL,
    ReplyListener,
};
use super::{
    InputModes, MAX_COLUMNS, MAX_ROWS, MIN_COLUMNS, MIN_ROWS, MouseEncoding, MouseTracking,
    TerminalCursor, TerminalCursorShape, TerminalEmulator, config,
};

/// A bare `Term` and parser, driven the way alacritty_terminal drives them, with replies kept.
struct Bare {
    term: Term<ReplyListener>,
    processor: Processor<StdSyncHandler>,
    listener: ReplyListener,
}

impl Bare {
    fn new(columns: u16, rows: u16) -> Self {
        let listener = ReplyListener::default();
        listener.queue().accepting = true;
        let size = (usize::from(rows), usize::from(columns));
        Self {
            term: Term::new(config(columns), &size, listener.clone()),
            processor: Processor::new(),
            listener,
        }
    }

    fn feed(&mut self, output: &[u8]) {
        self.processor.advance(&mut self.term, output);
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        self.term.set_options(config(columns));
        self.term.resize((usize::from(rows), usize::from(columns)));
    }

    fn flush_sync(&mut self) {
        if self.processor.sync_timeout().sync_timeout().is_some() {
            self.processor.stop_sync(&mut self.term);
        }
    }

    fn take_replies(&self) -> String {
        std::mem::take(&mut self.listener.queue().text)
    }
}

/// An emulator with write access, so queries get replies.
fn live(columns: u16, rows: u16) -> TerminalEmulator {
    let mut emulator = TerminalEmulator::new(columns, rows);
    emulator.set_write_access(true);
    emulator
}

fn feed_both(columns: u16, rows: u16, output: &[u8]) -> (Bare, TerminalEmulator) {
    let mut bare = Bare::new(columns, rows);
    bare.feed(output);
    let mut emulator = live(columns, rows);
    emulator.engine.feed(output);
    (bare, emulator)
}

/// Everything about a cell a viewer can see. Hyperlinks are left out: the emulator drops them.
fn same_cell(a: &TermCell, b: &TermCell) -> bool {
    a.c == b.c
        && a.fg == b.fg
        && a.bg == b.bg
        && a.flags == b.flags
        && a.zerowidth().unwrap_or_default() == b.zerowidth().unwrap_or_default()
        && a.underline_color() == b.underline_color()
}

/// Every cell, the scrollback included, both cursors, the modes and the cursor style.
fn assert_same_screen<A, B>(case: &str, expected: &Term<A>, actual: &Term<B>) {
    let (want, got) = (expected.grid(), actual.grid());
    assert_eq!(
        (want.columns(), want.screen_lines(), want.history_size()),
        (got.columns(), got.screen_lines(), got.history_size()),
        "{case}: grid size"
    );
    assert_eq!(
        want.display_offset(),
        got.display_offset(),
        "{case}: view offset"
    );
    for line in want.topmost_line().0..=want.bottommost_line().0 {
        for column in 0..want.columns() {
            let (a, b) = (
                &want[Line(line)][Column(column)],
                &got[Line(line)][Column(column)],
            );
            assert!(
                same_cell(a, b),
                "{case}: cell at line {line}, column {column}"
            );
        }
    }
    for (name, a, b) in [
        ("cursor", &want.cursor, &got.cursor),
        ("saved cursor", &want.saved_cursor, &got.saved_cursor),
    ] {
        assert_eq!(a.point, b.point, "{case}: {name} position");
        assert_eq!(
            a.input_needs_wrap, b.input_needs_wrap,
            "{case}: {name} pending wrap"
        );
        assert_eq!(a.charsets, b.charsets, "{case}: {name} character sets");
        assert!(
            same_cell(&a.template, &b.template),
            "{case}: {name} attributes"
        );
    }
    assert_eq!(expected.mode(), actual.mode(), "{case}: modes");
    assert_eq!(
        expected.cursor_style(),
        actual.cursor_style(),
        "{case}: cursor style"
    );
}

fn assert_same_term<A, B>(case: &str, expected: &Term<A>, actual: &Term<B>) {
    assert_same_screen(case, expected, actual);
    for index in 0..COUNT {
        assert_eq!(
            expected.colors()[index],
            actual.colors()[index],
            "{case}: color {index}"
        );
    }
}

fn assert_no_hyperlinks<T>(case: &str, term: &Term<T>) {
    let grid = term.grid();
    for line in grid.topmost_line().0..=grid.bottommost_line().0 {
        for column in 0..grid.columns() {
            assert!(
                grid[Line(line)][Column(column)].hyperlink().is_none(),
                "{case}: hyperlink at line {line}, column {column}"
            );
        }
    }
    assert!(
        grid.cursor.template.hyperlink().is_none(),
        "{case}: hyperlink in the cursor's attributes"
    );
}

/// Feeds `output` to a bare `Term` and to the emulator in one call each, and checks they end
/// in the same state with the same replies.
fn assert_matches_bare(case: &str, columns: u16, rows: u16, output: &[u8]) {
    let (bare, mut emulator) = feed_both(columns, rows, output);
    assert_same_term(case, &bare.term, &emulator.engine.term);
    assert!(
        bare.take_replies() == emulator.take_replies().unwrap_or_default(),
        "{case}: replies differ"
    );
    assert_no_hyperlinks(case, &emulator.engine.term);
}

/// Splits `text` into pieces of `size` characters.
fn pieces(text: &str, size: usize) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let end = rest
            .char_indices()
            .nth(size)
            .map_or(rest.len(), |(index, _)| index);
        let (piece, tail) = rest.split_at(end);
        pieces.push(piece);
        rest = tail;
    }
    pieces
}

/// Feeds `output` whole to one emulator and in pieces of each size to others, and checks they
/// all end in the same state with the same replies.
fn assert_split_matches_whole(case: &str, columns: u16, rows: u16, output: &str) {
    let mut whole = live(columns, rows);
    whole.apply(output);
    let replies = whole.take_replies();
    for size in [1, 2, 3, 7, 64, 4095, 4097] {
        let mut split = live(columns, rows);
        for piece in pieces(output, size) {
            split.apply(piece);
        }
        let case = format!("{case}, pieces of {size}");
        assert_same_term(&case, &whole.engine.term, &split.engine.term);
        assert!(split.take_replies() == replies, "{case}: replies differ");
    }
}

fn draw(emulator: &TerminalEmulator) -> Buffer {
    let (columns, rows) = emulator.size();
    let area = Rect::new(0, 0, columns, rows);
    let mut buf = Buffer::empty(area);
    emulator.render(area, &mut buf);
    buf
}

fn rows(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (area.top()..area.bottom())
        .map(|y| {
            (area.left()..area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect()
        })
        .collect()
}

fn screen(emulator: &TerminalEmulator) -> Vec<String> {
    rows(&draw(emulator))
}

fn marks(emulator: &TerminalEmulator, line: i32, column: usize) -> usize {
    emulator.engine.term.grid()[Line(line)][Column(column)]
        .zerowidth()
        .map_or(0, <[char]>::len)
}

fn history(emulator: &TerminalEmulator) -> usize {
    emulator.engine.term.grid().history_size()
}

/// Bidirectional formatting characters, which the renderer keeps out of symbols.
fn bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

fn assert_clean_symbols(case: &str, buf: &Buffer) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let clean = buf[(x, y)]
                .symbol()
                .chars()
                .all(|c| !c.is_control() && !bidi_control(c));
            assert!(clean, "{case}: control character in the cell at {x}, {y}");
        }
    }
}

/// One or more uses of every `Handler` method the parser calls, except titles and hyperlinks,
/// which the emulator drops on purpose.
fn every_handler_call() -> String {
    let mut output = String::new();
    for part in [
        // reset_state, then text with a wrap, a wide character and zero-width characters.
        "\x1bc",
        "plain text that wraps past the edge",
        "e\u{301}\u{302} 漢\u{301}字 ",
        // REP repeats the last character printed, a combining one included.
        "x\x1b[3b\u{301}\x1b[2b",
        // put_tab, backspace, carriage_return, linefeed from LF, VT and FF, bell, substitute.
        "\t|\x08\r\n\x0b\x0c\x07\x1a",
        // Tab stops: set_horizontal_tabstop, clear_tabs, set_tabs, the tab movements.
        "\x1bH\x1b[g\x1b[3g\x1b[?5W\x1b[2I\x1b[Z\t",
        // Cursor movement.
        "\x1b[5;3H\x1b[2d\x1b[7G\x1b[A\x1b[2B\x1b[3C\x1b[2D\x1b[E\x1b[F\x1b[e\x1b[a\x1b[`",
        // Replies: identify_terminal, device_status, text_area_size_chars.
        "\x1b[c\x1b[>c\x1bZ\x1b[5n\x1b[6n\x1b[18t",
        // text_area_size_pixels, which only a window could answer.
        "\x1b[14t",
        // Editing.
        "\x1b[2@abc\x1b[3X\x1b[P\x1b[L\x1b[M\x1b[2S\x1b[T\x1bM\x1bD\x1bE",
        "\x1b7\x1b[4;4Hsaved\x1b8\x1b[s\x1b[2;2H\x1b[u",
        "line\x1b[K\x1b[1K\x1b[2Kmore\x1b[J\x1b[1J",
        // terminal_attribute: every SGR the parser knows.
        "\x1b[1;2;3;4;5;6;7;8;9mA\x1b[4:2mB\x1b[4:3mC\x1b[4:4mD\x1b[4:5mE",
        "\x1b[21;22;23;24;25;27;28;29mF\x1b[31;42mG\x1b[91;102mH",
        "\x1b[38;5;200;48;5;17mI\x1b[38;2;1;2;3;48;2;4;5;6mJ",
        "\x1b[58;2;9;8;7mK\x1b[58;5;99mL\x1b[59mM\x1b[39;49mN\x1b[0m",
        // set_mode, unset_mode and report_mode.
        "\x1b[4hins\x1b[4l\x1b[20h\n\x1b[20l\x1b[4$p\x1b[20$p\x1b[99$p",
        // Private modes, set and unset, and their reports.
        "\x1b[?1h\x1b[?1l\x1b[?1h\x1b[?6h\x1b[2;2H\x1b[?6l\x1b[?7lno wrap past here\x1b[?7h",
        "\x1b[?12h\x1b[?12l\x1b[?25l\x1b[?25h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1003l",
        "\x1b[?1005h\x1b[?1006h\x1b[?1004h\x1b[?1007l\x1b[?1007h\x1b[?2004h\x1b[?1042h",
        "\x1b[?1$p\x1b[?7$p\x1b[?12$p\x1b[?1006$p\x1b[?2004$p\x1b[?2026$p\x1b[?3$p\x1b[?9$p",
        // The alternate screen, with text on it.
        "\x1b[?1049halt screen\x1b[?1049l\x1b[?1049h\x1b[?1049l",
        // set_scrolling_region, with scrolling inside it.
        "\x1b[2;4r\x1b[4;1H\n\n\nregion\x1b[r",
        // Keypad modes and character sets.
        "\x1b=\x1b>\x1b=\x1b)0\x0eqqq\x0f\x1b(0lqk\x1b(B",
        // set_color, dynamic_color_sequence, reset_color.
        "\x1b]4;1;rgb:12/34/56\x07\x1b]4;3;#abcdef;5;#123\x1b\\",
        "\x1b]10;#abcdef\x07\x1b]11;rgb:01/02/03\x07\x1b]12;#ff0000\x07",
        "\x1b]4;2;?\x07\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]104;3\x07\x1b]112\x07",
        // clipboard_store and clipboard_load, which the config already refuses.
        "\x1b]52;c;aGVsbG8=\x07\x1b]52;c;?\x07",
        // Cursor shape and style, and the mouse pointer.
        "\x1b]50;CursorShape=1\x07\x1b[2 q\x1b[5 q\x1b[ q\x1b]22;pointer\x07",
        // The kitty keyboard protocol, which the config turns off, and XTerm's
        // modifyOtherKeys, which alacritty_terminal ignores.
        "\x1b[?u\x1b[>1u\x1b[=1;2u\x1b[<1u\x1b[>4;1m\x1b[?4m\x1b[0;0 k",
        // A synchronized update, opened and closed.
        "\x1b[?2026hsynced\x1b[?2026l",
        // decaln, then clears, then enough lines to fill some scrollback.
        "\x1b#8\x1b[?3h\x1b#8\x1b[2J\x1b[3J",
    ] {
        output.push_str(part);
    }
    for line in 0..40 {
        output.push_str(&format!("line {line} \x1b[3{}m漢字\x1b[0m\r\n", line % 8));
    }
    output.push_str("\x1b[?1049hlast\x1b[2;3H");
    output
}

#[test]
fn every_handler_call_matches_a_bare_term() {
    let output = every_handler_call();
    for (columns, rows) in [(20, 6), (2, 1), (2, 4), (7, 3), (80, 24)] {
        assert_matches_bare(
            &format!("{columns}x{rows}"),
            columns,
            rows,
            output.as_bytes(),
        );
    }
}

#[test]
fn every_handler_call_split_into_pieces_matches_it_whole() {
    let mut output = every_handler_call();
    // Strings past the OSC cap, split anywhere, are capped the same way.
    output.push_str(&format!("\x1b]0;{}\x07after", "x".repeat(10_000)));
    output.push_str(&format!("\x1b]4;1;{}\x1b\\after", "0".repeat(10_000)));
    assert_split_matches_whole("handler calls", 20, 6, &output);
}

const RECORDINGS: [(&str, u16, u16, &str); 8] = [
    (
        "colored_underline",
        157,
        40,
        include_str!("../../tests/fixtures/alacritty/colored_underline.recording"),
    ),
    (
        "csi_rep",
        105,
        29,
        include_str!("../../tests/fixtures/alacritty/csi_rep.recording"),
    ),
    (
        "history",
        105,
        29,
        include_str!("../../tests/fixtures/alacritty/history.recording"),
    ),
    (
        "sgr",
        139,
        35,
        include_str!("../../tests/fixtures/alacritty/sgr.recording"),
    ),
    (
        "tab_rendering",
        116,
        63,
        include_str!("../../tests/fixtures/alacritty/tab_rendering.recording"),
    ),
    (
        "vim_simple_edit",
        80,
        24,
        include_str!("../../tests/fixtures/alacritty/vim_simple_edit.recording"),
    ),
    (
        "wrapline_alt_toggle",
        139,
        35,
        include_str!("../../tests/fixtures/alacritty/wrapline_alt_toggle.recording"),
    ),
    (
        "zerowidth",
        139,
        35,
        include_str!("../../tests/fixtures/alacritty/zerowidth.recording"),
    ),
];

#[test]
fn upstream_recordings_match_a_bare_term() {
    for (name, columns, rows, output) in RECORDINGS {
        assert_matches_bare(name, columns, rows, output.as_bytes());
        assert_split_matches_whole(name, columns, rows, output);
    }
}

#[test]
fn upstream_recordings_draw_without_control_characters() {
    for (name, columns, rows, output) in RECORDINGS {
        let mut emulator = live(columns, rows);
        emulator.apply(output);
        assert_clean_symbols(name, &draw(&emulator));
    }
}

fn guarded(pieces: &[&[u8]]) -> Vec<u8> {
    let mut guard = OscGuard::default();
    let mut forwarded = Vec::new();
    for piece in pieces {
        guard.filter(piece, |run| forwarded.extend_from_slice(run));
    }
    forwarded
}

#[test]
fn the_guard_caps_an_osc_payload_and_forwards_what_ends_it() {
    let ends: [&[u8]; 6] = [
        b"\x07",
        b"\x18",
        b"\x1a",
        b"\x1b\\",
        b"\x1b[31m",
        b"\x1b]2;next\x07",
    ];
    for (case, end) in ends.iter().enumerate() {
        let mut output = b"\x1b]0;".to_vec();
        output.extend(std::iter::repeat_n(b'x', 10_000));
        output.extend_from_slice(end);
        output.extend_from_slice(b"after");
        // The command number counts toward the cap, so 4095 of the x's fit.
        let mut expected = b"\x1b]0;".to_vec();
        expected.extend(std::iter::repeat_n(b'x', MAX_OSC_PAYLOAD - 1));
        expected.extend_from_slice(end);
        expected.extend_from_slice(b"after");
        assert!(guarded(&[&output[..]]) == expected, "end {case}, whole");
        for size in [1, 2, 7, 4095, 4096, 4097] {
            let split: Vec<&[u8]> = output.chunks(size).collect();
            assert!(guarded(&split) == expected, "end {case}, pieces of {size}");
        }
    }
}

#[test]
fn the_guard_counts_only_bytes_vte_keeps() {
    // Separators and ignored C0 controls add nothing to vte's buffer, so they pass.
    let mut output = b"\x1b]0".to_vec();
    output.extend(std::iter::repeat_n(b';', 10_000));
    output.extend(std::iter::repeat_n(0x01, 10_000));
    output.extend_from_slice(b"\x07");
    assert!(guarded(&[&output[..]]) == output);
    // Each string starts its own count.
    let mut output = Vec::new();
    for _ in 0..3 {
        output.extend_from_slice(b"\x1b]2;");
        output.extend(std::iter::repeat_n(b'y', MAX_OSC_PAYLOAD - 1));
        output.extend_from_slice(b"\x07");
    }
    assert!(guarded(&[&output[..]]) == output);
}

#[test]
fn the_guard_passes_everything_outside_osc_strings() {
    let mut output = Vec::new();
    for start in [&b"\x1bP1$q"[..], b"\x1b_", b"\x1b^", b"\x1bX", b"\x1b[1;"] {
        output.extend_from_slice(start);
        output.extend(std::iter::repeat_n(b'z', 10_000));
        output.extend_from_slice(b"\x1b\\");
    }
    output.extend(std::iter::repeat_n(b'w', 10_000));
    output.extend_from_slice("漢字 \u{9d}4;not an OSC\u{9c}".as_bytes());
    assert!(guarded(&[&output[..]]) == output);
}

/// An OSC 4 that sets color 1 only if vte keeps its whole payload, so a bare `Term` shows
/// whether vte parsed an OSC string here, and the emulator whether the guard capped it.
fn color_after_long_payload() -> Vec<u8> {
    let mut output = b"\x1b]4;".to_vec();
    output.extend(std::iter::repeat_n(b'0', 5000));
    output.extend_from_slice(b"1;rgb:ff/00/00\x07Z");
    output
}

#[test]
fn the_guard_tracks_every_byte_after_esc_as_vte_does() {
    let mut entered = 0;
    for byte in 0..=u8::MAX {
        let mut output = vec![0x1B, byte];
        output.extend(color_after_long_payload().into_iter().skip(1));
        let (bare, emulator) = feed_both(20, 4, &output);
        let case = format!("byte {byte:#04x} after ESC");
        // A guard that saw an OSC string where vte didn't would drop text vte shows.
        assert_same_screen(&case, &bare.term, &emulator.engine.term);
        if bare.term.colors()[1].is_some() {
            entered += 1;
            // A guard that missed an OSC string vte parsed would let the payload through.
            assert!(
                emulator.engine.term.colors()[1].is_none(),
                "{case}: payload not capped"
            );
        }
    }
    assert!(entered > 0, "no byte led vte into an OSC string");
}

#[test]
fn the_guard_finds_osc_strings_after_any_parser_state() {
    for prefix in [
        "",
        "text",
        "é漢",
        "\x1b[1;",
        "\x1b[?",
        "\x1b[1;2$",
        "\x1b[1:2",
        "\x1bP1$",
        "\x1bPq#0;2;0;0;0",
        "\x1b_apc",
        "\x1bXsos",
        "\x1b^pm",
        "\x1b(",
        "\x1b#",
        "\x1b]0;unterminated",
        "\x1b\x1b",
        "\x1b\x07",
        "\x1b\x7f",
        "\u{9d}",
        "\u{90}",
        "\x1b[?2026h",
    ] {
        let mut output = prefix.as_bytes().to_vec();
        output.extend(color_after_long_payload());
        let (mut bare, mut emulator) = feed_both(20, 4, &output);
        // The parser holds output after a synchronized update opens. The guard saw it first.
        bare.flush_sync();
        emulator.engine.flush_sync();
        let case = format!("prefix of {} bytes", prefix.len());
        assert!(
            bare.term.colors()[1].is_some(),
            "{case}: vte parsed no OSC string"
        );
        assert!(
            emulator.engine.term.colors()[1].is_none(),
            "{case}: payload not capped"
        );
        assert_same_screen(&case, &bare.term, &emulator.engine.term);
    }
}

#[test]
fn every_byte_after_a_capped_payload_acts_as_in_vte() {
    for byte in 0..=u8::MAX {
        let mut output = b"\x1b]0;".to_vec();
        output.extend(std::iter::repeat_n(b'x', 5000));
        output.push(byte);
        output.extend_from_slice(b"AB\x1b]4;1;rgb:ff/00/00\x07Z");
        let (bare, emulator) = feed_both(20, 4, &output);
        assert_same_term(
            &format!("byte {byte:#04x} after a capped payload"),
            &bare.term,
            &emulator.engine.term,
        );
    }
}

#[test]
fn text_after_an_interrupted_long_osc_keeps_its_color() {
    for start in ["\x1b]0;", "\x1b]52;c;", "\x1b]8;;https://example.com/"] {
        let mut emulator = live(10, 2);
        emulator.apply(start);
        emulator.apply(&"x".repeat(5000));
        emulator.apply("\x1b[31mred");
        let buf = draw(&emulator);
        assert!(rows(&buf)[0] == "red       ", "text after a long string");
        assert_eq!(buf[(0, 0)].fg, Color::Red);
        assert_eq!(buf[(2, 0)].fg, Color::Red);
    }
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<&'static str>>>);

impl EventListener for Recorder {
    fn send_event(&self, event: Event) {
        let kind = match event {
            Event::Title(_) => "title",
            Event::ResetTitle => "reset title",
            Event::ClipboardStore(..) => "clipboard store",
            Event::ClipboardLoad(..) => "clipboard load",
            Event::TextAreaSizeRequest(_) => "pixel size",
            Event::ColorRequest(..) => "color",
            Event::PtyWrite(_) => "reply",
            _ => "other",
        };
        self.0.lock().unwrap().push(kind);
    }
}

fn window_effects() -> String {
    [
        "\x1b]0;title\x07".to_string(),
        format!("\x1b]2;{}\x1b\\", "t".repeat(100_000)),
        "\x1b[22t".repeat(5000),
        "\x1b[23t\x1b[23t".to_string(),
        format!("\x1b]52;c;{}\x07", "QUJD".repeat(50_000)),
        "\x1b]52;c;?\x07\x1b[14t".to_string(),
        format!(
            "\x1b]8;id=1;https://example.com/{}\x07linked\x1b]8;;\x07",
            "p".repeat(100_000)
        ),
        "\x1b]8;;https://example.com/\x1b\\short\x1b]8;;\x1b\\".to_string(),
    ]
    .concat()
}

#[test]
fn titles_hyperlinks_and_the_clipboard_leave_nothing_behind() {
    // This config lets alacritty_terminal copy and paste, so only the wrapper keeps them out.
    let copy_paste = alacritty_terminal::term::Config {
        osc52: Osc52::CopyPaste,
        ..config(40)
    };
    let size = (4_usize, 40_usize);
    let output = window_effects();

    let bare = Recorder::default();
    let mut term = Term::new(copy_paste.clone(), &size, bare.clone());
    Processor::<StdSyncHandler>::new().advance(&mut term, output.as_bytes());
    let events = bare.0.lock().unwrap().clone();
    for kind in ["title", "clipboard store", "clipboard load", "pixel size"] {
        assert!(events.contains(&kind), "a bare Term sends no {kind} event");
    }

    let recorder = Recorder::default();
    let mut term = Term::new(copy_paste.clone(), &size, recorder.clone());
    let mut processor = Processor::<StdSyncHandler>::new();
    OscGuard::default().filter(output.as_bytes(), |run| {
        processor.advance(&mut GuardedTerm { term: &mut term }, run);
    });
    // `set_options` reports the stored title, or that there is none.
    term.set_options(copy_paste);
    let events = recorder.0.lock().unwrap().clone();
    assert_eq!(events, ["reset title"]);
    assert_no_hyperlinks("window effects", &term);
}

#[test]
fn long_dcs_apc_pm_and_sos_strings_show_nothing_and_keep_what_follows() {
    // Every 7-bit byte that keeps the string open, 4000 bytes at a time, about 1 MB in all.
    let filler: String = (0u8..0x80)
        .filter(|byte| ![0x18, 0x1A, 0x1B].contains(byte))
        .map(char::from)
        .collect::<String>()
        .repeat(32);
    for (name, start) in [
        ("DCS", "\x1bP1$q"),
        ("DCS passthrough", "\x1bPq"),
        ("APC", "\x1b_"),
        ("PM", "\x1b^"),
        ("SOS", "\x1bX"),
    ] {
        let mut emulator = live(20, 3);
        emulator.apply(start);
        for _ in 0..256 {
            emulator.apply(&filler);
        }
        emulator.apply("\x1b\\after");
        let screen = screen(&emulator);
        assert!(
            screen[0] == format!("{:20}", "after"),
            "{name}: text after the string"
        );
        assert!(
            screen[1..].iter().all(|row| row.trim().is_empty()),
            "{name}: string bytes reached the screen"
        );
        assert!(emulator.take_replies().is_none(), "{name}: replies");
    }
}

#[test]
fn zero_width_characters_match_a_bare_term_up_to_the_cap() {
    let marks = "\u{301}".repeat(MAX_ZERO_WIDTH_PER_CELL);
    for (case, output) in [
        ("combining", format!("e{marks}x")),
        ("wide", format!("漢{marks}x")),
        ("wrapping", format!("abcd{marks}e")),
        ("wide at the edge", format!("ab漢{marks}")),
        ("after a move", format!("ab\x1b[2G{marks}\x1b[4G{marks}")),
        ("REP", "e\u{301}\x1b[15b".to_string()),
        ("on a blank", format!("\x1b[3G{marks}")),
    ] {
        assert_matches_bare(case, 4, 3, output.as_bytes());
    }
}

#[test]
fn zero_width_characters_stop_at_the_cap() {
    let flood = "\u{301}".repeat(100_000);
    let cap = MAX_ZERO_WIDTH_PER_CELL;

    let mut emulator = live(10, 3);
    emulator.apply("a");
    emulator.apply(&flood);
    assert_eq!(marks(&emulator, 0, 0), cap);

    // REP repeats the last character printed, here a combining one.
    emulator.apply("b\u{302}\x1b[65535b");
    assert_eq!(marks(&emulator, 0, 1), cap);
    assert_eq!(emulator.engine.term.grid().cursor.point.column.0, 2);

    // Moving back targets an earlier cell, which is capped as well.
    emulator.apply("\x1b[2G");
    emulator.apply(&flood);
    emulator.apply("\x1b[3G");
    emulator.apply(&flood);
    assert_eq!((marks(&emulator, 0, 0), marks(&emulator, 0, 1)), (cap, cap));

    // After a wide character the target is its first column.
    let mut emulator = live(10, 3);
    emulator.apply("漢");
    emulator.apply(&flood);
    emulator.apply("\x1b[2G");
    emulator.apply(&flood);
    assert_eq!((marks(&emulator, 0, 0), marks(&emulator, 0, 1)), (cap, 0));

    // A wide character in the last columns leaves the cursor on its second half.
    let mut emulator = live(4, 3);
    emulator.apply("ab漢");
    assert!(emulator.engine.term.grid().cursor.input_needs_wrap);
    emulator.apply(&flood);
    assert_eq!((marks(&emulator, 0, 2), marks(&emulator, 0, 3)), (cap, 0));
}

#[test]
fn a_kitty_keyboard_stack_flood_is_harmless() {
    let mut emulator = live(10, 3);
    emulator.apply(&"\x1b[>1u".repeat(5000));
    emulator.apply("\x1b[?u\x1b[=31;1u\x1b[?1049h\x1b[>3u\x1b[?1049l\x1b[<9999u");
    let mode = *emulator.engine.term.mode();
    assert!(!mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
    assert_eq!(emulator.take_replies(), None);
}

#[test]
fn live_queries_get_exact_replies() {
    let mut emulator = live(80, 24);
    for (query, reply) in [
        ("\x1b[c", "\x1b[?6c"),
        ("\x1b[0c", "\x1b[?6c"),
        ("\x1bZ", "\x1b[?6c"),
        ("\x1b[>c", "\x1b[>0;2600;1c"),
        ("\x1b[5n", "\x1b[0n"),
        ("\x1b[3;5H\x1b[6n", "\x1b[3;5R"),
        ("\x1b[?2004$p", "\x1b[?2004;2$y"),
        ("\x1b[?2004h\x1b[?2004$p", "\x1b[?2004;1$y"),
        ("\x1b[?1h\x1b[?1$p", "\x1b[?1;1$y"),
        ("\x1b[?2026$p", "\x1b[?2026;2$y"),
        ("\x1b[?9999$p", "\x1b[?9999;0$y"),
        ("\x1b[4$p", "\x1b[4;2$y"),
        ("\x1b[4h\x1b[4$p", "\x1b[4;1$y"),
        ("\x1b[18t", "\x1b[8;24;80t"),
        ("\x1b[c\x1b[5n", "\x1b[?6c\x1b[0n"),
    ] {
        emulator.apply(query);
        assert_eq!(emulator.take_replies().as_deref(), Some(reply), "{query:?}");
    }
    emulator.resize(100, 30);
    emulator.apply("\x1b[18t");
    assert_eq!(emulator.take_replies().as_deref(), Some("\x1b[8;30;100t"));
}

#[test]
fn queries_without_a_native_reply_get_none() {
    let mut emulator = live(80, 24);
    for query in [
        "\x1b[14t",
        "\x1b]10;?\x07",
        "\x1b]11;?\x1b\\",
        "\x1b]12;?\x07",
        "\x1b]4;1;?\x07",
        "\x1b]52;c;?\x07",
        "\x1bP$qm\x1b\\",
        "\x1bP+q544e\x1b\\",
        "\x1b[?u",
        "\x1b[?4m",
        "\x1b[>q",
        "\x1b[1c",
    ] {
        emulator.apply(query);
        assert_eq!(emulator.take_replies(), None, "{query:?}");
    }
}

#[test]
fn replay_and_read_only_output_queue_no_replies() {
    let queries = "\x1b[c\x1b[>c\x1b[5n\x1b[6n\x1b[?2004$p\x1b[4$p\x1b[18t";

    let mut emulator = live(20, 4);
    emulator.reset(queries);
    assert_eq!(emulator.take_replies(), None);
    emulator.apply("\x1b[c");
    assert_eq!(emulator.take_replies().as_deref(), Some("\x1b[?6c"));

    let mut observer = TerminalEmulator::new(20, 4);
    observer.apply(queries);
    assert_eq!(observer.take_replies(), None);
    observer.reset(queries);
    assert_eq!(observer.take_replies(), None);
}

#[test]
fn a_reply_flood_stops_at_the_caps() {
    let mut emulator = live(20, 4);
    emulator.apply(&"\x1b[c".repeat(1000));
    assert_eq!(
        emulator.take_replies(),
        Some("\x1b[?6c".repeat(MAX_PENDING_REPLIES))
    );
    assert_eq!(emulator.dropped_replies(), 1000 - MAX_PENDING_REPLIES);
    // Taking the replies makes room again.
    emulator.apply("\x1b[5n");
    assert_eq!(emulator.take_replies().as_deref(), Some("\x1b[0n"));
    // A reset starts the count over.
    emulator.reset("");
    assert_eq!(emulator.dropped_replies(), 0);

    // Native replies are short, so the byte cap shows only with longer ones.
    let listener = ReplyListener::default();
    listener.queue().accepting = true;
    let reply = "r".repeat(1000);
    for _ in 0..5 {
        listener.send_event(Event::PtyWrite(reply.clone()));
    }
    listener.send_event(Event::PtyWrite("r".repeat(MAX_PENDING_REPLY_BYTES + 1)));
    listener.send_event(Event::Title("not a reply".to_string()));
    listener.send_event(Event::ClipboardStore(
        alacritty_terminal::term::ClipboardType::Clipboard,
        "not a reply".to_string(),
    ));
    let queue = listener.queue();
    assert_eq!((queue.text.len(), queue.count, queue.dropped), (4000, 4, 2));
}

#[test]
fn history_ending_in_a_synchronized_update_is_applied_without_replies() {
    let mut emulator = live(20, 4);
    emulator.reset("before\x1b[?2026h\r\nafter\x1b[c\x1b[5n");
    assert_eq!(emulator.sync_deadline(), None);
    assert_eq!(emulator.take_replies(), None);
    let screen = screen(&emulator);
    assert!(screen[0].starts_with("before") && screen[1].starts_with("after"));
    emulator.apply("\x1b[c");
    assert_eq!(emulator.take_replies().as_deref(), Some("\x1b[?6c"));
}

#[test]
fn the_caller_flushes_an_expired_synchronized_update() {
    let mut emulator = live(20, 4);
    emulator.apply("\x1b[?2026hheld\x1b[5n");
    let deadline = emulator
        .sync_deadline()
        .expect("an open synchronized update");
    assert!(screen(&emulator)[0].trim().is_empty());
    assert_eq!(emulator.take_replies(), None);

    if let Some(before) = deadline.checked_sub(Duration::from_millis(1)) {
        assert!(!emulator.flush_sync_if_due(before));
        assert_eq!(emulator.sync_deadline(), Some(deadline));
    }
    assert!(emulator.flush_sync_if_due(deadline));
    assert!(screen(&emulator)[0].starts_with("held"));
    assert_eq!(emulator.take_replies().as_deref(), Some("\x1b[0n"));
    assert_eq!(emulator.sync_deadline(), None);
    assert!(!emulator.flush_sync_if_due(deadline + Duration::from_secs(1)));

    // A program that closes its update needs no flush.
    emulator.apply("\x1b[?2026h\r\nshown\x1b[?2026l");
    assert_eq!(emulator.sync_deadline(), None);
    assert!(screen(&emulator)[1].starts_with("shown"));
}

#[test]
fn changing_write_access_settles_held_output_under_the_old_setting() {
    // Output held while read-only gets no reply once write access arrives.
    let mut emulator = TerminalEmulator::new(20, 4);
    emulator.apply("\x1b[?2026h\x1b[c");
    emulator.set_write_access(true);
    assert_eq!(emulator.sync_deadline(), None);
    assert_eq!(emulator.take_replies(), None);
    emulator.apply("\x1b[c");
    assert_eq!(emulator.take_replies().as_deref(), Some("\x1b[?6c"));

    // Taking write access away drops replies not taken, held ones included.
    emulator.apply("\x1b[5n\x1b[?2026h\x1b[c");
    emulator.set_write_access(false);
    assert_eq!(emulator.sync_deadline(), None);
    assert_eq!(emulator.take_replies(), None);
    emulator.apply("\x1b[c");
    assert_eq!(emulator.take_replies(), None);

    // A reset drops replies waiting from before it.
    emulator.set_write_access(true);
    emulator.apply("\x1b[c");
    emulator.reset("");
    assert_eq!(emulator.take_replies(), None);
    assert!(emulator.write_access());
}

#[test]
fn scrollback_follows_the_width() {
    for (columns, cap) in [(2, 5000), (40, 5000), (80, 2500), (333, 600), (1000, 200)] {
        let mut emulator = TerminalEmulator::new(columns, 5);
        emulator.apply(&"x\r\n".repeat(6000));
        assert_eq!(history(&emulator), cap, "{columns} columns");
    }

    let mut emulator = TerminalEmulator::new(80, 5);
    emulator.apply(&"x\r\n".repeat(6000));
    emulator.resize(1000, 5);
    assert_eq!(history(&emulator), 200);
    emulator.resize(40, 5);
    emulator.apply(&"x\r\n".repeat(6000));
    assert_eq!(history(&emulator), 5000);

    emulator.scroll_viewport(i32::MAX);
    emulator.scroll_viewport(i32::MAX);
    assert_eq!(emulator.viewport_offset(), 5000);
    emulator.scroll_viewport(-3);
    assert_eq!(emulator.viewport_offset(), 4997);
    emulator.scroll_viewport(i32::MIN);
    assert_eq!(emulator.viewport_offset(), 0);
}

#[test]
fn leaving_the_alternate_screen_restores_the_main_one() {
    let mut emulator = live(10, 3);
    emulator.apply("main\x1b[?1049h\x1b[2;2Halt");
    assert!(emulator.input_modes().alternate_screen);
    assert_eq!(screen(&emulator)[1], " alt      ");
    emulator.apply(&"x\r\n".repeat(20));
    emulator.apply("\x1b[?1049l");
    assert!(!emulator.input_modes().alternate_screen);
    assert_eq!(screen(&emulator)[0], "main      ");
    // Lines scrolled off the alternate screen add no scrollback.
    assert_eq!(history(&emulator), 0);
    let cursor = draw_cursor(&emulator, Rect::new(0, 0, 10, 3));
    assert_eq!(cursor.map(|cursor| (cursor.x, cursor.y)), Some((4, 0)));
}

#[test]
fn the_size_stays_within_bounds() {
    assert_eq!(TerminalEmulator::new(0, 0).size(), (MIN_COLUMNS, MIN_ROWS));
    assert_eq!(TerminalEmulator::new(1, 1).size(), (2, 1));
    assert_eq!(
        TerminalEmulator::new(u16::MAX, u16::MAX).size(),
        (MAX_COLUMNS, MAX_ROWS)
    );
    let mut emulator = TerminalEmulator::new(80, 24);
    emulator.resize(0, 0);
    assert_eq!(emulator.size(), (2, 1));
    let grid = emulator.engine.term.grid();
    assert_eq!((grid.columns(), grid.screen_lines()), (2, 1));
    emulator.resize(5000, 5000);
    assert_eq!(emulator.size(), (1000, 500));
}

#[test]
fn one_row_and_two_columns_work() {
    let output = "漢字ab\u{301}\r\n漢\x1b[6n\x1b[?1049hcd\x1b[?1049l";
    assert_matches_bare("2x1", 2, 1, output.as_bytes());
    let mut emulator = live(2, 1);
    emulator.apply(output);
    // The cursor waits on the second column of the wide character until the next one wraps.
    assert_eq!(emulator.take_replies().as_deref(), Some("\x1b[1;2R"));
    assert_clean_symbols("2x1", &draw(&emulator));
}

#[test]
fn resizing_wide_characters_matches_a_bare_term() {
    let output = "漢字かなabc漢\u{301}字\r\nxy漢\x1b[?1049hz漢\x1b[?1049l";
    let mut bare = Bare::new(5, 3);
    bare.feed(output.as_bytes());
    let mut emulator = live(5, 3);
    emulator.apply(output);
    for (columns, rows) in [(3, 3), (2, 2), (7, 4), (4, 1), (9, 5), (2, 1), (6, 3)] {
        bare.resize(columns, rows);
        emulator.resize(columns, rows);
        let case = format!("{columns}x{rows}");
        assert_same_term(&case, &bare.term, &emulator.engine.term);
        assert_clean_symbols(&case, &draw(&emulator));
    }
}

fn draw_cursor(emulator: &TerminalEmulator, area: Rect) -> Option<TerminalCursor> {
    let mut buf = Buffer::empty(area);
    emulator.render(area, &mut buf)
}

#[test]
fn rendering_stays_inside_the_area_and_the_buffer() {
    let mut emulator = live(10, 4);
    emulator.apply("abcdefghij\r\nklm");
    let filler = Cell::new("Q");
    let untouched = |buf: &Buffer| buf.content.iter().all(|cell| *cell == filler);

    let mut buf = Buffer::filled(Rect::new(0, 0, 8, 6), filler.clone());
    assert_eq!(emulator.render(Rect::new(0, 0, 0, 0), &mut buf), None);
    assert_eq!(emulator.render(Rect::new(3, 3, 0, 5), &mut buf), None);
    assert_eq!(emulator.render(Rect::new(3, 3, 5, 0), &mut buf), None);
    assert_eq!(emulator.render(Rect::new(100, 100, 10, 4), &mut buf), None);
    assert!(untouched(&buf));

    // An area running past the buffer's right and bottom edges.
    emulator.render(Rect::new(5, 2, 20, 10), &mut buf);
    for y in 0..6 {
        for x in 0..8 {
            let inside = x >= 5 && y >= 2;
            assert_eq!(buf[(x, y)] != filler, inside, "cell at {x}, {y}");
        }
    }
    assert_eq!(rows(&buf)[2], "QQQQQabc");
    assert_eq!(rows(&buf)[3], "QQQQQklm");
    assert_eq!(rows(&buf)[4], "QQQQQ   ");

    // A buffer that starts inside the area, which clips the area's top-left.
    let mut buf = Buffer::empty(Rect::new(10, 10, 4, 2));
    emulator.render(Rect::new(8, 9, 10, 4), &mut buf);
    assert_eq!(rows(&buf), ["m   ", "    "]);

    // An area larger than the grid blanks what the grid doesn't cover.
    let mut buf = Buffer::filled(Rect::new(0, 0, 14, 6), filler.clone());
    emulator.render(buf.area, &mut buf);
    assert!(buf.content.iter().all(|cell| *cell != filler));
    assert_eq!(rows(&buf)[5], " ".repeat(14));
}

#[test]
fn rendering_draws_wide_and_combining_characters() {
    let mut emulator = live(6, 3);
    emulator.apply("e\u{301}漢\tx\r\nab漢c\r\n\u{301}");
    let buf = draw(&emulator);
    assert_eq!(buf[(0, 0)].symbol(), "e\u{301}");
    assert_eq!(buf[(1, 0)].symbol(), "漢");
    // The hidden second column of a wide character is left empty, as ratatui expects.
    assert_eq!(buf[(2, 0)], Cell::EMPTY);
    // The tab's cells hold spaces, not tabs.
    assert_eq!(rows(&buf)[0], "e\u{301}漢   x");

    // A wide character cut off by the area's right edge becomes a space.
    let mut emulator = live(4, 1);
    emulator.apply("\x1b[42ma漢");
    let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
    emulator.render(buf.area, &mut buf);
    assert_eq!(rows(&buf), ["a "]);
    assert_eq!(buf[(1, 0)].bg, Color::Green);

    // Its second half, cut off from its first by the buffer's left edge, does too.
    let mut buf = Buffer::empty(Rect::new(2, 0, 2, 1));
    emulator.render(Rect::new(0, 0, 4, 1), &mut buf);
    assert_eq!(rows(&buf), ["  "]);
    assert_eq!(buf[(2, 0)].bg, Color::Green);

    // A wide character that didn't fit at the end of a line leaves a blank placeholder.
    let mut emulator = live(3, 2);
    emulator.apply("ab漢");
    assert_eq!(screen(&emulator), ["ab ", "漢  "]);
}

#[test]
fn rendering_maps_colors_and_attributes() {
    let mut emulator = live(10, 2);
    emulator.apply("\x1b[1;3;4;7;9;31;42ma\x1b[0;2;8;38;5;200;48;2;1;2;3mb");
    emulator.apply("\x1b[0;4:3;58;2;9;8;7mc\x1b[0;91;104md\x1b[0;38;5;3me\x1b[0m");
    let buf = draw(&emulator);
    let a = &buf[(0, 0)];
    assert_eq!((a.fg, a.bg), (Color::Red, Color::Green));
    assert_eq!(
        a.modifier,
        Modifier::BOLD
            | Modifier::ITALIC
            | Modifier::UNDERLINED
            | Modifier::REVERSED
            | Modifier::CROSSED_OUT
    );
    let b = &buf[(1, 0)];
    assert_eq!((b.fg, b.bg), (Color::Indexed(200), Color::Rgb(1, 2, 3)));
    assert_eq!(b.modifier, Modifier::DIM | Modifier::HIDDEN);
    let c = &buf[(2, 0)];
    assert_eq!(
        (c.modifier, c.underline_color),
        (Modifier::UNDERLINED, Color::Rgb(9, 8, 7))
    );
    let d = &buf[(3, 0)];
    assert_eq!((d.fg, d.bg), (Color::LightRed, Color::LightBlue));
    assert_eq!(buf[(4, 0)].fg, Color::Yellow);
    let blank = &buf[(5, 0)];
    assert_eq!(
        (blank.fg, blank.bg, blank.underline_color, blank.modifier),
        (Color::Reset, Color::Reset, Color::Reset, Modifier::empty())
    );

    // Colors the program sets apply to cells already drawn with them.
    emulator.apply("\x1b]4;1;rgb:12/34/56\x07\x1b]4;200;#010203\x07\x1b]10;rgb:aa/bb/cc\x07");
    let buf = draw(&emulator);
    assert_eq!(buf[(0, 0)].fg, Color::Rgb(0x12, 0x34, 0x56));
    assert_eq!(buf[(1, 0)].fg, Color::Rgb(1, 2, 3));
    assert_eq!(buf[(5, 0)].fg, Color::Rgb(0xaa, 0xbb, 0xcc));
    emulator.apply("\x1b]104\x07\x1b]110\x07");
    let buf = draw(&emulator);
    assert_eq!((buf[(0, 0)].fg, buf[(5, 0)].fg), (Color::Red, Color::Reset));
}

#[test]
fn rendering_leaves_no_stale_cells() {
    let mut emulator = live(6, 2);
    emulator.apply("hi");
    let mut stale = Cell::new("Z");
    stale.fg = Color::Blue;
    stale.bg = Color::Magenta;
    stale.underline_color = Color::Cyan;
    stale.modifier = Modifier::BOLD | Modifier::SLOW_BLINK;
    let mut buf = Buffer::filled(Rect::new(0, 0, 6, 2), stale);
    emulator.render(buf.area, &mut buf);
    let mut expected = Buffer::empty(buf.area);
    expected.set_string(0, 0, "hi", ratatui::style::Style::default());
    assert_eq!(buf, expected);
}

#[test]
fn rendering_reports_the_cursor() {
    let area = Rect::new(3, 2, 10, 3);
    let mut emulator = live(10, 3);
    emulator.apply("ab");
    let cursor = |emulator: &TerminalEmulator| draw_cursor(emulator, area);
    assert_eq!(
        cursor(&emulator),
        Some(TerminalCursor {
            x: 5,
            y: 2,
            shape: TerminalCursorShape::Block,
            blinking: false,
        })
    );
    for (style, shape, blinking) in [
        ("\x1b[1 q", TerminalCursorShape::Block, true),
        ("\x1b[3 q", TerminalCursorShape::Underline, true),
        ("\x1b[4 q", TerminalCursorShape::Underline, false),
        ("\x1b[5 q", TerminalCursorShape::Bar, true),
        ("\x1b[6 q", TerminalCursorShape::Bar, false),
        ("\x1b[ q", TerminalCursorShape::Block, false),
    ] {
        emulator.apply(style);
        let got = cursor(&emulator).map(|cursor| (cursor.shape, cursor.blinking));
        assert_eq!(got, Some((shape, blinking)), "{style:?}");
    }
    emulator.apply("\x1b[?25l");
    assert_eq!(cursor(&emulator), None);
    emulator.apply("\x1b[?25h");

    // On the second half of a wide character, the cursor sits on its first.
    emulator.apply("\r\n漢\x1b[2G");
    assert_eq!(
        cursor(&emulator).map(|cursor| (cursor.x, cursor.y)),
        Some((3, 3))
    );

    // A cursor outside the buffer, or scrolled out of view, isn't reported.
    let mut small = Buffer::empty(Rect::new(0, 0, 4, 3));
    assert_eq!(emulator.render(area, &mut small), None);
    emulator.apply(&"\r\n".repeat(10));
    emulator.scroll_viewport(5);
    assert_eq!(cursor(&emulator), None);
    emulator.scroll_viewport(-5);
    assert!(cursor(&emulator).is_some());
}

#[test]
fn rendered_symbols_hold_no_control_characters() {
    let controls: String = (0u32..0x20)
        .chain(0x7F..0xA0)
        .filter_map(char::from_u32)
        .filter(|&c| c != '\x1b')
        .collect();
    let mut emulator = live(30, 6);
    emulator.apply(&controls);
    emulator.apply("a\tb\u{202e}c\u{2066}\u{200f}\u{61c}d\u{301}\u{202a}");
    emulator.apply("e\u{200b}\u{200d}\u{fe0f}\u{feff}f\u{85}g\x1b(0lqkx\x1b(B");
    emulator.apply("\u{202e}\u{2067}\u{200e}");
    let buf = draw(&emulator);
    assert_clean_symbols("controls", &buf);
    // Direction marks are left out, and the character under them and its other marks stay.
    let text = rows(&buf).concat();
    for kept in ["b", "c", "d\u{301}"] {
        assert!(text.contains(kept), "{kept:?} missing");
    }
}

#[test]
fn input_modes_follow_the_program() {
    let mut emulator = live(10, 3);
    assert_eq!(
        emulator.input_modes(),
        InputModes {
            alternate_scroll: true,
            ..InputModes::default()
        }
    );
    emulator.apply("\x1b[?1h\x1b=\x1b[?2004h\x1b[?1004h\x1b[20h\x1b[?1049h\x1b[?1007l");
    for (sequence, mouse, encoding) in [
        ("\x1b[?1000h", MouseTracking::Click, MouseEncoding::Default),
        ("\x1b[?1002h", MouseTracking::Drag, MouseEncoding::Default),
        (
            "\x1b[?1003h\x1b[?1005h",
            MouseTracking::Motion,
            MouseEncoding::Utf8,
        ),
        ("\x1b[?1006h", MouseTracking::Motion, MouseEncoding::Sgr),
        ("\x1b[?1003l", MouseTracking::Off, MouseEncoding::Sgr),
    ] {
        emulator.apply(sequence);
        let modes = emulator.input_modes();
        assert_eq!((modes.mouse, modes.mouse_encoding), (mouse, encoding));
    }
    assert_eq!(
        emulator.input_modes(),
        InputModes {
            application_cursor: true,
            application_keypad: true,
            bracketed_paste: true,
            focus_events: true,
            alternate_screen: true,
            alternate_scroll: false,
            line_feed_new_line: true,
            mouse: MouseTracking::Off,
            mouse_encoding: MouseEncoding::Sgr,
        }
    );
    emulator.clear();
    assert_eq!(
        emulator.input_modes(),
        InputModes {
            alternate_scroll: true,
            ..InputModes::default()
        }
    );
}

#[test]
fn reset_and_clear_start_from_scratch() {
    let mut emulator = live(10, 3);
    emulator.apply("\x1b]4;1;#123456\x07\x1b[?2004h\x1b[31mold\x1b[?2026h\x1b[c");
    emulator.reset("new");
    assert_eq!(
        screen(&emulator),
        ["new       ", "          ", "          "]
    );
    assert_eq!(emulator.engine.term.colors()[1], None);
    assert!(!emulator.input_modes().bracketed_paste);
    assert_eq!(emulator.sync_deadline(), None);
    assert_eq!(emulator.take_replies(), None);
    // The reset parser starts in ground, so output cut off mid-sequence before it is gone.
    emulator.apply("\x1b]0;");
    emulator.reset("");
    emulator.apply("shown");
    assert!(screen(&emulator)[0].starts_with("shown"));
    emulator.clear();
    assert!(screen(&emulator).iter().all(|row| row.trim().is_empty()));
    assert_eq!(emulator.size(), (10, 3));
}

#[test]
fn debug_output_holds_no_terminal_text() {
    let mut emulator = live(20, 3);
    emulator.apply("secret-token\x1b[c\x1b]0;secret-title\x07\x1b[?2026hsecret-held");
    let debug = format!("{emulator:?}");
    assert!(!debug.contains("secret"));
    assert!(!debug.contains('\x1b'));
    assert!(debug.contains("pending_replies: 1"));
    assert!(debug.contains("sync_pending: true"));
}

/// Large inputs Root can time. Nothing here checks the clock.
#[test]
fn large_inputs_stay_within_their_bounds() {
    let mut emulator = live(MAX_COLUMNS, MAX_ROWS);
    let chunk = "QUJD".repeat(16 * 1024);
    emulator.apply("\x1b]52;c;");
    for _ in 0..64 {
        emulator.apply(&chunk);
    }
    emulator.apply("\x07\x1bP1$q");
    for _ in 0..16 {
        emulator.apply(&chunk);
    }
    emulator.apply("\x1b\\x");
    emulator.apply(&"\u{301}".repeat(1_000_000));
    emulator.apply("漢\x1b[65535b");
    emulator.apply("\u{302}\x1b[65535b");
    emulator.apply(&"\x1b[c".repeat(100_000));
    emulator.apply(&"line of output\r\n".repeat(20_000));
    assert_eq!(history(&emulator), 200);
    assert!(emulator.take_replies().map_or(0, |replies| replies.len()) <= MAX_PENDING_REPLY_BYTES);
    let grid = emulator.engine.term.grid();
    for line in grid.topmost_line().0..=grid.bottommost_line().0 {
        for column in 0..grid.columns() {
            let marks = grid[Line(line)][Column(column)]
                .zerowidth()
                .map_or(0, <[char]>::len);
            assert!(
                marks <= MAX_ZERO_WIDTH_PER_CELL,
                "line {line}, column {column}"
            );
        }
    }
    let mut buf = Buffer::empty(Rect::new(0, 0, MAX_COLUMNS, MAX_ROWS));
    emulator.render(buf.area, &mut buf);
    assert_clean_symbols("large", &buf);
}
