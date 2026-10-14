//! What a key or a paste sends the shell, as xterm encodes them without the kitty keyboard
//! protocol. t3term never turns that protocol on, so crossterm reports keys as a terminal's
//! legacy encoding gives them, and they go back out the same way.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::terminal_emulator::InputModes;

/// What `key` types into the program under `modes`, or `None` for a key that types nothing,
/// such as Caps Lock or a media key.
///
/// A modifier on a cursor, editing or function key goes as xterm's parameter: 1, plus 1 for
/// Shift, 2 for Alt and 4 for Ctrl. Alt before any other key sends Esc first. The cursor keys,
/// Home and End follow the program's application cursor mode while no modifier is held.
/// Crossterm reports the keypad's keys as the keys they print, so application keypad mode
/// changes nothing here.
pub(super) fn encode(key: &KeyEvent, modes: InputModes) -> Option<String> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::META);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let param = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let application = modes.application_cursor;
    Some(match key.code {
        KeyCode::Char(c) => {
            let typed = match ctrl.then(|| control(c)).flatten() {
                Some(byte) => char::from(byte),
                None => c,
            };
            with_alt(typed.encode_utf8(&mut [0; 4]), alt)
        }
        KeyCode::Enter if modes.line_feed_new_line => with_alt("\r\n", alt),
        KeyCode::Enter => with_alt("\r", alt),
        KeyCode::Tab => with_alt("\t", alt),
        KeyCode::BackTab => "\x1b[Z".to_string(),
        KeyCode::Backspace if ctrl => with_alt("\x08", alt),
        KeyCode::Backspace => with_alt("\x7f", alt),
        KeyCode::Esc => with_alt("\x1b", alt),
        KeyCode::Null => with_alt("\0", alt),
        KeyCode::Up => cursor('A', param, application),
        KeyCode::Down => cursor('B', param, application),
        KeyCode::Right => cursor('C', param, application),
        KeyCode::Left => cursor('D', param, application),
        KeyCode::Home => cursor('H', param, application),
        KeyCode::End => cursor('F', param, application),
        KeyCode::Insert => tilde(2, param),
        KeyCode::Delete => tilde(3, param),
        KeyCode::PageUp => tilde(5, param),
        KeyCode::PageDown => tilde(6, param),
        KeyCode::F(n @ 1..=4) => {
            let last = char::from(b'P' + (n - 1));
            if param == 1 {
                format!("\x1bO{last}")
            } else {
                format!("\x1b[1;{param}{last}")
            }
        }
        KeyCode::F(n @ 5..=12) => {
            tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)], param)
        }
        _ => return None,
    })
}

/// What a paste of `text` sends. With bracketed paste on, the text goes between `ESC [200~`
/// and `ESC [201~` as it is, less every Esc, so nothing in it can end the paste early. Without
/// it, each line break goes as the Enter key's `\r`. Alacritty sends pastes the same way.
pub(super) fn paste(text: &str, bracketed: bool) -> String {
    if bracketed {
        let mut sent = String::with_capacity(text.len() + 12);
        sent.push_str("\x1b[200~");
        sent.extend(text.chars().filter(|&c| c != '\x1b'));
        sent.push_str("\x1b[201~");
        sent
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r")
    }
}

/// `rows` presses of the up or down arrow, which a program on the alternate screen asks the
/// wheel to send.
pub(super) fn arrows(up: bool, rows: usize, application: bool) -> String {
    cursor(if up { 'A' } else { 'B' }, 1, application).repeat(rows)
}

