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
    code.lay_out(&[], &Shown::default(), width, 0, &theme())
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
    assert_eq!(code.action(Control::File(0)), "expand file");

    // Each line has the number it has after the change, or before it where it was removed,
    // and the unchanged lines between two hunks are counted.
    for at in 0..5 {
        code.act(Control::File(at));
    }
    assert_eq!(code.action(Control::File(0)), "collapse file");
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
            "",
            "⌄ R from.rs → to.rs",
            "No lines changed.",
        ]
    );
    let (_, targets) = laid(&code, 40);
    assert_eq!(targets[1], (7..8, Control::File(1)));
    code.act(Control::File(0));
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
    code.act(Control::File(0));
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
    code.act(Control::File(0));
    assert_eq!(rows(&code, 40)[3], "⋯ 1 unmodified line");
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
    assert!(!code.has(Control::Retry));
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
    assert!(code.has(Control::Retry));
    assert_eq!(code.action(Control::Retry), "retry");
    code.act(Control::Retry);
    assert!(!code.has(Control::Retry));
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
    assert!(!code.has(Control::Retry));
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
    code.act(Control::File(1));

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
        rows(&code, 40),
        ["1 file", "", "⌄ M two +1 -1", "1 - c", "1 + e"]
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
    code.act(Control::File(0));
    code.act(Control::File(1));
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
    code.act(Control::File(2));
    code.act(Control::File(0));
    code.prepare(40, &theme());
    assert_eq!(code.bodies[&(0, 1)].lines.as_ptr(), one);
    let mut kept: Vec<_> = code.bodies.keys().copied().collect();
    kept.sort();
    assert_eq!(kept, [(0, 1), (0, 2)]);
    code.prepare(41, &theme());
    assert_eq!(code.bodies[&(0, 1)].width, 41);

    // A later slice keeps the rows of the files before it, unless it says the host withheld
    // one of them.
    code.act(Control::File(0));
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
    code.act(Control::File(0));
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
    code.act(Control::File(0));
    code.act(Control::File(1));
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
    code.act(Control::File(1));
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
    assert!(code.has(Control::File(1)));
    assert!(!code.has(Control::File(2)));
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
    code.act(Control::File(0));
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
    let (lines, targets) = code.lay_out(conversation.threads(), shown, width, NOW, &theme());
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
    code.act(Control::File(lib));
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
    code.act(Control::File(lib));
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
    code.act(Control::File(0));
    code.act(Control::File(1));
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
    let mut conversation = conversation(threads);
    let mut code = read(slice(&patch, None), 0);
    code.act(Control::File(0));
    let mut shown = Shown::default();
    shown.act(Control::OffDiff, &conversation);
    for width in 1..=80 {
        let showing = code.threads_shown(conversation.threads(), &shown);
        assert_eq!(showing, [0, 1]);
        conversation.lay_out_threads(&showing, width, &theme());
        let (lines, targets) = code.lay_out(conversation.threads(), &shown, width, NOW, &theme());
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
    assert!(
        rows.contains(&"src/]8;;evilname.rsx".to_string()),
        "{rows:#?}"
    );
    // A blank row keeps a thread's remarks apart.
    let at = rows.iter().position(|row| row == "│   Second.").unwrap();
    assert_eq!(rows[at - 2], "│");
    assert!(rows[at - 1].starts_with("│ mona"));
}
