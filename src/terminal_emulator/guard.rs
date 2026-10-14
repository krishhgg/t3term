//! Caps how much of one OSC string reaches the parser.
//!
//! vte 0.15 keeps an OSC string's bytes in a `Vec` with no limit until the string ends, so a
//! program could make it grow without bound. This guard tracks the three parser states that
//! decide whether a byte joins that buffer, following vte 0.15.0's `advance_esc` and
//! `advance_osc_string` byte for byte. Once a string has passed the cap, the guard drops its
//! bytes up to the one that ends or cancels it. It never drops that byte, or any byte outside
//! a string, so displayed text and the sequences after a long string arrive intact. DCS, APC, PM and SOS strings need no guard:
//! vte keeps none of their bytes.
//!
//! The guard sees the stream before the parser does, including bytes the parser holds back
//! during a synchronized update, so what the parser buffers is already capped.

/// Payload bytes one OSC string may add to vte's buffer. Bytes past it are dropped.
pub(super) const MAX_OSC_PAYLOAD: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    /// Any parser state other than the two below. vte moves from each of them to Escape on
    /// ESC, and from none of them straight into an OSC string.
    #[default]
    Other,
    /// After ESC, where `]` starts an OSC string.
    Escape,
    /// Inside an OSC string.
    Osc,
}

#[derive(Debug, Default)]
pub(super) struct OscGuard {
    state: State,
    /// Payload bytes the current OSC string has passed to the parser.
    payload: usize,
}

impl OscGuard {
    /// Calls `forward` with each run of `bytes` the parser should see, in order. State carries
    /// over from the previous call, so a string split across chunks is capped as one.
    ///
    /// The caller passes whole UTF-8 text. vte 0.15 can drop an ESC that follows a character
    /// split across two calls, and then this model would no longer match the parser.
    pub(super) fn filter(&mut self, bytes: &[u8], mut forward: impl FnMut(&[u8])) {
        let mut start = 0;
        let mut index = 0;
        while index < bytes.len() {
            match self.state {
                State::Other => match bytes[index..].iter().position(|&byte| byte == 0x1B) {
                    Some(offset) => {
                        index += offset + 1;
                        self.state = State::Escape;
                    }
                    None => index = bytes.len(),
                },
                State::Escape => {
                    self.state = match bytes[index] {
                        b']' => {
                            self.payload = 0;
                            State::Osc
                        }
                        // vte executes these C0 controls, or ignores these bytes, and stays in
                        // Escape. CAN and SUB, 0x18 and 0x1A, go back to ground.
                        0x00..=0x17 | 0x19 | 0x1B..=0x1F | 0x7F..=0xFF => State::Escape,
                        _ => State::Other,
                    };
                    index += 1;
                }
                State::Osc => {
                    let byte = bytes[index];
                    match byte {
                        // vte ends the string on BEL, CAN and SUB, and on ESC, which also
                        // starts an escape.
                        0x07 | 0x18 | 0x1A => self.state = State::Other,
                        0x1B => self.state = State::Escape,
                        _ if self.payload >= MAX_OSC_PAYLOAD => {
                            if start < index {
                                forward(&bytes[start..index]);
                            }
                            start = index + 1;
                        }
                        // vte ignores the other C0 controls inside the string, and `;` only
                        // marks where a parameter ends, so neither adds to the buffer.
                        0x00..=0x1F | b';' => {}
                        _ => self.payload += 1,
                    }
                    index += 1;
                }
            }
        }
        if start < bytes.len() {
            forward(&bytes[start..]);
        }
    }
}
