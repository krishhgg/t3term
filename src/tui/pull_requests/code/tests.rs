use super::super::conversation::Conversation;
use super::*;
use crate::tui::theme::Depth;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

/// Five minutes after the review threads' remarks were written.
const NOW: i64 = 1_792_058_400_000 + 5 * 60_000;

fn theme() -> Theme {
    Theme::new(Depth::TrueColor)
}

/// Each line's text, the padding at its end dropped.
fn rows_of(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect()
}

/// The tab's rows and targets at `width`, with no review threads.
fn laid(code: &Code, width: usize) -> (Vec<Line<'static>>, Targets) {
    code.lay_out(&[], &[], &Shown::default(), width, 0, &theme())
}

/// The tab's rows at `width`.
fn rows(code: &Code, width: usize) -> Vec<String> {
    rows_of(&laid(code, width).0)
}

/// A slice of a diff as T3 sends it, with the cursor of the next where `next` says.
fn slice(patch: &str, next: Option<&str>) -> Box<pull_request::Diff> {
    Box::new(pull_request::Diff {
        patch: patch.to_string(),
        truncated: false,
        next_cursor: next.map(str::to_string),
        omitted: Vec::new(),
        omitted_cut: false,
    })
}

/// The tab once its first slice, `diff`, came at `at`.
fn read(diff: Box<pull_request::Diff>, at: i64) -> Code {
    let mut code = Code::new();
    assert_eq!(code.ask(1), Some(None));
    assert_eq!(code.take(1, Ok(diff), at), Some(true));
    code
}

/// A modified file with two hunks and an unchanged run between them, then an added, a deleted,
/// a renamed and a binary file.
const PATCH: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 use std::fmt;
-fn old() {}
+fn new() {}
 fn keep() {}
