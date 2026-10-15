use super::*;
use crate::pull_request::patch::Patch;
use serde_json::{Value, json};

/// Each hunk of `files` as jsdiff.mjs writes the desktop's: its runs of lines in context and of
/// lines changed, each with where it starts among the hunk's lines before and after the
/// change, and the lines it adds and removes.
fn runs(files: &[File]) -> Value {
    let hunks = |file: &File| -> Vec<Value> {
        file.hunks
            .iter()
            .map(|hunk| {
                let mut runs: Vec<Value> = Vec::new();
                let (mut old, mut new) = (0, 0);
                let (mut additions, mut deletions) = (0, 0);
                for line in &hunk.lines {
                    let context = line.kind == Kind::Context;
                    let last = runs.last_mut().filter(|run| match context {
                        true => run[0] == "context",
                        false => run[0] == "change",
                    });
                    match (line.kind, last) {
                        (Kind::NoNewline, _) => continue,
                        (Kind::Context, Some(run)) => run[1] = json!(run[1].as_u64().unwrap() + 1),
                        (Kind::Context, None) => runs.push(json!(["context", 1, old, new])),
                        (_, None) => runs.push(json!(["change", 0, 0, old, new])),
                        (_, Some(_)) => {}
                    }
                    let run = runs.last_mut().unwrap();
                    match line.kind {
                        Kind::Removed => {
                            run[1] = json!(run[1].as_u64().unwrap() + 1);
                            deletions += 1;
                        }
                        Kind::Added => {
                            run[2] = json!(run[2].as_u64().unwrap() + 1);
                            additions += 1;
                        }
                        Kind::Context | Kind::NoNewline => {}
                    }
                    old += u64::from(line.kind != Kind::Added);
                    new += u64::from(line.kind != Kind::Removed);
                }
                json!({"blocks": runs, "additions": additions, "deletions": deletions})
            })
            .collect()
    };
    Value::from(files.iter().map(hunks).collect::<Vec<_>>())
}

/// The patch's text at `line`.
fn text<'a>(patch: &'a str, line: &Line) -> &'a str {
    &patch[line.text.clone()]
}

/// Each line of `file`'s first hunk as git marks it, with its numbers and text.
fn marked(patch: &str, file: &File) -> Vec<String> {
    file.hunks[0]
        .lines
        .iter()
        .map(|line| {
            let mark = match line.kind {
                Kind::Context => " ",
                Kind::Added => "+",
                Kind::Removed => "-",
                Kind::NoNewline => "",
            };
            let number = |at: Option<u64>| at.map_or("_".to_string(), |at| at.to_string());
            let numbers = format!("{} {}", number(line.old), number(line.new));
            format!("{numbers} {mark}{}", text(patch, line))
        })
        .collect()
}

/// The patch of one file, `f`, with a hunk from `header` of `lines`.
fn patch_of(header: &str, lines: &[&str]) -> String {
    let mut patch = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n{header}\n");
    for line in lines {
        patch.push_str(line);
        patch.push('\n');
    }
    patch
}

#[test]
fn every_hunk_pairs_its_lines_as_the_desktops_filter_does() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("jsdiff.json")).unwrap();
    assert_eq!(cases.len(), 59);
    let mut hunks = 0;
    for case in cases {
        let patch = case["patch"].as_str().unwrap();
        let read = Patch::read(patch);
        let hidden = hide(patch, &read.files);
        assert_eq!(hidden.kept, 0);
        assert_eq!(runs(&hidden.files), case["files"], "{patch:?}");
        hunks += read
            .files
            .iter()
            .map(|file| file.hunks.len())
            .sum::<usize>();
    }
    assert_eq!(hunks, 261);
}

#[test]
fn a_line_changed_only_in_whitespace_shows_unchanged_with_its_text_after() {
    let patch = patch_of(
        "@@ -3,4 +3,4 @@ fn main()",
        &[
            " {",
            "-  let a = 1;",
            "-  let b = 2;",
            "+    let a = 1;",
            "+    let c = 3;",
            " }",
        ],
    );
    let read = Patch::read(&patch);
    let hidden = hide(&patch, &read.files);
    let file = &hidden.files[0];
    assert_eq!(
        marked(&patch, file),
        [
            "3 3  {",
            "4 4      let a = 1;",
            "5 _ -  let b = 2;",
            "_ 5 +    let c = 3;",
            "6 6  }",
        ]
    );
    assert_eq!((file.additions, file.deletions), (1, 1));
    assert_eq!(&patch[file.hunks[0].section.clone()], "fn main()");
    let hunk = &file.hunks[0];
    let header = (
        hunk.old_start,
        hunk.old_count,
        hunk.new_start,
        hunk.new_count,
    );
    assert_eq!(header, (3, 4, 3, 4));
    assert_eq!(hidden.kept, 0);
    // The patch as read stays as it was.
    assert_eq!((read.files[0].additions, read.files[0].deletions), (2, 2));
}

