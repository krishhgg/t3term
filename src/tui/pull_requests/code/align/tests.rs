use super::super::whitespace::hide;
use super::*;
use crate::pull_request::patch::{File, Patch};
use serde_json::{Value, json};

/// Each hunk of `files`' parts as pierre.mjs writes the desktop's: a run of lines in context
/// with its count, and a change with its counts of lines removed and added.
fn listed(patch: &str, files: &[File]) -> Value {
    let hunk = |lines: &[Line]| -> Value {
        let mut listed: Vec<Value> = Vec::new();
        parts(patch, lines, &mut |part| match part {
            Part::Context(_) => match listed.last_mut().filter(|last| last[0] == "context") {
                Some(last) => last[1] = json!(last[1].as_u64().unwrap() + 1),
                None => listed.push(json!(["context", 1])),
            },
            Part::Change(removed, added) => {
                listed.push(json!(["change", removed.len(), added.len()]))
            }
            Part::Note(_) => {}
        });
        Value::from(listed)
    };
    let file = |file: &File| -> Value {
        let hunks = file.hunks.iter().map(|each| hunk(&each.lines));
        Value::from(hunks.collect::<Vec<_>>())
    };
    Value::from(files.iter().map(file).collect::<Vec<_>>())
}

/// How many changes in `listed` split in two or three.
fn split(listed: &Value) -> usize {
    let hunks = listed.as_array().unwrap().iter();
    hunks
        .flat_map(|file| file.as_array().unwrap())
        .map(|hunk| {
            let parts = hunk.as_array().unwrap();
            let change = |at: usize| parts.get(at).is_some_and(|part| part[0] == "change");
            (1..parts.len())
                .filter(|&at| change(at) && change(at - 1) && !change(at.wrapping_sub(2)))
                .count()
        })
        .sum()
}

#[test]
fn each_change_splits_as_the_desktops_does() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("pierre.json")).unwrap();
    assert_eq!(cases.len(), 63);
    let (mut shown, mut hidden) = (0, 0);
    for case in cases {
        let patch = case["patch"].as_str().unwrap();
        let read = Patch::read(patch);
        assert_eq!(listed(patch, &read.files), case["shown"], "{patch:?}");
        shown += split(&case["shown"]);
        let without = hide(patch, &read.files);
        assert_eq!(without.kept, 0);
        assert_eq!(listed(patch, &without.files), case["hidden"], "{patch:?}");
        hidden += split(&case["hidden"]);
    }
    assert_eq!((shown, hidden), (39, 32));
}

/// A patch of one file with a hunk of `lines`, each with its mark, whose header counts them.
fn patch_of(lines: &[&str]) -> String {
    let count = |marks: &[char]| {
        let on = |line: &&&str| {
            line.chars()
                .next()
                .is_some_and(|mark| marks.contains(&mark))
        };
        lines.iter().filter(on).count()
    };
    let (old, new) = (count(&[' ', '-']), count(&[' ', '+']));
    let mut patch = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,{old} +1,{new} @@\n");
    for line in lines {
        patch.push_str(line);
        patch.push('\n');
    }
    patch
}

/// The lines of `patch`'s one hunk as they stack, each with its mark, and side by side, each
/// row's two sides with `│` between them.
fn shown(patch: &str) -> (Vec<String>, Vec<String>) {
    let read = Patch::read(patch);
    let lines = &read.files[0].hunks[0].lines;
    let marked = |line: &Line| {
        let mark = match line.kind {
            Kind::Context => " ",
            Kind::Added => "+",
            Kind::Removed => "-",
            Kind::NoNewline => "",
        };
        format!("{mark}{}", &patch[line.text.clone()])
    };
    let stacked = stacked(patch, lines).into_iter().map(marked).collect();
    let side = |line: Option<&Line>| line.map(marked).unwrap_or_default();
    let pairs = pairs(patch, lines);
    let beside = pairs
        .into_iter()
        .map(|(old, new)| format!("{}│{}", side(old), side(new)));
    (stacked, beside.collect())
}

#[test]
fn the_lines_left_over_show_where_the_lines_that_pair_are_most_alike() {
    // `run(2);` is more like `run(1);` than `setup();` is, so the line added before it shows
    // first and pairs with nothing.
    let patch = patch_of(&[
        " fn main() {",
        "-    run(1);",
        "+    setup();",
        "+    run(2);",
        " }",
    ]);
    let (stacked, beside) = shown(&patch);
    assert_eq!(
        stacked,
        [
            " fn main() {",
            "+    setup();",
            "-    run(1);",
            "+    run(2);",
            " }"
        ]
    );
    assert_eq!(
        beside,
        [
            " fn main() {│ fn main() {",
            "│+    setup();",
            "-    run(1);│+    run(2);",
            " }│ }"
        ]
    );

    // Lines left over can show before and after the lines that pair. The lines that pair stay
    // together, so a change with lines left over between them pairs as it can.
    let (stacked, beside) = shown(&patch_of(&["-x", "-a(1)", "-y", "+a(2)"]));
    assert_eq!(stacked, ["-x", "-a(1)", "+a(2)", "-y"]);
    assert_eq!(beside, ["-x│", "-a(1)│+a(2)", "-y│"]);
    let (stacked, beside) = shown(&patch_of(&[
        "-use old;",
        "-a(1)",
        "-b(1)",
        "+a(2)",
        "+b(2)",
    ]));
    assert_eq!(stacked, ["-use old;", "-a(1)", "-b(1)", "+a(2)", "+b(2)"]);
    assert_eq!(beside, ["-use old;│", "-a(1)│+a(2)", "-b(1)│+b(2)"]);

    // A change whose sides have as many lines, and any run of lines in context, shows as the
    // patch has it, the lines removed first.
    let (stacked, beside) = shown(&patch_of(&["-a", "+x", "-a(1)", "+a(1)", " end"]));
    assert_eq!(stacked, ["-a", "-a(1)", "+x", "+a(1)", " end"]);
    assert_eq!(beside, ["-a│+x", "-a(1)│+a(1)", " end│ end"]);
}