@@ -10,2 +10,3 @@ impl Thing {
 fn a() {}
+fn b() {}
 fn c() {}
diff --git a/docs/new.md b/docs/new.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/docs/new.md
@@ -0,0 +1,2 @@
+# New
+Text
\\ No newline at end of file
diff --git a/old.txt b/old.txt
deleted file mode 100644
index 4444444..0000000
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-gone
diff --git a/from.rs b/to.rs
similarity index 100%
rename from from.rs
rename to to.rs
diff --git a/logo.png b/logo.png
index 5555555..6666666 100644
Binary files a/logo.png and b/logo.png differ
";

#[test]
fn files_show_folded_and_each_unfolds_into_its_hunks() {
    // In reading order, which for files that import nothing is by path.
    let mut code = read(slice(PATCH, None), 0);
    assert_eq!(
        rows(&code, 40),
        [
            "5 files",
            "",
            "› A docs/new.md +2 -0",
            "",
            "› M logo.png",
            "",
            "› D old.txt +0 -1",
            "",
            "› M src/lib.rs +2 -1",
            "",
            "› R from.rs → to.rs",
        ]
    );
    let (_, targets) = laid(&code, 40);
    let files: Vec<_> = (0..5).map(Control::File).collect();
    assert_eq!(
        targets,
        [2..3, 4..5, 6..7, 8..9, 10..11]
            .into_iter()
            .zip(files)
            .collect::<Vec<_>>()
    );
    assert_eq!(code.action(Control::File(0), &[]), "expand file");

    // Each line has the number it has after the change, or before it where it was removed,
    // and the unchanged lines between two hunks are counted.
    for at in 0..5 {
        code.act(Control::File(at), &[]);
    }
    assert_eq!(code.action(Control::File(0), &[]), "collapse file");
    assert_eq!(
        rows(&code, 40),
        [
            "5 files",
            "",
            "⌄ A docs/new.md +2 -0",
            "1 + # New",
            "2 + Text",
            "    No newline at end of file",
            "",
            "⌄ M logo.png",
            "A binary file, whose changes have no",
            "lines to show.",
            "",
            "⌄ D old.txt +0 -1",
            "1 - gone",
            "",
            "⌄ M src/lib.rs +2 -1",
            " 1   use std::fmt;",
            " 2 - fn old() {}",
            " 2 + fn new() {}",
            " 3   fn keep() {}",
            "⋯ 6 unmodified lines",
            "10   fn a() {}",
            "11 + fn b() {}",
            "12   fn c() {}",
            "⋯ More unchanged context may be",
            "available",
            "",
            "⌄ R from.rs → to.rs",
            "No lines changed.",
        ]
    );
    let (_, targets) = laid(&code, 40);
    assert_eq!(targets[1], (7..8, Control::File(1)));
    code.act(Control::File(0), &[]);
    assert_eq!(rows(&code, 40)[2..4], ["› A docs/new.md +2 -0", ""]);
}

#[test]
fn the_unchanged_lines_before_a_hunk_count_from_the_line_git_names() {
    // A hunk that removes nothing names the line before it, so the five above it are unchanged,
    // as are the two between a removal at 6 and 7 and the next hunk at 9.
    let patch = "diff --git a/a b/a
--- a/a
+++ b/a
@@ -5,0 +6 @@
+added
@@ -6,1 +7,0 @@
-removed
@@ -9 +9 @@
-x
+y
";
    let mut code = read(slice(patch, None), 0);
    code.act(Control::File(0), &[]);
    assert_eq!(
        rows(&code, 40),
        [
            "1 file",
            "",
            "⌄ M a +2 -2",
            "⋯ 5 unmodified lines",
            "6 + added",
            "6 - removed",
            "⋯ 2 unmodified lines",
            "9 - x",
            "9 + y",
        ]
    );
    // One unchanged line is a line.
    let patch = "diff --git a/a b/a
--- a/a
+++ b/a
@@ -2 +2 @@
-x
+y
";
    let mut code = read(slice(patch, None), 0);
    code.act(Control::File(0), &[]);
    assert_eq!(rows(&code, 40)[3], "⋯ 1 unmodified line");
}

#[test]
fn a_hunk_that_removes_nothing_after_the_highest_line_unfolds() {
    // The parser takes a range that ends at the highest number, so a hunk that removes nothing
    // can name u64::MAX as the line before it. The first line after it can't be numbered, and
    // the count of the lines above stops at the highest.
    let patch = "diff --git a/a b/a
--- a/a
+++ b/a
@@ -18446744073709551615,0 +1 @@
+added
";
    let mut code = read(slice(patch, None), 0);
    code.act(Control::File(0), &[]);
    let rows = rows(&code, 60);
    assert_eq!(
        rows[2..4],
        [
            "⌄ M a +1 -0",
            "⋯ 18,446,744,073,709,551,614 unmodified lines"
        ]
    );
    assert_eq!(rows[4].trim_start(), "1 + added");
}

#[test]
fn the_first_slice_is_read_once_and_the_next_as_the_end_comes_near() {
    let first = "diff --git a/one b/one
--- a/one
+++ b/one
@@ -1 +1 @@
-a
+b
";
    let second = "diff --git a/two b/two
--- a/two
+++ b/two
@@ -1 +1 @@
-c
+d
";
    let mut code = Code::new();
    assert_eq!(rows(&code, 40), ["Loading pull request diff…"]);
    assert_eq!(code.ask(1), Some(None));
    assert_eq!(code.ask(2), None, "one read at a time");
    assert_eq!(code.take(9, Ok(slice(first, Some("2"))), 0), None);
    assert_eq!(code.take(1, Ok(slice(first, Some("2"))), 0), Some(true));
    assert_eq!(code.take(1, Ok(slice(second, None)), 0), None, "taken once");
    assert_eq!(
        rows(&code, 40),
        ["1 file+", "", "› M one +1 -1", "", "Loading more files…"]
    );
    assert!(code.wants_more());

    // The next slice waits until the end of the files comes near.
    assert_eq!(code.ask(2), None);
    assert!(!code.due());
    code.near_end();
    assert!(code.due());
    assert_eq!(code.ask(2), Some(Some("2".to_string())));
    assert!(!code.due());
    code.near_end();
    assert!(!code.due(), "not while it is under way");
    assert_eq!(code.take(2, Ok(slice(second, None)), 0), Some(false));
    assert_eq!(
        rows(&code, 40),
        ["2 files", "", "› M one +1 -1", "", "› M two +1 -1"]
    );
    assert!(!code.wants_more());
    code.near_end();
    assert_eq!(code.ask(3), None, "nothing after the last slice");
}

#[test]
fn a_failure_says_why_and_a_later_slice_s_offers_retry() {
    let first = "diff --git a/one b/one
--- a/one
+++ b/one
@@ -1 +1 @@
-a
+b
";
    // The first slice's failure is all the tab shows, with no Retry, as the desktop's.
    let mut code = Code::new();
    assert_eq!(code.ask(1), Some(None));
    let failure = "Couldn't read this diff: \u{1b}[31mGitHub said no\u{7}";
    assert_eq!(code.take(1, Err(failure.into()), 0), Some(true));
    assert_eq!(
        rows(&code, 40),
        ["Couldn't read this diff: [31mGitHub said", "no"]
    );
    assert!(laid(&code, 40).1.is_empty());
    assert!(!code.has(Control::Retry, &[]));
    assert_eq!(code.ask(2), None, "a failure isn't read again by itself");
    assert!(code.stale(0), "showing the tab again reads it");

    // A later slice's failure shows under the files, with Retry, which reads it again.
    let mut code = read(slice(first, Some("2")), 0);
    code.near_end();
    assert_eq!(code.ask(2), Some(Some("2".to_string())));
    assert_eq!(code.take(2, Err("T3 timed out.".into()), 0), Some(false));
    assert!(!code.wants_more());
    code.near_end();
    assert_eq!(code.ask(3), None);
    assert!(!code.stale(0), "the slices shown came");
    assert_eq!(
        rows(&code, 40),
        [
            "1 file+",
            "",
            "› M one +1 -1",
            "",
            "t3term couldn't read the rest of this",
            "diff.",
            "T3 timed out.",
            "↻ Retry",
        ]
    );
    let (_, targets) = laid(&code, 40);
    assert_eq!(targets[1], (7..8, Control::Retry));
    assert!(code.has(Control::Retry, &[]));
    assert_eq!(code.action(Control::Retry, &[]), "retry");
    code.act(Control::Retry, &[]);
    assert!(!code.has(Control::Retry, &[]));
    assert_eq!(code.ask(3), Some(Some("2".to_string())));

    // A read of the first slice again that failed leaves the slices shown under a note that
    // they may be out of date, with no Retry and no slice read after them. Showing the tab
    // again, or `r`, reads the first slice again.
    let mut code = read(slice(first, Some("2")), 0);
    code.again();
    assert_eq!(code.ask(2), Some(None));
    assert_eq!(code.take(2, Err("Gone.".into()), 0), Some(true));
    let shown = [
        "1 file+",
        "May be out of date: t3term couldn't read",
        "the diff again.",
        "Gone.",
        "",
        "› M one +1 -1",
    ];
    assert_eq!(rows(&code, 40), shown);
    assert!(!code.has(Control::Retry, &[]));
    code.near_end();
    assert_eq!(code.ask(3), None);
    assert!(code.stale(0));
    code.again();
    assert_eq!(code.ask(3), Some(None));
}

#[test]
fn a_read_again_replaces_the_slices_and_keeps_the_files_unfolded() {
    let first = "diff --git a/one b/one
--- a/one
+++ b/one
@@ -1 +1 @@
-a
+b
diff --git a/two b/two
--- a/two
+++ b/two
@@ -1 +1 @@
-c
+d
";
    let again = "diff --git a/two b/two
--- a/two
+++ b/two
@@ -1 +1 @@
-c
+e
";
    let mut code = read(slice(first, Some("2")), 0);
    code.near_end();
    assert_eq!(code.ask(2), Some(Some("2".to_string())));
    assert_eq!(code.take(2, Ok(slice(first, None)), 0), Some(false));
    code.act(Control::File(1), &[]);

    // A read under way when the diff is wanted again is dropped when it comes.
    code.near_end();
    code.again();
    assert_eq!(code.ask(3), Some(None));
    code.again();
    assert_eq!(code.ask(4), None, "one read at a time");
    assert_eq!(code.take(3, Ok(slice(first, None)), 5), None);
    assert_eq!(code.ask(4), Some(None));
    assert_eq!(code.take(4, Ok(slice(again, None)), 7), Some(true));
    assert_eq!(
        rows(&code, 80),
        [
            "1 file",
            "",
            "⌄ M two +1 -1",
            "1 - c",
            "1 + e",
            "⋯ More unchanged context may be available"
        ]
    );

    // The first slice's time says when showing the tab again reads it again.
    assert!(!code.stale(7 + FRESH - 1));
    assert!(code.stale(7 + FRESH));
}

#[test]
fn each_unfolded_file_is_laid_out_once_for_each_width() {
    let first = "diff --git a/big.json b/big.json
--- a/big.json
+++ b/big.json
diff --git a/one b/one
--- a/one
+++ b/one
@@ -1 +1 @@
-a
+b
diff --git a/two b/two
--- a/two
+++ b/two
@@ -1 +1 @@
-c
+d
";
    let later = "diff --git a/three b/three
--- a/three
+++ b/three
@@ -1 +1 @@
-e
+f
";
    let mut code = read(slice(first, Some("2")), 0);
    code.act(Control::File(0), &[]);
    code.act(Control::File(1), &[]);
    let fresh = rows(&code, 40);
    code.prepare(40, &theme());
    // The rows the tab keeps are those it lays out afresh, and it shows those it keeps.
    assert_eq!(rows(&code, 40), fresh);
    let one = code.bodies[&(0, 1)].lines.as_ptr();
    let kept = std::mem::replace(
        &mut code.bodies.get_mut(&(0, 1)).unwrap().lines,
        vec![Line::from("kept")],
    );
    assert!(rows(&code, 40).contains(&"kept".to_string()));
    assert!(!rows(&code, 41).contains(&"kept".to_string()));
    code.bodies.get_mut(&(0, 1)).unwrap().lines = kept;

    // Unfolding another file, or the minute changing, lays out that file alone, and folding
    // one forgets its rows.
    code.act(Control::File(2), &[]);
    code.act(Control::File(0), &[]);
    code.prepare(40, &theme());
    assert_eq!(code.bodies[&(0, 1)].lines.as_ptr(), one);
    let mut kept: Vec<_> = code.bodies.keys().copied().collect();
    kept.sort();
    assert_eq!(kept, [(0, 1), (0, 2)]);
    code.prepare(41, &theme());
    assert_eq!(code.bodies[&(0, 1)].width, 41);

    // A later slice keeps the rows of the files before it, unless it says the host withheld
    // one of them.
    code.act(Control::File(0), &[]);
    code.prepare(41, &theme());
    assert_eq!(rows_of(&code.bodies[&(0, 0)].lines), ["No lines changed."]);
    let two = code.bodies[&(0, 2)].lines.as_ptr();
    code.near_end();
    assert_eq!(code.ask(2), Some(Some("2".to_string())));
    let mut diff = slice(later, None);
    diff.omitted = vec![pull_request::Omitted {
        path: "big.json".into(),
        additions: 5,
        deletions: 0,
    }];
    assert_eq!(code.take(2, Ok(diff), 0), Some(false));
    code.prepare(41, &theme());
    assert_eq!(code.bodies[&(0, 2)].lines.as_ptr(), two);
    assert_eq!(
        rows_of(&code.bodies[&(0, 0)].lines),
        ["The host withheld this file's changes."]
    );

    // A read again of the first slice lays out every file again.
    code.again();
    assert_eq!(code.ask(3), Some(None));
    assert_eq!(code.take(3, Ok(slice(first, None)), 5), Some(true));
    assert!(code.bodies.is_empty());
}

#[test]
fn what_the_host_withheld_is_noted_and_counted() {
    // GitHub's file list sends a file too large to show as its header alone, with its counts
    // in `omittedFileStats` (gitHubPullRequestJson.ts:3229-3258).
    let patch = "diff --git a/big.json b/big.json
--- a/big.json
+++ b/big.json
diff --git a/small b/small
--- a/small
+++ b/small
@@ -1 +1 @@
-a
+b
";
    let mut diff = slice(patch, None);
    diff.truncated = true;
    diff.omitted = vec![
        pull_request::Omitted {
            path: "big.json".into(),
            additions: 12_000,
            deletions: 3,
        },
        // Counts the hunks already give stay theirs.
        pull_request::Omitted {
            path: "small".into(),
            additions: 9,
            deletions: 9,
        },
    ];
    let mut code = read(diff, 0);
    code.act(Control::File(0), &[]);
    assert_eq!(
        rows(&code, 60),
        [
            "2 files",
            "The host withheld part of this diff, such as a binary file",
            "or a change too large to show.",
            "",
            "⌄ M big.json +12,000 -3",
            "The host withheld this file's changes.",
            "",
            "› M small +1 -1",
        ]
    );
}

#[test]
fn an_empty_or_unreadable_diff_says_so() {
    let code = read(slice("", None), 0);
    assert_eq!(
        rows(&code, 60),
        ["0 files", "", "This pull request has no file changes."]
    );
    // An empty slice with more after it waits for them.
    let code = read(slice("", Some("2")), 0);
    assert_eq!(rows(&code, 60), ["0 files+", "", "Loading more files…"]);
    let code = read(slice("not a diff\n", None), 0);
    assert_eq!(
        rows(&code, 60),
        [
            "0 files",
            "The host withheld part of this diff, such as a binary file",
            "or a change too large to show.",
            "",
            "t3term couldn't split this patch into files.",
        ]
    );

    // A file whose hunk header isn't one, or whose hunk ends short, says so under its lines.
    let text =
        "diff --git a/a b/a\n@@ -x +1 @@\n+lost\ndiff --git a/b b/b\n@@ -1,2 +1,2 @@\n kept\n";
    let mut code = read(slice(text, None), 0);
    code.act(Control::File(0), &[]);
    code.act(Control::File(1), &[]);
    let note = "t3term couldn't read some lines of this file's patch.";
    assert_eq!(
        rows(&code, 60),
        ["2 files", "", "⌄ M a", note, "", "⌄ M b", "1   kept", note]
    );

    // A file past the bound on lines has no hunk, and says why rather than that nothing changed.
    let mut text = format!(
        "diff --git a/big b/big\n@@ -0,0 +1,{0} @@\n",
        patch::MOST_LINES
    );
    text.push_str(&"+x\n".repeat(patch::MOST_LINES));
    text.push_str("diff --git a/next b/next\n@@ -1 +1 @@\n-a\n+b\n");
    let mut code = read(slice(&text, None), 0);
    code.act(Control::File(1), &[]);
    let rows = rows(&code, 100);
    assert_eq!(
        rows[rows.len() - 2..],
        [
            "⌄ M next +1 -1",
            "t3term reads at most 200,000 lines of a slice, so the rest of this file doesn't show.",
        ]
    );
}

#[test]
fn a_path_a_slice_has_twice_shows_once_as_its_last_part() {
    // A file whose type changed is its removal and then its addition.
    let patch = "diff --git a/link b/link
deleted file mode 120000
--- a/link
+++ /dev/null
@@ -1 +0,0 @@
-target
diff --git a/link b/link
new file mode 100644
--- /dev/null
+++ b/link
@@ -0,0 +1 @@
+text
diff --git a/other b/other
--- a/other
+++ b/other
@@ -1 +1 @@
-a
+b
";
    let code = read(slice(patch, None), 0);
    assert_eq!(
        rows(&code, 40),
        ["2 files", "", "› A link +1 -0", "", "› M other +1 -1"]
    );
    assert!(code.has(Control::File(1), &[]));
    assert!(!code.has(Control::File(2), &[]));
}

#[test]
fn lines_are_cleaned_their_tabs_set_and_long_ones_cut() {
    assert_eq!(shown("\tx"), "    x");
    assert_eq!(shown("ab\tc"), "ab  c");
    assert_eq!(shown("界\tc"), "界  c");
    assert_eq!(shown("a\u{1b}[2Jb\rc\u{7}"), "a[2Jbc");
    let long = "x".repeat(LINE + 5);
    let cut = shown(&long);
    assert_eq!(cut.chars().count(), LINE + 1);
    assert!(cut.ends_with("x…"));
    assert_eq!(shown(&"x".repeat(LINE)), "x".repeat(LINE));

    // Breaking keeps every space.
    assert_eq!(broken("  a  b", 3), ["  a", "  b"]);
    assert_eq!(broken("", 3), [""]);
    assert_eq!(broken("界界", 3), ["界", "界"]);
}

#[test]
fn every_row_fits_its_width_and_hostile_text_is_cleaned() {
    let patch = "diff --git a/src/\u{1b}]8;;evil\u{7}very/long/path/that/goes/on/name.rs b/src/\u{1b}]8;;evil\u{7}very/long/path/that/goes/on/name.rs
--- a/src/\u{1b}]8;;evil\u{7}very/long/path/that/goes/on/name.rs
+++ b/src/\u{1b}]8;;evil\u{7}very/long/path/that/goes/on/name.rs
@@ -1,2 +1,2 @@ fn \u{1b}[31mred
 \tlet x = \"界界界界界界界界界界界界界界界界\";\u{1b}[2J
-old line that is rather long and has to wrap at narrow widths
+new line that is rather long and has to wrap at narrow widths
";
    let mut code = read(slice(patch, Some("2")), 0);
    code.act(Control::File(0), &[]);
    for width in 1..=80 {
        let (lines, targets) = laid(&code, width);
        for line in &lines {
            let text = line.to_string();
            assert!(text.width() <= width, "{width}: {text:?}");
            assert!(
                !text.contains(|c: char| c.is_control()),
                "{width}: {text:?}"
            );
        }
        assert!(targets.iter().all(|(rows, _)| rows.end <= lines.len()));
    }
    // Every row of a header leads to the file.
    let (lines, targets) = laid(&code, 12);
    let (header, _) = &targets[0];
    assert!(header.len() > 1, "{:?}", rows_of(&lines));
    assert_eq!(rows_of(&lines)[header.start], "⌄ M src/]8;;");
    // A line's rows after its first leave its number and mark blank.
    let wide = rows(&code, 40);
    let at = wide.iter().position(|row| row.starts_with("2 -")).unwrap();
    assert_eq!(
        wide[at..at + 2],
        [
            "2 - old line that is rather long and has",
            "     to wrap at narrow widths"
        ]
    );
    // A pane too narrow for the numbers leaves them out and keeps the marks.
    assert!(rows(&code, 12).contains(&"2 - old line".to_string()));
    assert!(rows(&code, 11).contains(&"-old line t".to_string()));
}

/// A review thread `id` on `path` at `line` on `side`, with a remark by mona for each of
/// `bodies`.
fn thread(
    id: &str,
    path: &str,
    line: Option<u64>,
    side: &str,
    resolved: bool,
    bodies: &[&str],
) -> Value {
    let comments: Vec<Value> = bodies
        .iter()
        .enumerate()
        .map(|(at, body)| {
            json!({"id": format!("{id}-{at}"), "author": {"login": "mona", "name": null},
                "body": body, "createdAt": "2026-10-15T10:00:00Z", "url": null})
        })
        .collect();
    json!({"id": id, "path": path, "line": line, "side": side, "isResolved": resolved,
        "isOutdated": false, "comments": comments})
}

/// A conversation with `threads` and no other remarks.
fn conversation(threads: Vec<Value>) -> Conversation {
    let activity = json!({"comments": [], "commentCount": 0, "commentsTruncated": false,
        "reviewThreads": threads, "commits": []});
    Conversation::of(&pull_request::Activity::read(&activity).unwrap())
}