#[test]
fn whitespace_is_what_javascript_calls_it() {
    // U+FEFF is whitespace to JavaScript, and U+0085 and U+200B aren't.
    let patch = patch_of(
        "@@ -1,3 +1,3 @@",
        &[
            "-\u{feff}a\u{a0}b\u{3000}",
            "-c",
            "-d",
            "+ab",
            "+c\u{85}",
            "+d\u{200b}",
        ],
    );
    let read = Patch::read(&patch);
    let hidden = hide(&patch, &read.files);
    assert_eq!(
        marked(&patch, &hidden.files[0]),
        [
            "1 1  ab",
            "2 _ -c",
            "3 _ -d",
            "_ 2 +c\u{85}",
            "_ 3 +d\u{200b}",
        ]
    );
    assert!(js_space('\u{feff}') && !'\u{feff}'.is_whitespace());
    assert!(!js_space('\u{85}') && '\u{85}'.is_whitespace());
    let spaces =
        "\t\n\u{b}\u{c}\r \u{a0}\u{1680}\u{2000}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}";
    assert!(spaces.chars().all(js_space));
    assert!("a\u{200b}\u{180e}\u{1f}".chars().all(|c| !js_space(c)));
}

#[test]
fn a_hunk_of_whitespace_alone_counts_no_lines() {
    let patch = patch_of(
        "@@ -1,3 +1,3 @@",
        &["-if a {", "-\tb()", "+if a{", "+  b ( )", " }"],
    );
    let read = Patch::read(&patch);
    let file = &hide(&patch, &read.files).files[0];
    assert_eq!(
        marked(&patch, file),
        ["1 1  if a{", "2 2    b ( )", "3 3  }"]
    );
    assert_eq!((file.additions, file.deletions), (0, 0));
}

#[test]
fn a_line_with_no_end_keeps_its_note_where_the_desktop_shows_it() {
    let hidden = |lines: &[&str]| {
        let olds = lines
            .iter()
            .filter(|line| line.starts_with([' ', '-']))
            .count();
        let news = lines
            .iter()
            .filter(|line| line.starts_with([' ', '+']))
            .count();
        let patch = patch_of(&format!("@@ -1,{olds} +1,{news} @@"), lines);
        let read = Patch::read(&patch);
        marked(&patch, &hide(&patch, &read.files).files[0])
    };
    let no_end = "\\ No newline at end of file";
    let note = "_ _ \\ No newline at end of file";
    // Both sides' last line, which shows unchanged, has one note.
    assert_eq!(hidden(&["-a", no_end, "+ a", no_end]), ["1 1   a", note]);
    assert_eq!(hidden(&[" a", no_end]), ["1 1  a", note]);
    // A line end added or dropped is whitespace too, and the side without one keeps its note.
    assert_eq!(hidden(&["-a", no_end, "+a"]), ["1 1  a", note]);
    assert_eq!(hidden(&["-a", "+a", no_end]), ["1 1  a", note]);
    // The removed lines' note follows the last of them, before the lines added.
    assert_eq!(
        hidden(&["-a", "-b", no_end, "+c", no_end]),
        ["1 _ -a", "2 _ -b", note, "_ 1 +c", note]
    );
    // Where the last line before the change shows unchanged above lines added, the last run
    // has none of its lines and its note goes, as the desktop's does.
    assert_eq!(hidden(&["-a", no_end, "+a", "+b"]), ["1 1  a", "_ 2 +b"]);
    assert_eq!(
        hidden(&["-a", "-b", no_end, "+a", "+c"]),
        ["1 1  a", "2 _ -b", note, "_ 2 +c"]
    );
    assert_eq!(hidden(&["-a", "-b", "+a", no_end]), ["1 1  a", "2 _ -b"]);
    // A last line in context that pairs with nothing is removed or added, and the note that
    // was both sides' follows it, as @pierre/diffs 1.5.2 marks it.
    assert_eq!(
        hidden(&["-  x", "-y", " x", no_end]),
        ["1 1  x", "2 _ -y", "3 _ -x", note]
    );
    assert_eq!(
        hidden(&["+  x", "+y", " x", no_end]),
        ["1 1    x", "_ 2 +y", "_ 3 +x", note]
    );
}