#[test]
fn the_lines_pair_in_order_unless_another_place_is_over_half_a_point_more_alike_a_pair() {
    // `xb` is half like `ab`, and the half point the first place gets makes it as good as `ab`
    // itself, so the lines stay in order.
    let (stacked, _) = shown(&patch_of(&["-ab", "+xb", "+ab"]));
    assert_eq!(stacked, ["-ab", "+xb", "+ab"]);
    // `xy` is nothing like it.
    let (stacked, _) = shown(&patch_of(&["-ab", "+xy", "+ab"]));
    assert_eq!(stacked, ["+xy", "-ab", "+ab"]);
    // Of places as alike, the first wins.
    let (_, beside) = shown(&patch_of(&["-ab", "+xy", "+ab", "+ab"]));
    assert_eq!(beside, ["│+xy", "-ab│+ab", "│+ab"]);
}

#[test]
fn a_character_past_the_basic_plane_counts_as_two_code_units() {
    // 😀 and 😃 share the first of their two UTF-16 code units, so pierre finds them half alike,
    // and the lines stay in order. Counted as characters, they would be nothing alike and the
    // second 😀 would pair with the first.
    let (stacked, _) = shown(&patch_of(&["-😀", "+😃", "+😀"]));
    assert_eq!(stacked, ["-😀", "+😃", "+😀"]);
}

#[test]
fn the_whitespace_in_a_line_doesnt_count() {
    // Without its whitespace `  run( 1 );` is `run(1);` itself.
    let (stacked, _) = shown(&patch_of(&["-run(1);", "+setup();", "+  run( 1 );"]));
    assert_eq!(stacked, ["+setup();", "-run(1);", "+  run( 1 );"]);
    // U+3000 is whitespace to JavaScript, where U+0085 isn't, though Rust counts both. With
    // two U+0085 left in, the line is less like `abcd` than `axyz` is with the half point.
    let (stacked, _) = shown(&patch_of(&["-abcd", "+axyz", "+a\u{3000}b\u{3000}cd"]));
    assert_eq!(stacked, ["+axyz", "-abcd", "+a\u{3000}b\u{3000}cd"]);
    let (stacked, _) = shown(&patch_of(&["-abcd", "+axyz", "+a\u{85}b\u{85}cd"]));
    assert_eq!(stacked, ["-abcd", "+axyz", "+a\u{85}b\u{85}cd"]);
}

#[test]
fn a_change_with_more_than_4096_pairs_to_compare_pairs_in_order() {
    // One line removed and 4,096 added: 1 pair at each of 4,096 places.
    let order = |others: usize| {
        let mut lines = vec!["-target".to_string()];
        lines.extend((0..others).map(|at| format!("+other {at}")));
        lines.push("+target".to_string());
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (stacked, _) = shown(&patch_of(&lines));
        stacked
    };
    let stacked = order(4095);
    assert_eq!(stacked[4094..], ["+other 4094", "-target", "+target"]);
    let stacked = order(4096);
    assert_eq!(stacked[..2], ["-target", "+other 0"]);
    assert_eq!(stacked.len(), 4098);
}

#[test]
fn a_note_that_a_line_has_no_end_stays_after_it() {
    let patch = patch_of(&["-a(1)", "\\ No newline at end of file", "+b", "+a(2)"]);
    let (stacked, beside) = shown(&patch.replace("+a(2)\n", "+a(2)\n\\ No newline\n"));
    assert_eq!(
        stacked,
        [
            "+b",
            "-a(1)",
            "\\ No newline at end of file",
            "+a(2)",
            "\\ No newline"
        ]
    );
    // Side by side the notes show under the hunk, as `ends` says.
    assert_eq!(beside, ["│+b", "-a(1)│+a(2)"]);

    // A note with no line before it shows first where the lines stack.
    let (stacked, beside) = shown(&patch_of(&["\\ No newline", " a"]));
    assert_eq!(stacked, ["\\ No newline", " a"]);
    assert_eq!(beside, [" a│ a"]);
}

#[test]
fn a_line_that_starts_another_counts_its_characters_once() {
    // `aa` starts `aaa` and ends it, but each of its characters counts once, so `aaa` is two
    // thirds like it. That is less than `ax`, half like it, with the half point, so the lines
    // stay in order.
    let (stacked, _) = shown(&patch_of(&["-aa", "+ax", "+aaa"]));
    assert_eq!(stacked, ["-aa", "+ax", "+aaa"]);
}