/// The tab's rows and targets at `width` with `conversation`'s threads, their remarks laid out
/// as the panel lays them out first.
fn with_threads(
    code: &Code,
    conversation: &mut Conversation,
    shown: &Shown,
    width: usize,
) -> (Vec<String>, Targets) {
    let showing = code.threads_shown(conversation.threads(), shown);
    conversation.lay_out_threads(&showing, width, &theme());
    let (lines, targets) = code.lay_out(conversation.threads(), &[], shown, width, NOW, &theme());
    (rows_of(&lines), targets)
}

#[test]
fn review_threads_show_under_their_lines() {
    let mut conversation = conversation(vec![
        thread("new", "src/lib.rs", Some(2), "right", false, &["New name."]),
        thread("old", "src/lib.rs", Some(2), "left", false, &["Old name."]),
        thread(
            "kept-right",
            "src/lib.rs",
            Some(3),
            "right",
            false,
            &["Right."],
        ),
        thread(
            "kept-left",
            "src/lib.rs",
            Some(3),
            "left",
            false,
            &["Left."],
        ),
        thread(
            "between",
            "src/lib.rs",
            Some(5),
            "right",
            false,
            &["Between."],
        ),
        thread(
            "file",
            "src/lib.rs",
            None,
            "right",
            false,
            &["On the file."],
        ),
        thread("gone", "gone.rs", Some(1), "right", false, &["Gone."]),
        thread("done", "src/lib.rs", Some(11), "right", true, &["Done."]),
    ]);
    let mut code = read(slice(PATCH, None), 0);
    let lib = (0..code.files.len())
        .find(|&at| code.file(at).unwrap().1.path() == "src/lib.rs")
        .unwrap();
    code.act(Control::File(lib), &[]);
    let mut shown = Shown::default();
    let (rows, targets) = with_threads(&code, &mut conversation, &shown, 40);
    let at = rows
        .iter()
        .position(|row| row == "⌄ M src/lib.rs +2 -1")
        .unwrap();
    // A thread on a removed line shows under it, one on a line before and after the change
    // under it on either side, its side before the change first, and a resolved one closed.
    let open = |text: &str| {
        [
            "│ ⌄ Open · 1 comment".to_string(),
            "│ mona                            5m ago".to_string(),
            format!("│   {text}"),
        ]
    };
    let mut expected = vec![
        " 1   use std::fmt;".to_string(),
        " 2 - fn old() {}".to_string(),
    ];
    expected.extend(open("Old name."));
    expected.push(" 2 + fn new() {}".into());
    expected.extend(open("New name."));
    expected.push(" 3   fn keep() {}".into());
    expected.extend(open("Left."));
    expected.push(String::new());
    expected.extend(open("Right."));
    expected.extend(
        [
            "⋯ 6 unmodified lines",
            "10   fn a() {}",
            "11 + fn b() {}",
            "│ › Resolved · 1 comment",
            "12   fn c() {}",
        ]
        .map(String::from),
    );
    assert_eq!(rows[at + 1..at + 1 + expected.len()], expected);
    let threads: Vec<_> = targets
        .iter()
        .filter(|(_, control)| matches!(control, Control::Thread(_)))
        .map(|(rows, control)| (rows.start - at, *control))
        .collect();
    assert_eq!(
        threads,
        [
            (3, Control::Thread(1)),
            (7, Control::Thread(0)),
            (11, Control::Thread(3)),
            (15, Control::Thread(2)),
            (21, Control::Thread(7)),
        ]
    );
    assert_eq!(
        code.threads_shown(conversation.threads(), &shown),
        [0, 1, 2, 3]
    );

    // The rest show above the files in a list that starts closed: one on a line no hunk has,
    // one on the file, and one on a file not in the diff.
    assert_eq!(
        rows[1..4],
        ["", "› 3 conversations not on the current", "diff"]
    );
    assert_eq!(targets[0], (2..4, Control::OffDiff));
    assert!(code.shows(Control::OffDiff, conversation.threads(), &shown));
    assert!(!code.shows(Control::Thread(4), conversation.threads(), &shown));
    shown.act(Control::OffDiff, &conversation);
    assert!(code.shows(Control::Thread(4), conversation.threads(), &shown));
    let (rows, _) = with_threads(&code, &mut conversation, &shown, 40);
    let mut expected = vec![
        "⌄ 3 conversations not on the current".to_string(),
        "diff".into(),
        String::new(),
        "src/lib.rs".into(),
        "Line 5".into(),
    ];
    expected.extend(open("Between."));
    expected.push(String::new());
    expected.extend(open("On the file."));
    expected.extend([String::new(), "gone.rs".into(), "Line 1".into()]);
    expected.extend(open("Gone."));
    expected.push(String::new());
    assert_eq!(rows[2..2 + expected.len()], expected);
    assert_eq!(rows[2 + expected.len()], "› A docs/new.md +2 -0");

    // Opening the resolved thread shows its remark, and closing an open one hides its own.
    shown.act(Control::Thread(7), &conversation);
    shown.act(Control::Thread(0), &conversation);
    assert_eq!(
        code.threads_shown(conversation.threads(), &shown),
        [1, 2, 3, 4, 5, 6, 7]
    );
    let (rows, _) = with_threads(&code, &mut conversation, &shown, 40);
    let at = rows.iter().position(|row| row == "11 + fn b() {}").unwrap();
    assert_eq!(rows[at + 1], "│ ⌄ Resolved · 1 comment");
    assert_eq!(rows[at + 3], "│   Done.");
    let at = rows
        .iter()
        .position(|row| row == " 2 + fn new() {}")
        .unwrap();
    assert_eq!(
        rows[at + 1..at + 3],
        ["│ › Open · 1 comment", " 3   fn keep() {}"]
    );

    // Folding the file takes its threads away, with their controls.
    code.act(Control::File(lib), &[]);
    assert!(!code.shows(Control::Thread(0), conversation.threads(), &shown));
    assert_eq!(
        code.threads_shown(conversation.threads(), &shown),
        [4, 5, 6]
    );
    let (rows, _) = with_threads(&code, &mut conversation, &shown, 40);
    assert!(!rows.iter().any(|row| row.contains("Done.")));
}

#[test]
fn a_thread_shows_after_a_line_s_end_note_and_off_a_hunk_cut_short() {
    let patch = "diff --git a/a b/a
--- a/a
+++ b/a
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
diff --git a/b b/b
--- a/b
+++ b/b
@@ -1,3 +1,3 @@
 one
-two
+deux
";
    let mut conversation = conversation(vec![
        thread("old", "a", Some(1), "left", false, &["Old."]),
        thread("new", "a", Some(1), "right", false, &["New."]),
        thread("deux", "b", Some(2), "right", false, &["Deux."]),
        // The hunk's header counts a third line, which never came.
        thread("short", "b", Some(3), "right", false, &["Short."]),
    ]);
    let mut code = read(slice(patch, None), 0);
    code.act(Control::File(0), &[]);
    code.act(Control::File(1), &[]);
    let (rows, _) = with_threads(&code, &mut conversation, &Shown::default(), 40);
    let card = |text: &str| {
        [
            "│ ⌄ Open · 1 comment".to_string(),
            "│ mona                            5m ago".to_string(),
            format!("│   {text}"),
        ]
    };
    let mut expected = vec![
        "2 files".to_string(),
        String::new(),
        "› 1 conversation not on the current diff".into(),
        String::new(),
        "⌄ M a +1 -1".into(),
        "1 - old".into(),
        "    No newline at end of file".into(),
    ];
    expected.extend(card("Old."));
    expected.extend(["1 + new".into(), "    No newline at end of file".into()]);
    expected.extend(card("New."));
    assert_eq!(rows[..expected.len()], expected[..], "{rows:#?}");
    let at = rows.iter().position(|row| row == "2 + deux").unwrap();
    assert_eq!(rows[at + 3], "│   Deux.");
    assert_eq!(rows[at + 4], "t3term couldn't read some lines of this");
    assert!(code.shows(Control::OffDiff, conversation.threads(), &Shown::default()));
    let on_lines = self::conversation(vec![thread("old", "a", Some(1), "left", false, &["Old."])]);
    assert!(!code.shows(Control::OffDiff, on_lines.threads(), &Shown::default()));

    // Before a slice comes, no thread shows, nor does the list.
    let mut shown = Shown::default();
    shown.act(Control::OffDiff, &conversation);
    let none = Code::new();
    assert!(!none.shows(Control::OffDiff, conversation.threads(), &shown));
    assert!(!none.shows(Control::Thread(3), conversation.threads(), &shown));
    assert!(
        none.threads_shown(conversation.threads(), &shown)
            .is_empty()
    );
}

#[test]
fn what_the_reader_opened_of_threads_goes_with_them() {
    let with = conversation(vec![thread("t", "a", Some(1), "right", false, &["A."])]);
    let without = conversation(Vec::new());
    let mut shown = Shown::default();
    shown.act(Control::Thread(0), &with);
    shown.prune(&with);
    assert_ne!(shown, Shown::default());
    shown.prune(&without);
    assert_eq!(shown, Shown::default());
}

/// A diff of one file, `a`, whose one line changed.
const ONE: &str = "diff --git a/a b/a
--- a/a
+++ b/a
@@ -1 +1 @@
-old
+new
";

/// A page of a thread's remarks with these `ids`, each saying `Remark {id}.`, and `next` where
/// the page after starts.
fn page(ids: &[&str], next: Option<&str>) -> pull_request::CommentPage {
    let comments: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({"id": id, "author": {"login": "mona", "name": null},
                "body": format!("Remark {id}."), "createdAt": "2026-10-15T10:00:00Z", "url": null})
        })
        .collect();
    pull_request::CommentPage::read(&json!({"comments": comments, "nextCursor": next})).unwrap()
}

/// The rows that read thread `index`'s next page, where they show.
fn more_rows(targets: &Targets, index: usize) -> Option<std::ops::Range<usize>> {
    targets
        .iter()
        .find(|(_, control)| *control == Control::More(index))
        .map(|(rows, _)| rows.clone())
}