/// The control character Ctrl and `c` type, as xterm maps them, or `None` for a key Ctrl
/// doesn't change.
fn control(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0x00,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '~' | '6' => 0x1e,
        '_' | '-' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

fn with_alt(text: &str, alt: bool) -> String {
    if alt {
        format!("\x1b{text}")
    } else {
        text.to_string()
    }
}

/// A cursor key, Home or End: `CSI final`, `SS3 final` in application cursor mode, and
/// `CSI 1 ; param final` with a modifier in either mode.
fn cursor(last: char, param: u8, application: bool) -> String {
    match (param, application) {
        (1, true) => format!("\x1bO{last}"),
        (1, false) => format!("\x1b[{last}"),
        _ => format!("\x1b[1;{param}{last}"),
    }
}

/// A key xterm sends as `CSI number ~`, and `CSI number ; param ~` with a modifier.
fn tilde(number: u8, param: u8) -> String {
    if param == 1 {
        format!("\x1b[{number}~")
    } else {
        format!("\x1b[{number};{param}~")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(code: KeyCode, modifiers: KeyModifiers) -> Option<String> {
        encode(&KeyEvent::new(code, modifiers), InputModes::default())
    }

    fn plain(code: KeyCode) -> String {
        sent(code, KeyModifiers::NONE).unwrap()
    }

    #[test]
    fn text_goes_as_utf_8_and_alt_puts_esc_before_it() {
        assert_eq!(plain(KeyCode::Char('a')), "a");
        assert_eq!(sent(KeyCode::Char('A'), KeyModifiers::SHIFT).unwrap(), "A");
        assert_eq!(plain(KeyCode::Char('é')), "é");
        assert_eq!(plain(KeyCode::Char('漢')), "漢");
        assert_eq!(
            sent(KeyCode::Char('x'), KeyModifiers::ALT).unwrap(),
            "\x1bx"
        );
        assert_eq!(
            sent(KeyCode::Char('x'), KeyModifiers::META).unwrap(),
            "\x1bx"
        );
    }

    #[test]
    fn ctrl_types_the_control_characters_xterm_maps() {
        let ctrl = |c| sent(KeyCode::Char(c), KeyModifiers::CONTROL).unwrap();
        assert_eq!(ctrl('c'), "\x03");
        assert_eq!(ctrl('x'), "\x18");
        assert_eq!(ctrl('a'), "\x01");
        assert_eq!(ctrl('z'), "\x1a");
        assert_eq!(ctrl(' '), "\0");
        assert_eq!(ctrl('@'), "\0");
        assert_eq!(ctrl('['), "\x1b");
        assert_eq!(ctrl('\\'), "\x1c");
        assert_eq!(ctrl(']'), "\x1d");
        assert_eq!(ctrl('^'), "\x1e");
        assert_eq!(ctrl('_'), "\x1f");
        assert_eq!(ctrl('?'), "\x7f");
        // A key Ctrl doesn't map types itself.
        assert_eq!(ctrl('1'), "1");
        let shifted = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(sent(KeyCode::Char('C'), shifted).unwrap(), "\x03");
        let both = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert_eq!(sent(KeyCode::Char('c'), both).unwrap(), "\x1b\x03");
    }

    #[test]
    fn enter_tab_backspace_and_esc_send_what_a_terminal_sends() {
        assert_eq!(plain(KeyCode::Enter), "\r");
        assert_eq!(plain(KeyCode::Tab), "\t");
        assert_eq!(plain(KeyCode::BackTab), "\x1b[Z");
        assert_eq!(plain(KeyCode::Backspace), "\x7f");
        assert_eq!(
            sent(KeyCode::Backspace, KeyModifiers::CONTROL).unwrap(),
            "\x08"
        );
        assert_eq!(
            sent(KeyCode::Backspace, KeyModifiers::ALT).unwrap(),
            "\x1b\x7f"
        );
        assert_eq!(plain(KeyCode::Esc), "\x1b");
        assert_eq!(sent(KeyCode::Esc, KeyModifiers::ALT).unwrap(), "\x1b\x1b");
        assert_eq!(sent(KeyCode::Enter, KeyModifiers::ALT).unwrap(), "\x1b\r");
        let newline = InputModes {
            line_feed_new_line: true,
            ..InputModes::default()
        };
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(encode(&enter, newline).unwrap(), "\r\n");
    }

    #[test]
    fn cursor_keys_follow_application_mode_until_a_modifier_is_held() {
        assert_eq!(plain(KeyCode::Up), "\x1b[A");
        assert_eq!(plain(KeyCode::Down), "\x1b[B");
        assert_eq!(plain(KeyCode::Right), "\x1b[C");
        assert_eq!(plain(KeyCode::Left), "\x1b[D");
        assert_eq!(plain(KeyCode::Home), "\x1b[H");
        assert_eq!(plain(KeyCode::End), "\x1b[F");
        let application = InputModes {
            application_cursor: true,
            ..InputModes::default()
        };
        let key = |code, modifiers| encode(&KeyEvent::new(code, modifiers), application).unwrap();
        assert_eq!(key(KeyCode::Up, KeyModifiers::NONE), "\x1bOA");
        assert_eq!(key(KeyCode::Home, KeyModifiers::NONE), "\x1bOH");
        assert_eq!(key(KeyCode::Up, KeyModifiers::CONTROL), "\x1b[1;5A");
        assert_eq!(
            sent(KeyCode::Left, KeyModifiers::SHIFT).unwrap(),
            "\x1b[1;2D"
        );
        assert_eq!(
            sent(KeyCode::Right, KeyModifiers::ALT).unwrap(),
            "\x1b[1;3C"
        );
        let all = KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL;
        assert_eq!(sent(KeyCode::End, all).unwrap(), "\x1b[1;8F");
    }

    #[test]
    fn editing_and_function_keys_send_xterm_sequences_with_their_modifiers() {
        assert_eq!(plain(KeyCode::Insert), "\x1b[2~");
        assert_eq!(plain(KeyCode::Delete), "\x1b[3~");
        assert_eq!(plain(KeyCode::PageUp), "\x1b[5~");
        assert_eq!(plain(KeyCode::PageDown), "\x1b[6~");
        assert_eq!(
            sent(KeyCode::Delete, KeyModifiers::CONTROL).unwrap(),
            "\x1b[3;5~"
        );
        let function: Vec<String> = (1..=12).map(|n| plain(KeyCode::F(n))).collect();
        assert_eq!(
            function,
            [
                "\x1bOP", "\x1bOQ", "\x1bOR", "\x1bOS", "\x1b[15~", "\x1b[17~", "\x1b[18~",
                "\x1b[19~", "\x1b[20~", "\x1b[21~", "\x1b[23~", "\x1b[24~"
            ]
        );
        assert_eq!(
            sent(KeyCode::F(1), KeyModifiers::SHIFT).unwrap(),
            "\x1b[1;2P"
        );
        assert_eq!(
            sent(KeyCode::F(5), KeyModifiers::CONTROL).unwrap(),
            "\x1b[15;5~"
        );
        assert_eq!(sent(KeyCode::F(13), KeyModifiers::NONE), None);
        assert_eq!(sent(KeyCode::CapsLock, KeyModifiers::NONE), None);
    }

    #[test]
    fn a_paste_brackets_its_text_only_when_the_program_asked() {
        assert_eq!(paste("one\ntwo\r\nthree", false), "one\rtwo\rthree");
        assert_eq!(
            paste("one\ntwo", true),
            "\x1b[200~one\ntwo\x1b[201~",
            "a bracketed paste keeps its line breaks"
        );
        // An end marker inside the text loses its Esc, so it can't close the paste early.
        assert_eq!(
            paste("a\x1b[201~rm -rf\x1b", true),
            "\x1b[200~a[201~rm -rf\x1b[201~"
        );
    }

    #[test]
    fn the_wheel_sends_arrows_in_the_cursor_mode_the_program_set() {
        assert_eq!(arrows(true, 3, false), "\x1b[A\x1b[A\x1b[A");
        assert_eq!(arrows(false, 2, true), "\x1bOB\x1bOB");
    }
}
