//! Parses T3's frames, including the UTF-16 surrogates without a pair that JavaScript writes.
//!
//! T3 writes each frame with JavaScript's `JSON.stringify`. A JavaScript string is UTF-16, and
//! T3 can cut one between the two halves of a surrogate pair: `truncateTerminalWireLabel` in
//! apps/server/src/terminal/Manager.ts keeps the first 128 code units of a terminal's label,
//! so a label whose 128th unit starts an emoji ends with half of it. `JSON.stringify` writes
//! that half as an escape such as `\ud83d`. A Rust string holds Unicode scalar values, which
//! leave surrogates out, so serde_json refuses the whole frame.
//!
//! [`decode`] parses a frame as it is first, so a frame serde_json takes costs nothing more.
//! Only if serde_json refuses it does `decode` copy it, write `\ufffd` over each escape of a
//! surrogate without its pair in a string value, and parse the copy once. U+FFFD REPLACEMENT
//! CHARACTER takes the half's place, and the half itself is lost, since no Rust string can
//! hold it. An object's key keeps its escape, so serde_json still refuses the frame: two keys
//! that differ only in such a half would both become U+FFFD and collide.
//!
//! `\ufffd` is as long as the escape it replaces, so the copy is exactly the frame's length,
//! and the socket's limit on a message bounds it too. Finding the escapes reads the copy once,
//! and a string that has one twice. Only the four hex digits of each such escape change. The
//! copy must still parse, so a frame that is wrong in any other way stays refused.

use serde_json::Value;