#[test]
fn a_thread_reads_its_remarks_a_page_at_a_time() {
    let mut first = thread("t", "a", Some(1), "right", false, &["First."]);
    first["commentCount"] = json!(5);
    first["nextCommentsCursor"] = json!("p2");
    let mut conversation = conversation(vec![first]);
    let mut code = read(slice(ONE, None), 0);
    code.act(Control::File(0), &[]);
    let mut shown = Shown::default();
    let (rows, targets) = with_threads(&code, &mut conversation, &shown, 40);
    let at = rows
        .iter()
        .position(|row| row == "│ ⌄ Open · 5 comments")
        .unwrap();
    assert_eq!(rows[at + 2..at + 4], ["│   First.", "│ Load more comments"]);
    let trailing = ["⋯ More unchanged context may be", "available"];
    assert_eq!(rows[at + 4..], trailing, "{rows:#?}");
    assert_eq!(more_rows(&targets, 0), Some(at + 3..at + 4));
    assert!(code.shows(Control::More(0), conversation.threads(), &shown));
    assert_eq!(conversation.take_wanted(1), None);

    // Asked for, it says the page is coming and offers no button until it comes. One read at
    // a time goes for each thread.
    assert!(conversation.want_more(0));
    assert!(!conversation.want_more(0));
    assert!(!conversation.want_more(1));
    let (rows, targets) = with_threads(&code, &mut conversation, &shown, 40);
    assert_eq!(rows[at + 3], "│ Loading more comments…");
    assert_eq!(more_rows(&targets, 0), None);
    assert!(!code.shows(Control::More(0), conversation.threads(), &shown));
    assert_eq!(conversation.take_wanted(7), Some(("t".into(), "p2".into())));
    assert_eq!(conversation.take_wanted(8), None);
    assert!(!conversation.want_more(0));
    let (rows, _) = with_threads(&code, &mut conversation, &shown, 40);
    assert_eq!(rows[at + 3], "│ Loading more comments…");

    // Another read's answer isn't its. A failure says why, cleaned, above the button.
    assert!(!conversation.take_page(8, Ok(&page(&["x"], None))));
    assert!(conversation.take_page(7, Err("Slow \u{1b}[2J host.")));
    assert!(!conversation.take_page(7, Ok(&page(&["x"], None))));
    let (rows, targets) = with_threads(&code, &mut conversation, &shown, 40);
    assert_eq!(
        rows[at + 3..at + 6],
        [
            "│ t3term couldn't read more comments.",
            "│ Slow [2J host.",
            "│ Load more comments"
        ]
    );
    assert_eq!(more_rows(&targets, 0), Some(at + 5..at + 6));

    // The page's remarks follow the thread's, each once, and its cursor reads the next.
    assert!(conversation.want_more(0));
    assert_eq!(conversation.take_wanted(9), Some(("t".into(), "p2".into())));
    let next = page(&["t-0", "x", "x", "y"], Some("p3"));
    assert!(conversation.take_page(9, Ok(&next)));
    let (rows, targets) = with_threads(&code, &mut conversation, &shown, 40);
    assert_eq!(
        rows[at + 2..at + 10],
        [
            "│   First.",
            "│",
            "│ mona                            5m ago",
            "│   Remark x.",
            "│",
            "│ mona                            5m ago",
            "│   Remark y.",
            "│ Load more comments",
        ]
    );
    assert!(!rows.iter().any(|row| row.contains("Remark t-0.")));
    assert_eq!(more_rows(&targets, 0), Some(at + 9..at + 10));

    // Closed, it shows none of them, nor the button.
    shown.act(Control::Thread(0), &conversation);
    let (rows, targets) = with_threads(&code, &mut conversation, &shown, 40);
    assert_eq!(rows[at], "│ › Open · 5 comments");
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("Remark") || row.contains("Load"))
    );
    assert_eq!(more_rows(&targets, 0), None);
    assert!(!code.shows(Control::More(0), conversation.threads(), &shown));
    shown.act(Control::Thread(0), &conversation);
    // Nor under a folded file.
    assert!(code.shows(Control::More(0), conversation.threads(), &shown));
    code.act(Control::File(0), &[]);
    assert!(!code.shows(Control::More(0), conversation.threads(), &shown));
    code.act(Control::File(0), &[]);
    assert!(conversation.want_more(0));
    assert_eq!(
        conversation.take_wanted(10),
        Some(("t".into(), "p3".into()))
    );

    // The last page takes the button away, and the host counts one remark that never came.
    assert!(conversation.take_page(10, Ok(&page(&["z"], None))));
    assert!(!conversation.want_more(0));
    let (rows, targets) = with_threads(&code, &mut conversation, &shown, 40);
    assert_eq!(
        rows[at + 11..at + 13],
        ["│   Remark z.", "│ 1 more comment on the host."]
    );
    assert_eq!(more_rows(&targets, 0), None);
    assert!(!code.shows(Control::More(0), conversation.threads(), &shown));
}

#[test]
fn a_thread_reads_no_page_past_what_t3term_keeps() {
    let bodies = |count: usize| vec!["Kept."; count];
    let ids = |count: usize| (0..count).map(|at| format!("p-{at}")).collect::<Vec<_>>();
    let mut nearly = thread("nearly", "a", Some(1), "right", false, &bodies(999));
    nearly["commentCount"] = json!(1_005);
    nearly["nextCommentsCursor"] = json!("p2");
    let mut full = thread("full", "a", Some(1), "left", false, &bodies(1_000));
    full["nextCommentsCursor"] = json!("p2");
    let mut cut = thread("cut", "a", Some(1), "left", false, &["Cut."]);
    cut["nextCommentsCursor"] = json!("p2");
    let mut conversation = conversation(vec![nearly, full, cut]);
    let mut code = read(slice(ONE, None), 0);
    code.act(Control::File(0), &[]);
    let shown = Shown::default();

    // A thread that came with as many remarks as t3term keeps reads no page after them.
    assert!(!conversation.want_more(1));
    let (_, targets) = with_threads(&code, &mut conversation, &shown, 40);
    assert!(more_rows(&targets, 0).is_some() && more_rows(&targets, 2).is_some());
    assert_eq!(more_rows(&targets, 1), None);

    // A page keeps what fits and reads no page after it.
    assert!(conversation.want_more(0));
    assert_eq!(
        conversation.take_wanted(1),
        Some(("nearly".into(), "p2".into()))
    );
    let ids_of_three = ids(3);
    let three: Vec<&str> = ids_of_three.iter().map(String::as_str).collect();
    assert!(conversation.take_page(1, Ok(&page(&three, Some("p3")))));
    assert!(!conversation.want_more(0));
    let (rows, targets) = with_threads(&code, &mut conversation, &shown, 40);
    assert_eq!(more_rows(&targets, 0), None);
    assert!(rows.contains(&"│   Remark p-0.".to_string()));
    assert!(!rows.contains(&"│   Remark p-1.".to_string()));
    assert!(rows.contains(&"│ 5 more comments on the host.".to_string()));

    // Nor does a page longer than t3term keeps, whose last remarks the next would skip.
    assert!(conversation.want_more(2));
    assert_eq!(
        conversation.take_wanted(2),
        Some(("cut".into(), "p2".into()))
    );
    let mut long = vec!["cut-0"; 1_000];
    long.push("cut-1");
    let long = page(&long, Some("p3"));
    assert!(long.cut);
    assert!(conversation.take_page(2, Ok(&long)));
    assert!(!conversation.want_more(2));
}

#[test]
fn pages_go_with_their_thread_when_the_conversation_is_read_again() {
    let threads = |bodies: &[&str], cursor: &str| {
        let mut paged = thread("t", "a", Some(1), "right", false, bodies);
        paged["commentCount"] = json!(9);
        paged["nextCommentsCursor"] = json!(cursor);
        let mut asked = thread("u", "a", Some(1), "left", false, &["Asked."]);
        asked["nextCommentsCursor"] = json!("q2");
        vec![paged, asked]
    };
    let mut old = conversation(threads(&["First."], "p2"));
    assert!(old.want_more(0));
    assert_eq!(old.take_wanted(1), Some(("t".into(), "p2".into())));
    assert!(old.take_page(1, Ok(&page(&["t-1", "t-2"], Some("p3")))));
    assert!(old.want_more(1));
    assert_eq!(old.take_wanted(2), Some(("u".into(), "q2".into())));

    // The read again sends the first remark of the page with the thread, and a cursor of its
    // own. The page's other remark stays, and the next page is the one after it.
    let mut new = conversation(threads(&["First.", "Second."], "fresh"));
    new.carry(&mut old);
    let mut code = read(slice(ONE, None), 0);
    code.act(Control::File(0), &[]);
    let shown = Shown::default();
    let (rows, _) = with_threads(&code, &mut new, &shown, 40);
    for body in ["│   First.", "│   Second.", "│   Remark t-2."] {
        assert_eq!(
            rows.iter().filter(|row| *row == body).count(),
            1,
            "{rows:#?}"
        );
    }
    assert!(!rows.contains(&"│   Remark t-1.".to_string()), "{rows:#?}");
    // A read after that keeps them too.
    let mut newer = conversation(threads(&["First.", "Second."], "fresher"));
    newer.carry(&mut new);
    let (rows, _) = with_threads(&code, &mut newer, &shown, 40);
    assert!(rows.contains(&"│   Remark t-2.".to_string()), "{rows:#?}");
    let mut new = newer;
    assert!(new.want_more(0));
    assert_eq!(new.take_wanted(3), Some(("t".into(), "p3".into())));

    // The read under way still answers to the thread, which had no page, and its cursor is
    // the new one's until then.
    assert!(!new.want_more(1));
    assert!(new.take_page(2, Ok(&page(&["u-9"], None))));
    let (rows, _) = with_threads(&code, &mut new, &shown, 40);
    assert!(rows.contains(&"│   Remark u-9.".to_string()), "{rows:#?}");

    // A thread read again with as many remarks as t3term keeps takes no page's remarks past
    // them, and reads no page after.
    let mut old = conversation(threads(&["First."], "p2"));
    assert!(old.want_more(0));
    assert_eq!(old.take_wanted(1), Some(("t".into(), "p2".into())));
    assert!(old.take_page(1, Ok(&page(&["t-1", "x"], Some("p3")))));
    assert!(old.want_more(0) && old.want_more(1));
    let mut new = conversation(threads(&vec!["Kept."; 1_000], "fresh"));
    new.carry(&mut old);
    assert!(!new.want_more(0));
    let (rows, _) = with_threads(&code, &mut new, &shown, 40);
    assert!(!rows.contains(&"│   Remark x.".to_string()));
    // Its page the reader wanted goes unread, and the next thread's goes.
    assert_eq!(new.take_wanted(4), Some(("u".into(), "q2".into())));
    assert_eq!(new.take_wanted(5), None);
    let (rows, _) = with_threads(&code, &mut new, &shown, 40);
    let loading = rows.iter().filter(|row| row.contains("Loading more"));
    assert_eq!(loading.count(), 1, "{rows:#?}");

    // A thread no page was read for keeps the cursor the read again sent.
    let mut old = conversation(threads(&["First."], "p2"));
    let mut new = conversation(threads(&["First."], "fresh"));
    new.carry(&mut old);
    assert!(new.want_more(0));
    assert_eq!(new.take_wanted(1), Some(("t".into(), "fresh".into())));

    // A thread whose pages ran out reads on from the read again's own cursor once the host
    // counts more remarks than it holds, as after a reply, and holds each remark once.
    let counted = |count: u64| {
        let mut paged = thread("t", "a", Some(1), "right", false, &["First."]);
        paged["commentCount"] = json!(count);
        paged["nextCommentsCursor"] = json!("p2");
        vec![paged]
    };
    let mut old = conversation(counted(3));
    assert!(old.want_more(0));
    assert_eq!(old.take_wanted(1), Some(("t".into(), "p2".into())));
    assert!(old.take_page(1, Ok(&page(&["t-1", "t-2"], None))));
    assert!(!old.want_more(0));
    let mut same = conversation(counted(3));
    same.carry(&mut old);
    assert!(!same.want_more(0));
    let mut replied = conversation(counted(4));
    replied.carry(&mut same);
    assert!(replied.want_more(0));
    assert_eq!(replied.take_wanted(2), Some(("t".into(), "p2".into())));
    assert!(replied.take_page(2, Ok(&page(&["t-1", "t-2", "t-3"], None))));
    assert!(!replied.want_more(0));
    let (rows, _) = with_threads(&code, &mut replied, &shown, 40);
    for body in ["│   Remark t-1.", "│   Remark t-2.", "│   Remark t-3."] {
        assert_eq!(
            rows.iter().filter(|row| *row == body).count(),
            1,
            "{rows:#?}"
        );
    }
}