/// The patch of a file at `path` whose hunk removes `x` and `count` - 1 lines and adds ` x` and
/// `count` - 1 others.
fn rewritten(path: &str, count: usize) -> String {
    let mut lines = vec!["-x".to_string()];
    lines.extend((1..count).map(|at| format!("-old {at}")));
    lines.push("+ x".into());
    lines.extend((1..count).map(|at| format!("+new {at}")));
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    patch_of(&format!("@@ -1,{count} +1,{count} @@"), &lines)
        .replace("a/f b/f", &format!("a/{path} b/{path}"))
}

#[test]
fn hunks_past_the_steps_left_show_as_the_patch_has_them() {
    // 2,000 lines removed and 2,000 others added take more than `MOST_STEPS` to compare, and
    // a smaller hunk after it is compared first.
    let mut patch = rewritten("f", 2_000);
    patch.push_str(&rewritten("g", 2));
    let read = Patch::read(&patch);
    let hidden = hide(&patch, &read.files);
    assert_eq!(hidden.kept, 1);
    assert_eq!(hidden.files[0], read.files[0]);
    assert_eq!(
        marked(&patch, &hidden.files[1]),
        ["1 1   x", "2 _ -old 1", "_ 2 +new 1"]
    );

    // 1,500 and 1,500 take about 2,250,000, so the steps compare one of two such hunks, the
    // first, and the one smaller than both.
    let mut patch = rewritten("f", 1_500);
    patch.push_str(&rewritten("g", 1_500));
    patch.push_str(&rewritten("h", 2));
    let read = Patch::read(&patch);
    let hidden = hide(&patch, &read.files);
    assert_eq!(hidden.kept, 1);
    assert_eq!(marked(&patch, &hidden.files[0])[0], "1 1   x");
    assert_eq!(hidden.files[1], read.files[1]);
    assert_eq!(marked(&patch, &hidden.files[2])[0], "1 1   x");
}

#[test]
fn a_file_past_the_bound_keeps_the_counts_of_the_hunks_left_out() {
    let patch = patch_of("@@ -1,2 +1,2 @@", &["-a", "-b", "+ a", "+c"]);
    let mut read = Patch::read(&patch);
    // As if a hunk of 5 lines added and 3 removed was left out past `MOST_LINES`.
    read.files[0].additions += 5;
    read.files[0].deletions += 3;
    read.files[0].lines_cut = true;
    let file = &hide(&patch, &read.files).files[0];
    assert_eq!((file.additions, file.deletions), (6, 4));
    assert!(file.lines_cut);
}

#[test]
fn compare_takes_jsdiffs_path_and_counts_its_steps() {
    use Step::{Added, Removed, Same};
    let compared = |old: &[u32], new: &[u32]| {
        let mut left = usize::MAX;
        let steps = compare(old, new, &mut left).unwrap();
        (steps, usize::MAX - left)
    };
    assert_eq!(compared(&[], &[]), (vec![], 0));
    assert_eq!(compared(&[1, 2], &[1, 2]), (vec![Same, Same], 2));
    assert_eq!(compared(&[1], &[]), (vec![Removed], 2));
    assert_eq!(compared(&[], &[1, 2]), (vec![Added, Added], 2));
    // Of two paths as long, jsdiff takes the one further along the old side, and where both
    // are as far, the one that removes.
    assert_eq!(compared(&[1], &[2]), (vec![Removed, Added], 3));
    assert_eq!(
        compared(&[1, 2, 3], &[2, 3, 1]),
        (vec![Removed, Same, Same, Added], 7)
    );
    assert_eq!(
        compared(&[1, 2, 1], &[2, 1, 2]),
        (vec![Removed, Same, Same, Added], 7)
    );
    assert_eq!(
        compared(&[1, 1, 2], &[2, 1, 1]),
        (vec![Added, Same, Same, Removed], 5)
    );
    assert_eq!(
        compared(&[1, 2, 3, 4], &[4, 3, 2, 1]),
        (
            vec![Removed, Removed, Removed, Same, Added, Added, Added],
            19
        )
    );
    // The steps run out on the last one wanted.
    let mut left = 3;
    assert!(compare(&[1], &[2], &mut left).is_some());
    assert_eq!(left, 0);
    let mut left = 2;
    assert_eq!(compare(&[1], &[2], &mut left), None);
    let mut left = 1;
    assert_eq!(compare(&[1, 2], &[1, 2], &mut left), None);
}
