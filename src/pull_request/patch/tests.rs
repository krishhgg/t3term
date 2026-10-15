use super::*;

/// Each line of `file`'s hunks as its mark, its numbers and its text, as `-12 . old`.
fn rows(patch: &str, file: &File) -> Vec<String> {
    let number = |n: Option<u64>| n.map_or(".".to_string(), |n| n.to_string());
    file.hunks
        .iter()
        .flat_map(|hunk| {
            let header = format!(
                "@@ {},{} {},{} {}",
                hunk.old_start,
                hunk.old_count,
                hunk.new_start,
                hunk.new_count,
                &patch[hunk.section.clone()]
            );
            std::iter::once(header).chain(hunk.lines.iter().map(|line| {
                let mark = match line.kind {
                    Kind::Context => ' ',
                    Kind::Added => '+',
                    Kind::Removed => '-',
                    Kind::NoNewline => '\\',
                };
                format!(
                    "{mark}{} {} {}",
                    number(line.old),
                    number(line.new),
                    &patch[line.text.clone()]
                )
            }))
        })
        .collect()
}

/// A file's status, paths, kind, hunks and counts on one line.
fn about(file: &File) -> String {
    let path = |path: &Option<String>| path.clone().unwrap_or("-".into());
    format!(
        "{:?} {} > {}{}, {} hunks +{} -{}",
        file.status,
        path(&file.old_path),
        path(&file.new_path),
        if file.binary { " binary" } else { "" },
        file.hunks.len(),
        file.additions,
        file.deletions
    )
}