#[test]
fn a_new_theme_restyles_the_threads() {
    let mut conversation = conversation(vec![thread(
        "on",
        "src/lib.rs",
        Some(2),
        "right",
        false,
        &["Body text."],
    )]);
    let mut code = read(slice(PATCH, None), 0);
    let lib = (0..code.files.len())
        .find(|&at| code.file(at).unwrap().1.path() == "src/lib.rs")
        .unwrap();
    code.act(Control::File(lib), &[]);
    let shown = Shown::default();
    // The color of the remark's text, laid out at one width as the panel lays it out.
    let body = |conversation: &mut Conversation, theme: &Theme| {
        let showing = code.threads_shown(conversation.threads(), &shown);
        conversation.lay_out_threads(&showing, 40, theme);
        let (lines, _) = code.lay_out(conversation.threads(), &[], &shown, 40, NOW, theme);
        lines.iter().find_map(|line| {
            let span = line
                .spans
                .iter()
                .find(|span| span.content.contains("Body"))?;
            line.style.patch(span.style).fg
        })
    };
    let dark = theme();
    let light = Theme::light(Depth::TrueColor);
    assert_ne!(dark.fg, light.fg);
    assert_eq!(body(&mut conversation, &dark), Some(dark.fg));
    // Another theme at the same width lays the text out again in its colors.
    conversation.restyle();
    assert_eq!(body(&mut conversation, &light), Some(light.fg));
}

#[test]
fn threads_fit_every_width_and_hostile_text_is_cleaned() {
    let path = "src/\u{1b}]8;;evil\u{7}name.rs";
    let patch = format!(
        "diff --git a/{path} b/{path}
--- a/{path}
+++ b/{path}
@@ -1 +1 @@
-old line
+new line
"
    );
    let hostile = "Look \u{1b}[2J here, at a remark long enough to wrap at every width.";
    let mut threads = vec![
        thread("on", path, Some(1), "right", false, &[hostile, "Second."]),
        thread(
            "off",
            &format!("{path}x"),
            Some(7),
            "left",
            false,
            &[hostile],
        ),
    ];
    threads[0]["comments"][0]["author"]["login"] = json!("ev\u{1b}]0;il");
    threads[0]["isOutdated"] = json!(true);
    threads[0]["commentCount"] = json!(12_000);
    threads[1]["nextCommentsCursor"] = json!("p2");
    let mut conversation = conversation(threads);
    assert!(conversation.want_more(1));
    assert_eq!(
        conversation.take_wanted(1),
        Some(("off".into(), "p2".into()))
    );
    assert!(conversation.take_page(1, Err(hostile)));
    let mut code = read(slice(&patch, None), 0);
    code.act(Control::File(0), &[]);
    let mut shown = Shown::default();
    shown.act(Control::OffDiff, &conversation);
    for width in 1..=80 {
        let showing = code.threads_shown(conversation.threads(), &shown);
        assert_eq!(showing, [0, 1]);
        conversation.lay_out_threads(&showing, width, &theme());
        let (lines, targets) =
            code.lay_out(conversation.threads(), &[], &shown, width, NOW, &theme());
        for line in &lines {
            let text = line.to_string();
            // The Markdown renderer keeps 8 columns for text beside the bar's 2, and the
            // details' page cuts what is wider than it shows.
            assert!(text.width() <= width.max(10), "{width}: {text:?}");
            assert!(
                !text.contains(|c: char| c.is_control()),
                "{width}: {text:?}"
            );
        }
        assert!(targets.iter().all(|(rows, _)| rows.end <= lines.len()));
        let header = |index| {
            let (rows, _) = targets
                .iter()
                .find(|(_, control)| *control == Control::Thread(index))
                .unwrap();
            rows.len()
        };
        assert!(header(0) >= 1 && header(1) >= 1, "{width}");
        assert!(more_rows(&targets, 1).is_some_and(|rows| !rows.is_empty()));
    }
    // A first row that wraps opens and closes the thread on every row it takes.
    let (rows, targets) = with_threads(&code, &mut conversation, &shown, 12);
    let (header, _) = targets
        .iter()
        .find(|(_, control)| *control == Control::Thread(0))
        .unwrap();
    assert!(header.len() > 1, "{rows:#?}");
    assert_eq!(rows[header.start], "│ ⌄ Open");
    assert!(rows[header.end].starts_with("│ ev]0;il"), "{rows:#?}");
    // A pane too narrow for the bar leaves it out.
    let (rows, _) = with_threads(&code, &mut conversation, &shown, 2);
    assert!(rows.iter().all(|row| !row.starts_with('│')), "{rows:#?}");
    let (rows, _) = with_threads(&code, &mut conversation, &shown, 50);
    assert!(rows.contains(&"│ ⌄ Open · 12,000 comments · outdated".to_string()));
    assert!(rows.contains(&"│ 11,998 more comments on the host.".to_string()));
    let at = rows
        .iter()
        .position(|row| row == "│ t3term couldn't read more comments.")
        .unwrap();
    assert_eq!(
        rows[at + 1..at + 4],
        [
            "│ Look [2J here, at a remark long enough to wrap",
            "│ at every width.",
            "│ Load more comments"
        ]
    );
    assert!(
        rows.contains(&"src/]8;;evilname.rsx".to_string()),
        "{rows:#?}"
    );
    // A blank row keeps a thread's remarks apart.
    let at = rows.iter().position(|row| row == "│   Second.").unwrap();
    assert_eq!(rows[at - 2], "│");
    assert!(rows[at - 1].starts_with("│ mona"));
}

/// A conversation with `threads` and a commit for each of `commits`, as its id, headline and
/// time, in the order T3 sent them, and no remarks.
fn committed(threads: Vec<Value>, commits: &[(&str, &str, &str)]) -> Conversation {
    let commits: Vec<Value> = commits
        .iter()
        .map(|(oid, headline, at)| {
            json!({"oid": oid, "messageHeadline": headline, "committedDate": at})
        })
        .collect();
    let activity = json!({"comments": [], "commentCount": 0, "commentsTruncated": false,
        "reviewThreads": threads, "commits": commits});
    Conversation::of(&pull_request::Activity::read(&activity).unwrap())
}

/// The tab's rows and targets at `width` with `conversation`'s threads and commits.
fn scoped(
    code: &Code,
    conversation: &Conversation,
    shown: &Shown,
    width: usize,
) -> (Vec<String>, Targets) {
    let commits = conversation.commits();
    let threads = conversation.threads();
    let (lines, targets) = code.lay_out(threads, &commits, shown, width, NOW, &theme());
    (rows_of(&lines), targets)
}

/// An entry of the list of commits on a row of `width`: `lead`, then `text`, then `short` at
/// the end.
fn entry_row(lead: &str, text: &str, short: &str, width: usize) -> String {
    let gap = width - lead.width() - text.width() - short.width();
    format!("{lead}{text}{}{short}", " ".repeat(gap))
}

#[test]
fn the_list_of_commits_picks_the_diff_the_tab_reads() {
    // In the order T3 sent them: one in another zone, two at the same instant and one with
    // no time t3term reads.
    let on_line = thread(
        "t1",
        "src/lib.rs",
        Some(2),
        "right",
        false,
        &["On the new line."],
    );
    let conversation = committed(
        vec![on_line],
        &[
            ("aaaaaaa1111", "First", "2026-10-15T08:00:00Z"),
            ("bbbbbbb2222", "Zone", "2026-10-15T11:30:00+02:00"),
            ("ccccccc3333", "Newest", "2026-10-15T10:00:00Z"),
            ("ddddddd4444", "", "not a date"),
            ("eeeeeee5555", "Same time", "2026-10-15T08:00:00Z"),
        ],
    );
    let commits = conversation.commits();
    let shown = Shown::default();
    let mut code = read(slice(PATCH, None), 0);
    let (rows, targets) = scoped(&code, &conversation, &shown, 40);
    assert_eq!(rows[..2], ["› All commits", "5 files"]);
    assert_eq!(targets[0], (0..1, Control::Commits));
    assert_eq!(code.action(Control::Commits, &commits), "choose commit");
    for control in [
        Control::AllCommits,
        Control::Commit(0),
        Control::MoreCommits,
    ] {
        assert!(!code.has(control, &commits), "{control:?}");
    }
    // Under the whole change, the thread is under its line.
    assert!(
        !rows.iter().any(|row| row.contains("conversation")),
        "{rows:#?}"
    );

    // Newest first by when each was committed, ties in T3's order, the unreadable time last.
    assert!(!code.act(Control::Commits, &commits));
    assert_eq!(code.action(Control::Commits, &commits), "hide commits");
    let (rows, targets) = scoped(&code, &conversation, &shown, 40);
    assert_eq!(
        rows[..9],
        [
            "⌄ All commits".to_string(),
            "  ✓ All commits".into(),
            entry_row("    ", "Newest", "ccccccc", 40),
            entry_row("    ", "Zone", "bbbbbbb", 40),
            entry_row("    ", "First", "aaaaaaa", 40),
            entry_row("    ", "Same time", "eeeeeee", 40),
            entry_row("    ", "Untitled commit", "ddddddd", 40),
            String::new(),
            "5 files".into(),
        ]
    );
    let entries = [Control::AllCommits]
        .into_iter()
        .chain((0..5).map(Control::Commit));
    let expected: Vec<_> = (1..7).map(|row| row..row + 1).zip(entries).collect();
    assert_eq!(targets[1..7], expected);
    assert!(code.has(Control::Commit(4), &commits));
    assert!(!code.has(Control::Commit(5), &commits));
    assert!(!code.has(Control::MoreCommits, &commits));
    assert_eq!(
        code.action(Control::AllCommits, &commits),
        "show all commits"
    );
    assert_eq!(
        code.action(Control::Commit(1), &commits),
        "show commit bbbbbbb"
    );
    assert_eq!(code.action(Control::Commit(9), &commits), "show commit");

    // Picking the diff shown closes the list and reads nothing.
    code.act(Control::File(4), &commits);
    assert!(!code.act(Control::AllCommits, &commits));
    assert!(!code.has(Control::AllCommits, &commits));
    assert_eq!(code.ask(2), None);

    // Another commit's diff reads at once from its first slice, and a read for the whole
    // change under way is dropped when it comes.
    code.again();
    assert_eq!(code.ask(2), Some(None));
    code.act(Control::Commits, &commits);
    assert!(code.act(Control::Commit(1), &commits));
    assert_eq!(code.commit(), Some("bbbbbbb2222"));
    assert!(!code.has(Control::Commit(1), &commits));
    assert_eq!(code.ask(3), Some(None));
    assert_eq!(code.take(2, Ok(slice(PATCH, None)), 10), None);
    let (rows, targets) = scoped(&code, &conversation, &shown, 40);
    assert_eq!(rows, ["› Zone · bbbbbbb", "Loading pull request diff…"]);
    assert_eq!(targets, [(0..1, Control::Commits)]);

    // Its files show folded, and the thread goes off its diff though its line is in it.
    assert_eq!(code.take(3, Ok(slice(PATCH, None)), 10), Some(true));
    let (rows, _) = scoped(&code, &conversation, &shown, 40);
    assert_eq!(
        rows[..4],
        [
            "› Zone · bbbbbbb",
            "5 files",
            "",
            "› 1 conversation not on the current diff"
        ]
    );
    assert!(
        rows.contains(&"› R from.rs → to.rs".to_string()),
        "{rows:#?}"
    );
    code.act(Control::File(3), &commits);
    assert!(
        code.threads_shown(conversation.threads(), &shown)
            .is_empty()
    );

    // A commit with no files says so.
    code.act(Control::Commits, &commits);
    assert!(code.act(Control::Commit(0), &commits));
    assert_eq!(code.ask(4), Some(None));
    assert_eq!(code.take(4, Ok(slice("", None)), 20), Some(true));
    let (rows, _) = scoped(&code, &conversation, &shown, 40);
    assert_eq!(
        rows,
        [
            "› Newest · ccccccc",
            "0 files",
            "",
            "This commit has no file changes."
        ]
    );

    // All commits reads the whole change again, and a failure shows under the row.
    code.act(Control::Commits, &commits);
    assert!(code.act(Control::AllCommits, &commits));
    assert_eq!(code.commit(), None);
    assert_eq!(code.ask(5), Some(None));
    let failure = Err("T3 couldn't read the diff.".to_string());
    assert_eq!(code.take(5, failure, 30), Some(true));
    let (rows, _) = scoped(&code, &conversation, &shown, 40);
    assert_eq!(rows, ["› All commits", "T3 couldn't read the diff."]);

    // A commit picked after that reads its own diff, and the failure goes with the diff it
    // was of.
    code.act(Control::Commits, &commits);
    assert!(code.act(Control::Commit(0), &commits));
    let (rows, _) = scoped(&code, &conversation, &shown, 40);
    assert_eq!(rows, ["› Newest · ccccccc", "Loading pull request diff…"]);
}