/// Parses `text`, one frame from T3. A frame serde_json refuses gets one more try, with each
/// surrogate without its pair in a string value replaced by U+FFFD.
pub(super) fn decode(text: &str) -> serde_json::Result<Value> {
    let refused = match serde_json::from_str::<Value>(text) {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    // serde_json refuses a surrogate without its pair as a syntax error, so a frame it refused
    // for any other kind of reason gets no copy.
    if !refused.is_syntax() {
        return Err(refused);
    }
    let mut copy = text.as_bytes().to_vec();
    if replace_lone_surrogates(&mut copy) == 0 {
        return Err(refused);
    }
    serde_json::from_slice(&copy)
}

/// Writes `fffd` over the four hex digits of each `\u` escape in a string value whose
/// surrogate has no pair, and returns how many it changed. An object's key keeps its escapes.
fn replace_lone_surrogates(json: &mut [u8]) -> usize {
    let mut replaced = 0;
    let mut at = 0;
    // Outside a string, a quote can only start one.
    while let Some(quote) = json[at..].iter().position(|&byte| byte == b'"') {
        let start = at + quote + 1;
        let (end, lone) = walk_string(json, start, false);
        if lone > 0 && !is_key(json, end) {
            walk_string(json, start, true);
            replaced += lone;
        }
        at = end;
    }
    replaced
}

/// Reads the string whose text starts at `start`, just after its opening quote. Returns where
/// it ends, just after its closing quote or at the end of `json`, and how many of its `\u`
/// escapes are of a surrogate without its pair. With `replace`, it writes `fffd` over the hex
/// digits of each of those.
fn walk_string(json: &mut [u8], start: usize, replace: bool) -> (usize, usize) {
    let mut lone = 0;
    let mut at = start;
    while let Some(&byte) = json.get(at) {
        match byte {
            b'"' => return (at + 1, lone),
            b'\\' if json.get(at + 1) == Some(&b'u') => {
                let Some(unit) = hex_unit(json, at + 2) else {
                    // serde_json refuses the escape, in the copy too.
                    at += 2;
                    continue;
                };
                if (0xD800..0xDC00).contains(&unit) && low_follows(json, at + 6) {
                    at += 12;
                    continue;
                }
                if (0xD800..0xE000).contains(&unit) {
                    lone += 1;
                    if replace {
                        json[at + 2..at + 6].copy_from_slice(b"fffd");
                    }
                }
                at += 6;
            }
            // Every other escape is two bytes, `\\` and `\"` among them, so the `u` after an
            // escaped backslash is text.
            b'\\' => at += 2,
            _ => at += 1,
        }
    }
    (json.len(), lone)
}

/// The UTF-16 code unit the four hex digits at `at` spell, if four are there.
fn hex_unit(json: &[u8], at: usize) -> Option<u32> {
    let digits = json.get(at..at + 4)?;
    digits.iter().try_fold(0, |unit, &digit| {
        Some((unit << 4) | char::from(digit).to_digit(16)?)
    })
}

/// Whether the escape of a low surrogate starts at `at`, to pair with the high one before it.
fn low_follows(json: &[u8], at: usize) -> bool {
    json.get(at..at + 2) == Some(b"\\u".as_slice())
        && hex_unit(json, at + 2).is_some_and(|unit| (0xDC00..0xE000).contains(&unit))
}

/// Whether the string that ends at `end` is an object's key. In JSON that parses, the first
/// byte after a key other than JSON's whitespace is a colon, and after a value it never is.
fn is_key(json: &[u8], end: usize) -> bool {
    let mut after = json[end..].iter().copied();
    after.find(|&byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r')) == Some(b':')
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_label_cut_inside_a_pair_ends_in_u_fffd_and_one_that_fits_is_untouched() {
        // T3 keeps the first 128 UTF-16 code units of a label. For 127 x's and 🙂, that ends
        // with 🙂's high half, which JSON.stringify writes as an escape.
        let cut = format!(r#"{{"label":"{}\ud83d"}}"#, "x".repeat(127));
        let label = decode(&cut).unwrap()["label"].clone();
        assert_eq!(label, format!("{}\u{fffd}", "x".repeat(127)));
        // 126 x's and 🙂 take 128 code units, so T3 keeps the pair whole, and JSON.stringify
        // writes it as it is.
        let whole = format!(r#"{{"label":"{}🙂"}}"#, "x".repeat(126));
        let label = decode(&whole).unwrap()["label"].clone();
        assert_eq!(label, format!("{}🙂", "x".repeat(126)));
    }

    #[test]
    fn each_unpaired_half_in_a_string_value_becomes_u_fffd_and_nothing_else_changes() {
        let cases = [
            // A low half first, and a high half before an escape that isn't a low half.
            (r#"["\ude42abc"]"#, json!(["\u{fffd}abc"])),
            (
                r#"["\ud83d\u0041","\uD83D\n"]"#,
                json!(["\u{fffd}A", "\u{fffd}\n"]),
            ),
            // A high half before a whole pair, and a low half after one.
            (r#"["\ud83d\ud83d\ude42"]"#, json!(["\u{fffd}🙂"])),
            (r#"["\uD83D\uDE42\ude42"]"#, json!(["🙂\u{fffd}"])),
            // An escaped backslash before a half, and one before text that looks like an
            // escape.
            (
                r#"["\\\ud83d","\\ud83d"]"#,
                json!(["\\\u{fffd}", r"\ud83d"]),
            ),
            // Halves in nested arrays and objects, beside pairs and other values.
            (
                r#"{"a":["\ud83d",{"b":"x\ude42y"},1,true,null],"c":{"d":"\ud83d\ude42\ud83d"}}"#,
                json!({"a": ["\u{fffd}", {"b": "x\u{fffd}y"}, 1, true, null],
                    "c": {"d": "🙂\u{fffd}"}}),
            ),
            // A value with whitespace after it, before a comma, a closing bracket or brace,
            // and a value that is a colon.
            (
                r#"[ "\ud83d" , ":" ,"\ude42"
                ]"#,
                json!(["\u{fffd}", ":", "\u{fffd}"]),
            ),
            ("{\"k\" :\t\"\\ud83d\"\r\n}", json!({"k": "\u{fffd}"})),
        ];
        for (frame, expected) in cases {
            assert_eq!(decode(frame).unwrap(), expected, "{frame}");
        }
    }

    #[test]
    fn the_copy_differs_only_in_the_hex_digits_of_each_unpaired_escape_in_a_value() {
        let frame = r#"{"a":"\ud83d\ude42\ud83d","\ude42":"\\ud83d","b":["\uDBFF",1]}"#;
        let mut copy = frame.as_bytes().to_vec();
        assert_eq!(replace_lone_surrogates(&mut copy), 2);
        let expected = r#"{"a":"\ud83d\ude42\ufffd","\ude42":"\\ud83d","b":["\ufffd",1]}"#;
        assert_eq!(String::from_utf8(copy).unwrap(), expected);
    }

    #[test]
    fn a_key_with_an_unpaired_half_is_refused_rather_than_renamed() {
        let frames = [
            r#"{"\ud83d":1}"#,
            // Both would become U+FFFD, and so would collide with each other or a key that
            // already is.
            r#"{"\ud83d":1,"\udbff":2}"#,
            r#"{"\ufffd":1,"\ude42":2}"#,
            // A value t3term would repair before a key with whitespace before its colon.
            "{\"a\":\"\\ud83d\", \"\\ude42\" \r\n\t: 1}",
        ];
        for frame in frames {
            assert!(decode(frame).is_err(), "{frame}");
        }
    }

    #[test]
    fn malformed_json_stays_refused_with_or_without_an_unpaired_half() {
        let frames = [
            // Bad hex.
            r#"["\ud8zz"]"#,
            r#"["\ud83d\uZZZZ"]"#,
            r#"["\ud83d","\u12"]"#,
            // An escape cut short, and a string that never ends.
            r#"["\ud83d\"#,
            r#"["\ud83d\u"#,
            r#"["\ud8"#,
            r#"["\ud83d"#,
            r#"["\ud83d,1]"#,
            // Something after the end.
            r#"["\ud83d"] x"#,
            r#"["\ud83d"]]"#,
            r#""\ud83d"""#,
            // Numbers JSON doesn't allow.
            r#"[01,"\ud83d"]"#,
            r#"[1.,"\ud83d"]"#,
            r#"[-,"\ud83d"]"#,
            r#"[+1,"\ud83d"]"#,
            r#"[1e,"\ud83d"]"#,
            r#"[NaN,"\ud83d"]"#,
            // An escape outside a string, one JSON doesn't have, and a raw control character.
            r#"[\ud83d]"#,
            r#"["\ud83d\x41"]"#,
            "[\"\\ud83d\u{1}\"]",
        ];
        for frame in frames {
            assert!(decode(frame).is_err(), "{frame}");
        }
    }

    /// A repair that went back over the frame, or parsed it again, for each escape would take
    /// far longer here.
    #[test]
    fn a_frame_of_many_unpaired_halves_and_escaped_backslashes_parses() {
        const COUNT: usize = 50_000;
        let halves = r"\ud83d".repeat(COUNT);
        let backslashes = r"\\".repeat(COUNT);
        let frame = format!(
            r#"["{halves}","{backslashes}u","{}"]"#,
            r#"\ude42",""#.repeat(COUNT)
        );
        let value = decode(&frame).unwrap();
        assert_eq!(value[0], "\u{fffd}".repeat(COUNT));
        assert_eq!(value[1], format!("{}u", "\\".repeat(COUNT)));
        assert_eq!(value.as_array().unwrap().len(), COUNT + 3);
        assert_eq!(value[2], "\u{fffd}");
        assert_eq!(value[COUNT + 2], "");
    }
}