#[test]
fn a_modified_file_reads_its_hunks_with_each_line_numbered() {
    let patch = "diff --git a/src/main.rs b/src/main.rs
index 3b18e51..a1c2e3f 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@ fn main() {
 use std::io;
-use std::fs;
+use std::fs::File;
+use std::path::Path;

 fn main() {
@@ -20,2 +21,2 @@
-    old();
+    new();
 }
\\ No newline at end of file
";
    let read = Patch::read(patch);
    assert!(!read.files_cut);
    let [file] = &read.files[..] else {
        panic!("{read:#?}")
    };
    assert_eq!(file.old_path.as_deref(), Some("src/main.rs"));
    assert_eq!(file.path(), "src/main.rs");
    assert_eq!(file.status, Status::Modified);
    assert_eq!((file.additions, file.deletions), (3, 2));
    assert!(!file.binary && !file.unreadable && !file.lines_cut);
    assert_eq!(
        rows(patch, file),
        [
            "@@ 1,4 1,5 fn main() {",
            " 1 1 use std::io;",
            "-2 . use std::fs;",
            "+. 2 use std::fs::File;",
            "+. 3 use std::path::Path;",
            // A blank line in context, whose space an editor dropped.
            " 3 4 ",
            " 4 5 fn main() {",
            "@@ 20,2 21,2 ",
            "-20 .     old();",
            "+. 21     new();",
            " 21 22 }",
            "\\. . \\ No newline at end of file",
        ]
    );
}

#[test]
fn added_deleted_renamed_and_binary_files_say_so() {
    let patch = "diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..e69de29
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+hello
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-one
-two
diff --git a/old name.rs b/new name.rs
similarity index 100%
rename from old name.rs
rename to new name.rs
diff --git a/logo.png b/logo.png
index 1b2c..3d4e 100644
Binary files a/logo.png and b/logo.png differ
diff --git a/icon.png b/icon.png
GIT binary patch
literal 5
Mc${NkU|`@00RR6000

literal 0
HcmV?d00001

diff --git a/script.sh b/script.sh
old mode 100644
new mode 100755
diff --git a/empty b/empty
new file mode 100644
index 0000000..e69de29
diff --git a/emptied b/emptied
deleted file mode 100644
index e69de29..0000000
";
    let read = Patch::read(patch);
    let summary: Vec<_> = read.files.iter().map(about).collect();
    assert_eq!(
        summary,
        [
            "Added - > new.txt, 1 hunks +1 -0",
            "Deleted gone.txt > -, 1 hunks +0 -2",
            "Renamed old name.rs > new name.rs, 0 hunks +0 -0",
            "Modified logo.png > logo.png binary, 0 hunks +0 -0",
            "Modified icon.png > icon.png binary, 0 hunks +0 -0",
            "Modified script.sh > script.sh, 0 hunks +0 -0",
            "Added - > empty, 0 hunks +0 -0",
            "Deleted emptied > -, 0 hunks +0 -0",
        ]
    );
    assert_eq!(read.files[1].path(), "gone.txt");
    assert!(read.files.iter().all(|file| !file.unreadable));
    assert_eq!(rows(patch, &read.files[0]), ["@@ 0,0 1,1 ", "+. 1 hello"]);
}

#[test]
fn paths_with_spaces_quotes_and_other_bytes_read_whole() {
    // Git ends a `---` or `+++` path that has a space with a tab, and quotes a path with a
    // byte outside ASCII or a quote, writing each such byte in octal.
    let patch = "diff --git a/docs/my notes.md b/docs/my notes.md
--- a/docs/my notes.md\t
+++ b/docs/my notes.md\t
@@ -1 +1 @@
-a
+b
diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251 \\\"menu\\\".txt\"
similarity index 90%
rename from \"caf\\303\\251.txt\"
rename to \"caf\\303\\251 \\\"menu\\\".txt\"
--- \"a/caf\\303\\251.txt\"
+++ \"b/caf\\303\\251 \\\"menu\\\".txt\"
@@ -1 +1 @@
-x
+y
diff --git a/a b/c b/a b/c
old mode 100644
new mode 100755
";
    let read = Patch::read(patch);
    let paths: Vec<_> = read
        .files
        .iter()
        .map(|file| (file.old_path.as_deref(), file.new_path.as_deref()))
        .collect();
    assert_eq!(
        paths,
        [
            (Some("docs/my notes.md"), Some("docs/my notes.md")),
            (Some("café.txt"), Some("café \"menu\".txt")),
            (Some("a b/c"), Some("a b/c")),
        ]
    );
    assert_eq!(read.files[1].status, Status::Renamed);
    // A byte that isn't UTF-8 reads as U+FFFD.
    assert_eq!(quoted("\"a/\\377x\" rest").0, "a/\u{FFFD}x");
    assert_eq!(quoted("\"a/\\377x\" rest").1, " rest");
    assert_eq!(quoted("\"a\\tb\\\\c\\\"\"").0, "a\tb\\c\"");
    // A quoted path past `PATH` bytes is cut, and the path after it still reads.
    let long = "\\303\\251".repeat(PATH);
    let patch = format!("diff --git \"a/{long}\" \"b/{long}\"\n");
    let read = Patch::read(&patch);
    let (old, new) = (&read.files[0].old_path, &read.files[0].new_path);
    assert_eq!(old.as_deref().map(str::len), Some(PATH));
    assert_eq!(old, new);
}

#[test]
fn t3s_page_of_files_and_a_plain_diff_read_too() {
    // T3 writes a file the host gave no patch for as its header alone
    // (gitHubPullRequestJson.ts:3219-3266), and a plain unified diff has no `diff --git`.
    let patch = "diff --git a/big.json b/big.json
--- a/big.json
+++ b/big.json
--- a/notes.txt\t2026-10-15 08:00:00
+++ b/notes.txt\t2026-10-15 09:00:00
@@ -1 +1,2 @@
 one
+two
";
    let read = Patch::read(patch);
    let paths: Vec<_> = read.files.iter().map(File::path).collect();
    assert_eq!(paths, ["big.json", "notes.txt"]);
    assert!(read.files[0].hunks.is_empty());
    assert_eq!(read.files[1].additions, 1);
    let plain = "--- old.txt\n+++ new.txt\n@@ -1 +1 @@\n-a\n+b\n";
    let read = Patch::read(plain);
    assert_eq!(read.files[0].old_path.as_deref(), Some("old.txt"));
    assert_eq!(read.files[0].new_path.as_deref(), Some("new.txt"));
    assert_eq!(Patch::read("").files, []);
    assert_eq!(Patch::read("From 1a2b Mon Sep 17\nSubject: x\n").files, []);
}

#[test]
fn line_ends_hunk_text_and_broken_hunks_are_handled() {
    // Windows line ends are left off the text, and a line in a hunk that looks like a header
    // is still a line.
    let patch = "diff --git a/a.txt b/a.txt\r\n--- a/a.txt\r\n+++ b/a.txt\r\n@@ -1,2 +1,2 @@\r\n-diff --git a/x b/x\r\n+@@ -1 +1 @@\r\n same\r\n";
    let read = Patch::read(patch);
    assert_eq!(
        rows(patch, &read.files[0]),
        [
            "@@ 1,2 1,2 ",
            "-1 . diff --git a/x b/x",
            "+. 1 @@ -1 +1 @@",
            " 2 2 same"
        ]
    );
    assert!(!read.files[0].unreadable);

    // A hunk header that isn't one, and a hunk that ends sooner than its header says, mark
    // the file. The lines around them still read.
    let patch = "diff --git a/b.txt b/b.txt
--- a/b.txt
+++ b/b.txt
@@ -x +1 @@
+lost
diff --git a/c.txt b/c.txt
--- a/c.txt
+++ b/c.txt
@@ -1,3 +1,3 @@
 kept
diff --git a/e.txt b/e.txt
--- a/c.txt
+++ b/c.txt
@@ -1 +1 @@
-c
+d
";
    let read = Patch::read(patch);
    let unreadable: Vec<_> = read.files.iter().map(|file| file.unreadable).collect();
    assert_eq!(unreadable, [true, true, false]);
    assert_eq!(read.files[0].hunks, []);
    assert_eq!(read.files[1].hunks[0].lines.len(), 1);
    assert_eq!((read.files[2].additions, read.files[2].deletions), (1, 1));
    for broken in [
        "@@ -1 +1",
        "@@ -1, +1 @@",
        "@@ 1 +1 @@",
        "@@ -1 -1 @@",
        "@@ -1 +1a @@",
    ] {
        let patch = format!("diff --git a/d b/d\n{broken}\n+x\n");
        let read = Patch::read(&patch);
        assert!(read.files[0].unreadable, "{broken}");
        assert_eq!(read.files[0].hunks, [], "{broken}");
    }
}

#[test]
fn a_patch_past_its_bounds_keeps_what_fits() {
    let mut patch = String::new();
    for n in 0..=MOST_FILES {
        patch.push_str(&format!(
            "diff --git a/f{n} b/f{n}\n--- a/f{n}\n+++ b/f{n}\n@@ -1 +1 @@\n-a\n+b\n"
        ));
    }
    let read = Patch::read(&patch);
    assert!(read.files_cut);
    assert_eq!(read.files.len(), MOST_FILES);
    assert_eq!(
        read.files.last().unwrap().path(),
        format!("f{}", MOST_FILES - 1)
    );
    assert!(read.files.iter().all(|file| file.hunks[0].lines.len() == 2));

    let mut patch = format!(
        "diff --git a/big b/big\n@@ -0,0 +1,{0} @@\n",
        MOST_LINES + 2
    );
    patch.push_str(&"+x\n".repeat(MOST_LINES + 2));
    patch.push_str("diff --git a/next b/next\n@@ -1 +1 @@\n-a\n+b\n");
    let read = Patch::read(&patch);
    let big = &read.files[0];
    assert!(big.lines_cut);
    assert_eq!(big.hunks[0].lines.len(), MOST_LINES);
    // The counts take in every line, kept or not.
    assert_eq!(big.additions, MOST_LINES as u64 + 2);
    assert!(read.files[1].lines_cut);
    assert_eq!(read.files[1].hunks[0].lines, []);
    assert_eq!((read.files[1].additions, read.files[1].deletions), (1, 1));

    // A path past `PATH` bytes is cut on a character boundary.
    let long = "é".repeat(PATH);
    let patch = format!("diff --git a/{long} b/{long}\n");
    let read = Patch::read(&patch);
    let path = read.files[0].path();
    assert!(
        path.len() <= PATH && path.len() > PATH - 2,
        "{}",
        path.len()
    );
    assert!(long.starts_with(path));
}