#[test]
fn the_list_shows_10_commits_at_a_time_and_a_rebase_takes_it_back_to_all() {
    let oids: Vec<String> = (0..23).map(|n| format!("{n:07}abc")).collect();
    let headlines: Vec<String> = (0..23).map(|n| format!("Commit {n}")).collect();
    let times: Vec<String> = (0..23)
        .map(|n| format!("2026-10-15T08:{n:02}:00Z"))
        .collect();
    let all: Vec<(&str, &str, &str)> = (0..23)
        .map(|n| (oids[n].as_str(), headlines[n].as_str(), times[n].as_str()))
        .collect();
    let conversation = committed(Vec::new(), &all);
    let commits = conversation.commits();
    let shown = Shown::default();
    let mut code = read(slice(ONE, None), 0);
    code.act(Control::Commits, &commits);
    let more = |code: &Code| {
        let (rows, targets) = scoped(code, &conversation, &shown, 40);
        let at = targets
            .iter()
            .find(|(_, control)| *control == Control::MoreCommits)
            .map(|(rows, _)| rows.start);
        (at.map(|at| rows[at].clone()), rows)
    };
    let (row, rows) = more(&code);
    assert_eq!(row.as_deref(), Some("    Show more (13 left)"));
    assert_eq!(rows[2], entry_row("    ", "Commit 22", "0000022", 40));
    assert_eq!(rows[11], entry_row("    ", "Commit 13", "0000013", 40));
    assert_eq!(rows[12..15], ["    Show more (13 left)", "", "1 file"]);
    assert!(code.has(Control::Commit(9), &commits));
    assert!(!code.has(Control::Commit(10), &commits));
    assert_eq!(
        code.action(Control::MoreCommits, &commits),
        "show more commits"
    );

    assert!(!code.act(Control::MoreCommits, &commits));
    assert_eq!(more(&code).0.as_deref(), Some("    Show more (3 left)"));
    code.act(Control::MoreCommits, &commits);
    let (row, rows) = more(&code);
    assert_eq!(row, None);
    assert_eq!(rows[24], entry_row("    ", "Commit 0", "0000000", 40));
    assert!(code.has(Control::Commit(22), &commits));
    assert!(!code.has(Control::MoreCommits, &commits));

    // Another commit lists 10 again when the list opens.
    assert!(code.act(Control::Commit(15), &commits));
    assert_eq!(code.commit(), Some("0000007abc"));
    code.act(Control::Commits, &commits);
    let (row, rows) = more(&code);
    assert_eq!(row.as_deref(), Some("    Show more (13 left)"));
    assert_eq!(rows[..2], ["⌄ Commit 7 · 0000007", "    All commits"]);

    // A read of the conversation that still lists the commit keeps it. One that doesn't, as
    // after a force-push, reads the whole change again, the list closed.
    assert!(!code.follow(&commits));
    assert_eq!(code.ask(2), Some(None));
    let kept = committed(Vec::new(), &all[..5]);
    assert!(code.follow(&kept.commits()));
    assert_eq!(code.commit(), None);
    assert!(!code.has(Control::AllCommits, &kept.commits()));
    assert_eq!(code.ask(3), Some(None));
    assert!(!code.follow(&kept.commits()));

    // A conversation with no commits has no row to pick one.
    let none = committed(Vec::new(), &[]);
    assert!(!code.has(Control::Commits, &none.commits()));
    assert_eq!(code.take(3, Ok(slice(ONE, None)), 10), Some(true));
    let (rows, targets) = scoped(&code, &none, &shown, 40);
    assert_eq!(rows[..3], ["1 file", "", "› M a +1 -1"]);
    assert_eq!(targets, [(2..3, Control::File(0))]);
}

#[test]
fn the_list_of_commits_fits_every_width_and_cleans_its_headlines() {
    let hostile = "Fix \u{1b}[2J the \u{7}bell, 漢字漢字, and a headline long enough to wrap";
    let conversation = committed(
        Vec::new(),
        &[
            ("f00d\u{1b}]0;x", hostile, "2026-10-15T08:00:00Z"),
            ("beadbeef", "Two", "2026-10-15T09:00:00Z"),
        ],
    );
    let commits = conversation.commits();
    let shown = Shown::default();
    let mut code = read(slice(ONE, None), 0);
    code.act(Control::Commits, &commits);
    assert!(code.act(Control::Commit(1), &commits));
    // T3 gets the id as it sent it.
    assert_eq!(code.commit(), Some("f00d\u{1b}]0;x"));
    code.act(Control::Commits, &commits);
    let (rows, _) = scoped(&code, &conversation, &shown, 80);
    let cleaned = "Fix [2J the bell, 漢字漢字, and a headline long enough to wrap";
    assert_eq!(
        rows[..5],
        [
            format!("⌄ {cleaned} · f00d]0;"),
            "    All commits".into(),
            entry_row("    ", "Two", "beadbee", 80),
            entry_row("  ✓ ", cleaned, "f00d]0;", 80),
            String::new(),
        ]
    );
    for width in 1..=80 {
        let threads = conversation.threads();
        let (lines, targets) = code.lay_out(threads, &commits, &shown, width, NOW, &theme());
        for line in &lines {
            let text = line.to_string();
            assert!(text.width() <= width, "{width}: {text:?}");
            assert!(
                !text.contains(|c: char| c.is_control()),
                "{width}: {text:?}"
            );
        }
        assert!(targets.iter().all(|(rows, _)| rows.end <= lines.len()));
        // Each entry is one row, and the row that opens the list wraps under itself.
        let entries: Vec<_> = targets
            .iter()
            .filter(|(_, control)| matches!(control, Control::AllCommits | Control::Commit(_)))
            .collect();
        assert_eq!(entries.len(), 3, "{width}");
        assert!(entries.iter().all(|(rows, _)| rows.len() == 1), "{width}");
        let rows = rows_of(&lines);
        // The id shows beside at least four columns of the headline.
        let two = &rows[targets[2].0.start];
        assert_eq!(
            two.ends_with("beadbee"),
            width >= 4 + 4 + 1 + 7,
            "{width}: {two:?}"
        );
        // Where the width leaves no room beside the lead, the headline shows without it.
        if (2..=4).contains(&width) {
            assert!(two.starts_with('T'), "{width}: {two:?}");
        }
    }

    // The row closes the list as it opened it, and an open list has nothing to pick once the
    // conversation lists no commits.
    code.act(Control::Commits, &commits);
    assert!(!code.has(Control::AllCommits, &commits));
    let (rows, _) = scoped(&code, &conversation, &shown, 80);
    assert_eq!(
        rows[..2],
        [format!("› {cleaned} · f00d]0;"), LOADING.into()]
    );
    code.act(Control::Commits, &commits);
    assert!(code.has(Control::AllCommits, &commits));
    assert!(!code.has(Control::AllCommits, &[]));
}

/// A file changed in three places of its 400 lines: 2 unchanged lines before the first hunk,
/// 250 between it and the second, 1 between the second and the third, and 144 after it.
const STRETCHED: &str = "diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -3 +3 @@
-o3
+l3
@@ -254 +254 @@
-o254
+l254
@@ -256 +256 @@
-o256
+l256
";

/// A file's text whose line `n` is `l{n}`, for lines 1 to `count`.
fn numbered(count: u64) -> String {
    (1..=count).map(|n| format!("l{n}\n")).collect()
}

/// A file's text, as T3 sends it.
fn text(new: String) -> Result<Box<FileContents>, String> {
    Ok(Box::new(FileContents { new }))
}

/// The rows of unchanged lines `numbers` with a gutter of `digits`.
fn unchanged(numbers: std::ops::RangeInclusive<u64>, digits: usize) -> Vec<String> {
    numbers.map(|n| format!("{n:>digits$}   l{n}")).collect()
}

/// The runs of unchanged lines each target offers more of, with the text of its first row.
fn stretch_targets(code: &Code, width: usize) -> Vec<(usize, usize, String)> {
    let (lines, targets) = laid(code, width);
    let rows = rows_of(&lines);
    targets
        .into_iter()
        .filter_map(|(at, control)| match control {
            Control::Stretch(file, place) => {
                assert_eq!(file, 0);
                Some((place, at.start, rows[at.start].clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn the_unchanged_lines_around_hunks_show_a_run_at_a_time() {
    let mut code = read(slice(STRETCHED, None), 0);
    assert!(!code.has(Control::Stretch(0, 1), &[]), "folded");
    code.act(Control::File(0), &[]);
    let header = ["1 file", "", "⌄ M f.rs +3 -3"].map(String::from).to_vec();
    let mut rows_before = header.clone();
    rows_before.extend(
        [
            "⋯ 2 unmodified lines",
            "  3 - o3",
            "  3 + l3",
            "⋯ 250 unmodified lines",
            "254 - o254",
            "254 + l254",
            "⋯ 1 unmodified line",
            "256 - o256",
            "256 + l256",
            "⋯ More unchanged context may be available",
        ]
        .map(String::from),
    );
    assert_eq!(rows(&code, 80), rows_before);
    let counted: Vec<_> = stretch_targets(&code, 80)
        .into_iter()
        .map(|(place, row, _)| (place, row))
        .collect();
    assert_eq!(counted, [(0, 3), (1, 6), (2, 9), (3, 12)]);
    for place in 0..4 {
        assert!(code.has(Control::Stretch(0, place), &[]), "{place}");
    }
    assert!(!code.has(Control::Stretch(0, 4), &[]));
    assert!(!code.has(Control::Stretch(1, 0), &[]));
    let action = |code: &Code, place| code.action(Control::Stretch(0, place), &[]);
    assert_eq!(action(&code, 0), "show 2 unmodified lines");
    assert_eq!(action(&code, 1), "show 200 unmodified lines");
    assert_eq!(action(&code, 2), "show 1 unmodified line");
    assert_eq!(action(&code, 3), "show unmodified lines");

    // The first Enter wants the file's text, once, and its run says so until it comes. The
    // rows laid out before go.
    code.prepare(80, &theme());
    assert!(!code.act(Control::Stretch(0, 1), &[]));
    assert_eq!(rows(&code, 80)[6], "⋯ 250 unmodified lines · Reading…");
    assert_eq!(rows(&code, 80)[3], "⋯ 2 unmodified lines");
    let asked = code.ask_text(5);
    assert_eq!(
        asked,
        Some((ChangeType::Change, "f.rs".into(), "f.rs".into()))
    );
    assert_eq!(code.ask_text(6), None);
    code.act(Control::Stretch(0, 1), &[]);
    assert_eq!(code.ask_text(6), None, "one read at a time");
    assert!(!code.take_text(4, text(numbered(400))));

    // Once it came, the run shows 100 lines at each end, and a run of one line shows whole.
    code.prepare(80, &theme());
    assert!(code.take_text(5, text(numbered(400))));
    assert!(!code.take_text(5, text(numbered(400))), "taken");
    let mut after = header.clone();
    after.extend(["⋯ 2 unmodified lines", "  3 - o3", "  3 + l3"].map(String::from));
    after.extend(unchanged(4..=103, 3));
    after.push("⋯ 50 unmodified lines".into());
    after.extend(unchanged(154..=253, 3));
    after.extend(["254 - o254", "254 + l254", "255   l255"].map(String::from));
    after.extend(["256 - o256", "256 + l256", "⋯ 144 unmodified lines"].map(String::from));
    assert_eq!(rows(&code, 80), after);
    assert!(!code.has(Control::Stretch(0, 2), &[]));
    assert_eq!(action(&code, 1), "show 50 unmodified lines");
    assert_eq!(action(&code, 3), "show 100 unmodified lines");
    let counted: Vec<_> = stretch_targets(&code, 80)
        .into_iter()
        .map(|(place, row, _)| (place, row))
        .collect();
    assert_eq!(counted, [(0, 3), (1, 106), (3, 212)]);

    // Before the first hunk, the lines nearest it show, and after the last, those nearest it.
    // With the text read, Enter shows them at once.
    code.prepare(80, &theme());
    code.act(Control::Stretch(0, 0), &[]);
    assert_eq!(code.ask_text(7), None);
    let rows_now = rows(&code, 80);
    assert_eq!(rows_now[3..6], ["  1   l1", "  2   l2", "  3 - o3"]);
    assert!(!code.has(Control::Stretch(0, 0), &[]));
    code.prepare(80, &theme());
    code.act(Control::Stretch(0, 3), &[]);
    let rows_now = rows(&code, 80);
    let end = rows_now.len();
    assert_eq!(rows_now[end - 102..end - 100], ["256 + l256", "257   l257"]);
    assert_eq!(rows_now[end - 2..], ["356   l356", "⋯ 44 unmodified lines"]);
    code.act(Control::Stretch(0, 3), &[]);
    let rows_now = rows(&code, 80);
    assert_eq!(rows_now.last().unwrap(), "400   l400");
    assert!(!code.has(Control::Stretch(0, 3), &[]));

    // The run between two hunks shows whole after another 100 at each end, and its row goes.
    code.act(Control::Stretch(0, 1), &[]);
    assert!(!code.has(Control::Stretch(0, 1), &[]));
    let mut whole_file = header;
    for n in 1..=400 {
        match n {
            3 | 254 | 256 => {
                whole_file.push(format!("{n:>3} - o{n}"));
                whole_file.push(format!("{n:>3} + l{n}"));
            }
            n => whole_file.push(format!("{n:>3}   l{n}")),
        }
    }
    assert_eq!(rows(&code, 80), whole_file);
    assert!(stretch_targets(&code, 80).is_empty());

    // Folded, the file offers none of its runs, and unfolded again it shows them as they were.
    code.act(Control::File(0), &[]);
    assert!(!code.has(Control::Stretch(0, 1), &[]));
    code.act(Control::File(0), &[]);
    assert_eq!(rows(&code, 80).len(), 3 + 403);
}

#[test]
fn a_failed_read_says_why_and_enter_reads_again() {
    let mut code = read(slice(STRETCHED, None), 0);
    code.act(Control::File(0), &[]);
    code.act(Control::Stretch(0, 3), &[]);
    assert!(code.ask_text(2).is_some());
    let failure = Err("This host cannot expand unchanged pull request lines.\u{1b}[2J".into());
    assert!(code.take_text(2, failure));
    let rows_now = rows(&code, 80);
    assert_eq!(
        rows_now[rows_now.len() - 2..],
        [
            "⋯ More unchanged context may be available",
            "This host cannot expand unchanged pull request lines.[2J",
        ]
    );
    // The reason shows under the run Enter was pressed on, and the target is its row alone.
    let counted = stretch_targets(&code, 80);
    assert_eq!(counted.last().unwrap().1, rows_now.len() - 2);
    assert!(code.has(Control::Stretch(0, 3), &[]));
    assert_eq!(code.ask_text(3), None);

    // Enter on another run reads the text again, and the reason goes.
    code.act(Control::Stretch(0, 1), &[]);
    let rows_now = rows(&code, 80);
    assert_eq!(
        rows_now.last().unwrap(),
        "⋯ More unchanged context may be available"
    );
    assert_eq!(rows_now[6], "⋯ 250 unmodified lines · Reading…");
    assert!(code.ask_text(3).is_some());
    assert!(code.take_text(3, text(numbered(400))));
    assert_eq!(rows(&code, 80)[106], "⋯ 50 unmodified lines");
}

#[test]
fn text_that_doesnt_match_the_diff_shows_nothing_of_it() {
    let fails = |new: String| {
        let mut code = read(slice(STRETCHED, None), 0);
        code.act(Control::File(0), &[]);
        code.act(Control::Stretch(0, 1), &[]);
        assert!(code.ask_text(2).is_some());
        assert!(code.take_text(2, text(new)));
        let rows_now = rows(&code, 80);
        let failed = rows_now[7].starts_with("This file's text doesn't match");
        if failed {
            assert_eq!(rows_now[6], "⋯ 250 unmodified lines");
        }
        failed
    };
    assert!(!fails(numbered(400)));
    // A line of a hunk differs, as after a push since the diff was read.
    assert!(fails(numbered(400).replace("l254\n", "l254 changed\n")));
    // The text ends before the last hunk's lines, or before a hunk that only removes.
    assert!(fails(numbered(255)));
    assert!(!fails(numbered(256)));
    // Windows line ends and a last line without one read as the patch's lines do.
    assert!(!fails(numbered(400).replace('\n', "\r\n")));
    assert!(!fails(numbered(256).trim_end().to_string()));
    assert!(fails(String::new()));

    let removal = "diff --git a/g b/g
--- a/g
+++ b/g
@@ -3,2 +2,0 @@
-x
-y
";
    let mut code = read(slice(removal, None), 0);
    code.act(Control::File(0), &[]);
    // The two lines before a removal at the end of the file show, then nothing is left.
    assert_eq!(
        rows(&code, 80),
        [
            "1 file",
            "",
            "⌄ M g +0 -2",
            "⋯ 2 unmodified lines",
            "3 - x",
            "4 - y",
            "⋯ More unchanged context may be available"
        ]
    );
    code.act(Control::Stretch(0, 1), &[]);
    assert!(code.ask_text(2).is_some());
    assert!(code.take_text(2, text("a\nb\n".into())));
    assert_eq!(
        rows(&code, 80)[3..],
        ["⋯ 2 unmodified lines", "3 - x", "4 - y"]
    );
    code.act(Control::Stretch(0, 0), &[]);
    assert_eq!(rows(&code, 80)[3..5], ["1   a", "2   b"]);
    assert!(fits("a\n", removal, &patch::Patch::read(removal).files[0]).is_none());
}

#[test]
fn only_a_file_changed_in_place_or_renamed_and_changed_offers_its_lines() {
    let patch = "diff --git a/added b/added
new file mode 100644
--- /dev/null
+++ b/added
@@ -0,0 +1 @@
+a
diff --git a/deleted b/deleted
deleted file mode 100644
--- a/deleted
+++ /dev/null
@@ -1 +0,0 @@
-a
diff --git a/moved b/moved
similarity index 100%
rename from moved
rename to there
diff --git a/old b/new
similarity index 90%
rename from old
rename to new
--- a/old
+++ b/new
@@ -5 +5 @@
-x
+y
diff --git a/skewed b/skewed
--- a/skewed
+++ b/skewed
@@ -5 +7 @@
-x
+y
";
    let mut code = read(slice(patch, None), 0);
    for at in 0..5 {
        code.act(Control::File(at), &[]);
    }
    let files = (0..5)
        .map(|at| code.file(at).unwrap().1.path().to_string())
        .collect::<Vec<_>>();
    let offered: Vec<_> = (0..5)
        .filter(|&at| (0..2).any(|place| code.has(Control::Stretch(at, place), &[])))
        .map(|at| files[at].as_str())
        .collect();
    assert_eq!(offered, ["new"]);
    let new = files.iter().position(|path| path == "new").unwrap();
    code.act(Control::Stretch(new, 0), &[]);
    let asked = code.ask_text(2);
    assert_eq!(
        asked,
        Some((ChangeType::RenameChanged, "old".into(), "new".into()))
    );
    // A file whose hunk headers leave more lines before a hunk on one side than the other
    // counts them as before, offers nothing, and wants no text.
    let skewed = files.iter().position(|path| path == "skewed").unwrap();
    code.act(Control::Stretch(skewed, 0), &[]);
    assert_eq!(code.ask_text(3), None);
    let (lines, targets) = laid(&code, 80);
    let rows_now = rows_of(&lines);
    assert!(rows_now.contains(&"⋯ 4 unmodified lines".to_string()));
    assert_eq!(
        rows_now
            .iter()
            .filter(|row| row.starts_with("⋯ More"))
            .count(),
        1
    );
    for (rows, control) in &targets {
        if let Control::Stretch(at, _) = control {
            assert_eq!(*at, new);
            assert!(rows_now[rows.start].starts_with('⋯'));
        }
    }

    // A binary file, one past t3term's bound and one with a broken hunk offer nothing either.
    let mut file = patch::Patch::read(STRETCHED).files.remove(0);
    assert_eq!(change_type(&file), Some(ChangeType::Change));
    for broken in 0..3 {
        let mut file = file.clone();
        match broken {
            0 => file.binary = true,
            1 => file.lines_cut = true,
            _ => file.unreadable = true,
        }
        assert_eq!(change_type(&file), None, "{broken}");
        assert_eq!(stretches(&file), None, "{broken}");
    }
    file.old_path = None;
    assert_eq!(change_type(&file), None);

    // Nor does a file added or deleted whose header names both paths.
    let named = "diff --git a/fresh b/fresh\nnew file mode 100644\n--- a/fresh\n+++ b/fresh\n\
                 @@ -0,0 +1 @@\n+a\ndiff --git a/gone b/gone\ndeleted file mode 100644\n\
                 --- a/gone\n+++ b/gone\n@@ -1 +0,0 @@\n-a\n";
    for file in patch::Patch::read(named).files {
        assert!(
            file.old_path.is_some() && file.new_path.is_some(),
            "{file:?}"
        );
        assert_eq!(change_type(&file), None, "{file:?}");
    }
}

#[test]
fn the_text_stays_while_the_hunks_do_and_goes_with_another_diff() {
    let shown_after_read = |code: &mut Code, ticket| {
        code.act(Control::Stretch(0, 1), &[]);
        assert!(code.ask_text(ticket).is_some());
        assert!(code.take_text(ticket, text(numbered(400))));
        assert_eq!(rows(code, 80)[106], "⋯ 50 unmodified lines");
    };
    let mut code = read(slice(STRETCHED, None), 0);
    code.act(Control::File(0), &[]);
    shown_after_read(&mut code, 2);

    // A read of the diff again with the same hunks keeps the lines shown.
    code.again();
    assert_eq!(code.ask(3), Some(None));
    assert_eq!(code.take(3, Ok(slice(STRETCHED, None)), 1), Some(true));
    assert_eq!(rows(&code, 80)[106], "⋯ 50 unmodified lines");

    // A change to a hunk takes the file back to its rows before the text came.
    let changed = STRETCHED.replace("-o256", "-p256");
    code.again();
    assert_eq!(code.ask(4), Some(None));
    assert_eq!(code.take(4, Ok(slice(&changed, None)), 2), Some(true));
    assert_eq!(rows(&code, 80)[6], "⋯ 250 unmodified lines");
    shown_after_read(&mut code, 5);

    // A file the first slice of a read again doesn't have keeps its text while slices are
    // left to read, and loses it once none are.
    let other = "diff --git a/z b/z
--- a/z
+++ b/z
@@ -1 +1 @@
-a
+b
";
    code.again();
    assert_eq!(code.ask(6), Some(None));
    assert_eq!(code.take(6, Ok(slice(other, Some("2"))), 3), Some(true));
    code.near_end();
    assert_eq!(code.ask(7), Some(Some("2".into())));
    assert_eq!(code.take(7, Ok(slice(&changed, None)), 3), Some(false));
    let rows_now = rows(&code, 80);
    assert!(rows_now.contains(&"⋯ 50 unmodified lines".to_string()));
    code.again();
    assert_eq!(code.ask(8), Some(None));
    assert_eq!(code.take(8, Ok(slice(other, None)), 4), Some(true));
    assert_eq!(code.take(8, Ok(slice(&changed, None)), 4), None);
    code.again();
    assert_eq!(code.ask(9), Some(None));
    assert_eq!(code.take(9, Ok(slice(&changed, None)), 5), Some(true));
    assert_eq!(rows(&code, 80)[6], "⋯ 250 unmodified lines");

    // An answer for a file the diff no longer shows is dropped.
    code.act(Control::Stretch(0, 1), &[]);
    assert!(code.ask_text(10).is_some());
    code.again();
    assert_eq!(code.ask(11), Some(None));
    assert_eq!(code.take(11, Ok(slice(other, None)), 6), Some(true));
    assert!(!code.take_text(10, text(numbered(400))));

    // Another commit's diff starts with no text read.
    let mut code = read(slice(STRETCHED, None), 0);
    code.act(Control::File(0), &[]);
    shown_after_read(&mut code, 2);
    let conversation = committed(Vec::new(), &[("abc1234def", "One", "2026-10-15T08:00:00Z")]);
    assert!(code.act(Control::Commit(0), &conversation.commits()));
    assert_eq!(code.ask(3), Some(None));
    assert_eq!(code.take(3, Ok(slice(STRETCHED, None)), 1), Some(true));
    code.act(Control::File(0), &[]);
    assert_eq!(rows(&code, 80)[6], "⋯ 250 unmodified lines");
}

#[test]
fn a_files_text_goes_when_its_hunks_or_paths_change() {
    let read_text = |code: &mut Code, ticket| {
        code.act(Control::Stretch(0, 1), &[]);
        assert!(code.ask_text(ticket).is_some());
        assert!(code.take_text(ticket, text(numbered(400))));
        assert!(rows(code, 80).contains(&"⋯ 50 unmodified lines".to_string()));
    };
    let read_again = |code: &mut Code, ticket, patch: &str| {
        code.again();
        assert_eq!(code.ask(ticket), Some(None));
        assert_eq!(code.take(ticket, Ok(slice(patch, None)), 1), Some(true));
    };
    let unread = |code: &Code, before: &str, between: &str| {
        let rows_now = rows(code, 80);
        assert_eq!(rows_now[3], before, "{rows_now:#?}");
        assert!(rows_now.contains(&between.to_string()), "{rows_now:#?}");
        assert!(
            !rows_now.iter().any(|row| row.ends_with("   l4")),
            "{rows_now:#?}"
        );
    };
    let mut code = read(slice(STRETCHED, None), 0);
    code.act(Control::File(0), &[]);
    read_text(&mut code, 2);

    // Hunks that change and change back don't bring back the text read before.
    read_again(&mut code, 3, &STRETCHED.replace("-o256", "-p256"));
    read_again(&mut code, 4, STRETCHED);
    unread(&code, "⋯ 2 unmodified lines", "⋯ 250 unmodified lines");

    // Nor does a hunk that moves with the same lines, as after the base gained a line.
    read_text(&mut code, 5);
    let moved = STRETCHED.replace("@@ -3 +3 @@", "@@ -4 +4 @@");
    read_again(&mut code, 6, &moved);
    unread(&code, "⋯ 3 unmodified lines", "⋯ 249 unmodified lines");

    // Nor a file renamed before and changed in place now, which T3 reads by other paths.
    let renamed = STRETCHED.replace(
        "diff --git a/f.rs b/f.rs\n--- a/f.rs",
        "diff --git a/e.rs b/f.rs\nsimilarity index 90%\nrename from e.rs\nrename to f.rs\n--- a/e.rs",
    );
    read_again(&mut code, 7, &renamed);
    code.act(Control::Stretch(0, 1), &[]);
    assert_eq!(
        code.ask_text(8),
        Some((ChangeType::RenameChanged, "e.rs".into(), "f.rs".into()))
    );
    assert!(code.take_text(8, text(numbered(400))));
    read_again(&mut code, 9, STRETCHED);
    unread(&code, "⋯ 2 unmodified lines", "⋯ 250 unmodified lines");
    code.act(Control::Stretch(0, 1), &[]);
    assert_eq!(
        code.ask_text(10),
        Some((ChangeType::Change, "f.rs".into(), "f.rs".into()))
    );

    // An answer that comes while the file waits for a later slice is dropped, and the file
    // reads its text again once it's back.
    let other = "diff --git a/z b/z\n--- a/z\n+++ b/z\n@@ -1 +1 @@\n-a\n+b\n";
    code.again();
    assert_eq!(code.ask(11), Some(None));
    assert_eq!(code.take(11, Ok(slice(other, Some("2"))), 2), Some(true));
    assert!(!code.take_text(10, text(numbered(400))));
    code.near_end();
    assert_eq!(code.ask(12), Some(Some("2".into())));
    assert!(code.take(12, Ok(slice(STRETCHED, None)), 2).is_some());
    let rows_now = rows(&code, 80);
    assert!(
        rows_now.contains(&"⋯ 250 unmodified lines".to_string()),
        "{rows_now:#?}"
    );
    assert_eq!(code.file(1).unwrap().1.path(), "f.rs");
    code.act(Control::Stretch(1, 1), &[]);
    assert!(code.ask_text(13).is_some());
}

#[test]
fn a_run_shows_the_lines_nearest_its_hunk_and_widens_the_numbers() {
    // Before the first hunk, the 100 lines nearest it show first.
    let late = "diff --git a/h.rs b/h.rs\n--- a/h.rs\n+++ b/h.rs\n@@ -251 +251 @@\n-o251\n+l251\n";
    let mut code = read(slice(late, None), 0);
    code.act(Control::File(0), &[]);
    assert_eq!(
        code.action(Control::Stretch(0, 0), &[]),
        "show 100 unmodified lines"
    );
    code.act(Control::Stretch(0, 0), &[]);
    assert!(code.ask_text(2).is_some());
    assert!(code.take_text(2, text(numbered(260))));
    let rows_now = rows(&code, 80);
    assert_eq!(rows_now[3], "⋯ 150 unmodified lines");
    assert_eq!(rows_now[4..104], unchanged(151..=250, 3));
    assert_eq!(
        rows_now[104..],
        ["251 - o251", "251 + l251", "⋯ 9 unmodified lines"]
    );

    // Lines past the hunks' numbers widen the column the numbers take.
    let early = "diff --git a/g.rs b/g.rs\n--- a/g.rs\n+++ b/g.rs\n@@ -98 +98 @@\n-o98\n+l98\n";
    let mut code = read(slice(early, None), 0);
    code.act(Control::File(0), &[]);
    assert_eq!(rows(&code, 80)[4..6], ["98 - o98", "98 + l98"]);
    code.act(Control::Stretch(0, 1), &[]);
    assert!(code.ask_text(2).is_some());
    assert!(code.take_text(2, text(numbered(150))));
    let rows_now = rows(&code, 80);
    assert_eq!(
        rows_now[3..6],
        ["⋯ 97 unmodified lines", " 98 - o98", " 98 + l98"]
    );
    assert_eq!(rows_now[6..], unchanged(99..=150, 3));
}

#[test]
fn rows_that_count_lines_keep_their_targets_among_threads() {
    let mut conversation = conversation(vec![
        thread(
            "a",
            "f.rs",
            Some(3),
            "right",
            false,
            &["On the first hunk."],
        ),
        thread("b", "f.rs", Some(254), "left", false, &["On the second."]),
        thread("c", "f.rs", Some(254), "right", false, &["Twice on it."]),
    ]);
    let mut code = read(slice(STRETCHED, None), 0);
    code.act(Control::File(0), &[]);
    code.act(Control::Stretch(0, 1), &[]);
    assert!(code.ask_text(2).is_some());
    assert!(code.take_text(2, text(numbered(400))));
    let shown = Shown::default();
    code.prepare(80, &theme());
    let (rows, targets) = with_threads(&code, &mut conversation, &shown, 80);
    // Every target is in row order, and each run's is the row that counts it.
    let starts: Vec<_> = targets.iter().map(|(rows, _)| rows.start).collect();
    assert!(
        starts.windows(2).all(|pair| pair[0] < pair[1]),
        "{targets:?}"
    );
    let counted: Vec<_> = targets
        .iter()
        .filter_map(|(at, control)| match control {
            Control::Stretch(_, place) => Some((*place, rows[at.start].as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        counted,
        [
            (0, "⋯ 2 unmodified lines"),
            (1, "⋯ 50 unmodified lines"),
            (3, "⋯ 144 unmodified lines")
        ]
    );
    let card = rows
        .iter()
        .position(|row| row.contains("On the first hunk"));
    let fifty = rows.iter().position(|row| row == "⋯ 50 unmodified lines");
    assert!(card.unwrap() < fifty.unwrap());
}

#[test]
fn a_file_read_past_what_the_tab_keeps_forgets_the_first_read() {
    let patch: String = (0..=MOST_TEXTS)
        .map(|n| format!("diff --git a/f{n:02} b/f{n:02}\n--- a/f{n:02}\n+++ b/f{n:02}\n@@ -3 +3 @@\n-o3\n+l3\n"))
        .collect();
    let mut code = read(slice(&patch, None), 0);
    for at in 0..=MOST_TEXTS {
        code.act(Control::File(at), &[]);
    }
    for at in 0..MOST_TEXTS {
        code.act(Control::Stretch(at, 0), &[]);
        let ticket = at as u64 + 10;
        assert!(code.ask_text(ticket).is_some());
        assert!(code.take_text(ticket, text(numbered(4))));
    }
    let shows_first = |code: &Code| {
        let rows = rows(code, 80);
        let header = rows.iter().position(|row| row == "⌄ M f00 +1 -1").unwrap();
        rows[header + 1] == "1   l1"
    };
    assert!(shows_first(&code));
    code.prepare(80, &theme());
    code.act(Control::Stretch(MOST_TEXTS, 0), &[]);
    assert_eq!(code.contexts.len(), MOST_TEXTS);
    assert!(
        !shows_first(&code),
        "the first file's text went, and its rows with it"
    );
    assert_eq!(code.contexts[0].path, "f01");
}

#[test]
fn unchanged_lines_fit_every_width_and_hostile_text_is_cleaned() {
    let hostile: String = (1..=400)
        .map(|n| match n {
            3 | 254 | 256 => format!("l{n}\n"),
            100 => "\u{1b}]0;title\u{7}\t\u{1b}[31mred\u{1b}[0m 漢字漢字漢字\n".to_string(),
            n => format!("line {n} {}\n", "x".repeat(n as usize % 90)),
        })
        .collect();
    let mut code = read(slice(STRETCHED, None), 0);
    code.act(Control::File(0), &[]);
    code.act(Control::Stretch(0, 1), &[]);
    assert!(code.ask_text(2).is_some());
    assert!(code.take_text(2, text(hostile)));
    code.act(Control::Stretch(0, 3), &[]);
    for width in 1..=80 {
        let (lines, targets) = laid(&code, width);
        for line in &lines {
            assert!(line.width() <= width, "{width}: {line:?}");
            let text = line.to_string();
            assert!(
                !text.contains(|c: char| c.is_control()),
                "{width}: {text:?}"
            );
        }
        assert!(targets.iter().all(|(rows, _)| rows.end <= lines.len()));
        let counted = targets
            .iter()
            .filter(|(_, control)| matches!(control, Control::Stretch(..)))
            .count();
        assert_eq!(counted, 3, "{width}");
    }
    let rows_now = rows(&code, 80);
    assert!(rows_now.contains(&"100   ]0;title    [31mred[0m 漢字漢字漢字".to_string()));
}
