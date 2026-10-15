use super::*;

/// A patch of files that each add `lines`, or with no hunk where they add none.
fn patch_of(files: &[(&str, &[&str])]) -> String {
    let mut text = String::new();
    for (path, lines) in files {
        text.push_str(&format!("diff --git a/{path} b/{path}\n"));
        if !lines.is_empty() {
            text.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
            for line in *lines {
                text.push_str(&format!("+{line}\n"));
            }
        }
    }
    text
}

/// The paths of `files` in reading order.
fn order(files: &[(&str, &[&str])]) -> Vec<String> {
    let text = patch_of(files);
    let patch = Patch::read(&text);
    reading_order(&text, &patch)
        .into_iter()
        .map(|at| patch.files[at].path().to_string())
        .collect()
}

fn paths(files: &[&str]) -> Vec<String> {
    order(
        &files
            .iter()
            .map(|&path| (path, &[][..]))
            .collect::<Vec<_>>(),
    )
}

// The cases from the desktop's pullRequestFileOrder.logic.test.ts, in its order.

#[test]
fn source_comes_before_tests_and_generated_files_whatever_the_convention() {
    let source = ["src/app.ts", "src/dist.ts", "src/testing.ts"];
    let tests = [
        "src/__tests__/app.ts",
        "src/app.spec.tsx",
        "src/app.test.ts",
        "test/app.ts",
        "tests/helpers/app.ts",
    ];
    let generated = [
        "apps/web/package-lock.json",
        "dist/app.js",
        "packages/core/vendor/lib.js",
        "pnpm-lock.yaml",
        "public/app.min.js",
        "src/__snapshots__/app.ts",
        "src/api.generated.ts",
        "src/app.test.ts.snap",
    ];
    let mut all: Vec<&str> = [&generated[..], &tests, &source].concat();
    all.reverse();
    assert_eq!(paths(&all), [&source[..], &tests, &generated].concat());
    assert_eq!(paths(&[]), Vec::<String>::new());
}

#[test]
fn a_file_comes_after_what_it_imports() {
    let b = r#"import { b } from "./b";"#;
    let c = r#"import { c } from "./c";"#;
    assert_eq!(
        order(&[("src/a.ts", &[b]), ("src/b.ts", &[])]),
        ["src/b.ts", "src/a.ts"]
    );
    // A chain the whole way down.
    assert_eq!(
        order(&[("src/a.ts", &[b]), ("src/b.ts", &[c]), ("src/c.ts", &[])]),
        ["src/c.ts", "src/b.ts", "src/a.ts"]
    );
    // A specifier that climbs out of the importer's directory.
    assert_eq!(
        order(&[
            ("src/ui/a.ts", &[r#"import { b } from "../lib/b";"#]),
            ("src/lib/b.ts", &[])
        ]),
        ["src/lib/b.ts", "src/ui/a.ts"]
    );
    // A directory means its index file.
    assert_eq!(
        order(&[
            ("src/a.ts", &[r#"import { b } from "./lib";"#]),
            ("src/lib/index.ts", &[])
        ]),
        ["src/lib/index.ts", "src/a.ts"]
    );
    // A path that names nothing changed falls back to its last segment.
    assert_eq!(
        order(&[
            ("src/a.ts", &[r#"import { b } from "~/lib/b";"#]),
            ("lib/b.ts", &[])
        ]),
        ["lib/b.ts", "src/a.ts"]
    );
    // Unless two files have that name.
    assert_eq!(
        order(&[
            ("src/a.ts", &[r#"import { b } from "~/somewhere/b";"#]),
            ("one/b.ts", &[]),
            ("two/b.ts", &[]),
        ]),
        ["one/b.ts", "src/a.ts", "two/b.ts"]
    );
    // A specifier with an extension, the `.js` beside a `.ts` among them.
    assert_eq!(
        order(&[
            ("src/a.ts", &[r#"import { b } from "./b.js";"#]),
            ("src/b.ts", &[])
        ]),
        ["src/b.ts", "src/a.ts"]
    );
    // The path, not the name two files share, finds the `.ts` the `.js` means.
    assert_eq!(
        order(&[
            ("src/a.ts", &[r#"import { b } from "./b.js";"#]),
            ("src/b.ts", &[]),
            ("lib/b.ts", &[]),
        ]),
        ["lib/b.ts", "src/b.ts", "src/a.ts"]
    );
    // A file that imports itself waits on nothing.
    assert_eq!(
        order(&[
            ("src/a.ts", &[r#"import { a } from "./a";"#]),
            ("src/b.ts", &[])
        ]),
        ["src/a.ts", "src/b.ts"]
    );
    // `require` and a bare import.
    assert_eq!(
        order(&[
            (
                "src/a.ts",
                &[r#"const b = require("./b");"#, r#"import "./c";"#]
            ),
            ("src/b.ts", &[]),
            ("src/c.ts", &[]),
        ]),
        ["src/b.ts", "src/c.ts", "src/a.ts"]
    );
}

#[test]
fn a_cycle_and_unrelated_files_go_by_path() {
    assert_eq!(
        order(&[
            ("src/b.ts", &[r#"import { a } from "./a";"#]),
            ("src/a.ts", &[r#"import { b } from "./b";"#]),
        ]),
        ["src/a.ts", "src/b.ts"]
    );
    assert_eq!(
        paths(&["src/ui/b.ts", "src/lib/z.ts", "src/lib/a.ts"]),
        ["src/lib/a.ts", "src/lib/z.ts", "src/ui/b.ts"]
    );
}

#[test]
fn tests_follow_the_source_they_cover() {
    assert_eq!(
        order(&[
            ("pnpm-lock.yaml", &[]),
            ("src/a.test.ts", &[]),
            ("src/__tests__/b.ts", &[]),
            ("src/a.ts", &[r#"import { b } from "./b";"#]),
            ("src/b.ts", &[]),
            ("dist/a.js", &[]),
        ]),
        [
            "src/b.ts",
            "src/a.ts",
            "src/__tests__/b.ts",
            "src/a.test.ts",
            "dist/a.js",
            "pnpm-lock.yaml",
        ]
    );
    // By where their source is, though the paths would sort the other way, and a test whose
    // source isn't in the slice comes after those whose source is.
    assert_eq!(
        order(&[
            ("src/0.test.ts", &[]),
            ("src/a.test.ts", &[]),
            ("src/b.test.ts", &[]),
            ("src/a.ts", &[r#"import { b } from "./b";"#]),
            ("src/b.ts", &[]),
        ]),
        [
            "src/b.ts",
            "src/a.ts",
            "src/b.test.ts",
            "src/a.test.ts",
            "src/0.test.ts"
        ]
    );
    assert_eq!(
        paths(&["src/orphan.spec.ts", "src/a.test.ts", "src/a.ts"]),
        ["src/a.ts", "src/a.test.ts", "src/orphan.spec.ts"]
    );
}

#[test]
fn the_same_files_read_the_same_way_in_any_order() {
    let b: &[&str] = &[r#"import { b } from "./b";"#];
    let none: &[&str] = &[];
    let mut files = vec![
        ("src/a.ts", b),
        ("src/b.ts", none),
        ("src/c.ts", none),
        ("src/a.test.ts", none),
        ("yarn.lock", none),
    ];
    let first = order(&files);
    files.reverse();
    assert_eq!(order(&files), first);
}

// What the desktop's tests leave to the library and the browser.

#[test]
fn imports_count_in_every_line_of_a_hunk_and_a_path_keeps_its_last_file() {
    // The desktop's diff library puts a partial patch's unchanged lines on both sides, so an
    // import that didn't change still counts. A removed one does too.
    let text = r#"diff --git a/src/a.ts b/src/a.ts
@@ -1,2 +1,2 @@
 import { b } from "./b";
-import { c } from "./c";
+export const a = 1;
diff --git a/src/b.ts b/src/b.ts
diff --git a/src/c.ts b/src/c.ts
diff --git a/src/c.ts b/src/c.ts
@@ -0,0 +1 @@
+import { a } from "./a";
"#;
    let patch = Patch::read(text);
    // `src/c.ts` keeps its last file, whose import of `a` makes a cycle. `b` is ready first,
    // then the cycle goes by path.
    assert_eq!(reading_order(text, &patch), [1, 0, 3]);
    let text = text.replace("./a", "./nothing");
    let patch = Patch::read(&text);
    assert_eq!(reading_order(&text, &patch), [1, 3, 0]);
}

#[test]
fn specifiers_are_found_as_the_desktop_s_pattern_finds_them() {
    assert_eq!(specifiers(r#"import x from "y";"#), ["y"]);
    assert_eq!(specifiers("export * from './y'"), ["./y"]);
    assert_eq!(specifiers("const y = require ( 'y' )"), ["y"]);
    assert_eq!(specifiers(r#"await import("y")"#), ["y"]);
    assert_eq!(
        specifiers(r#"from"a" from "b" import 'c'"#),
        ["a", "b", "c"]
    );
    // The pattern takes a quote of either kind at either end.
    assert_eq!(specifiers(r#"from "a'"#), ["a"]);
    // A word that only starts or ends like a keyword names nothing, nor does an empty or
    // unclosed specifier, nor `require` without its parenthesis.
    for line in [
        r#"reimport "a""#,
        r#"important "a""#,
        r#"_from "a""#,
        r#"from """#,
        r#"from "a"#,
        r#"require "a""#,
        "from",
        "",
    ] {
        assert_eq!(specifiers(line), Vec::<&str>::new(), "{line}");
    }
    // A keyword after a letter outside ASCII starts a word, as JavaScript's `\b` sees it.
    assert_eq!(specifiers(r#"éfrom "a""#), ["a"]);
}

#[test]
fn paths_collate_as_the_root_collation_does() {
    let mut paths = vec![
        "src/b.ts",
        "src/Button.tsx",
        "src/a.ts",
        "src/A.ts",
        "src/__tests__/a.ts",
        "src/a-b.ts",
        "src/a_b.ts",
        "src/a/x.ts",
        "src/a.b.ts",
        "src/10.ts",
        "src/9.ts",
        "src/é.ts",
        "src/z.ts",
    ];
    paths.sort_by(|left, right| collate(left, right));
    // Node's ICU 77 sorts these the same way, but for `é`, which it puts with `e`.
    assert_eq!(
        paths,
        [
            "src/__tests__/a.ts",
            "src/10.ts",
            "src/9.ts",
            "src/a_b.ts",
            "src/a-b.ts",
            "src/a.b.ts",
            "src/a.ts",
            "src/A.ts",
            "src/a/x.ts",
            "src/b.ts",
            "src/Button.tsx",
            "src/z.ts",
            "src/é.ts",
        ]
    );
}

#[test]
fn names_tiers_and_relative_paths_read_as_the_desktop_s_do() {
    assert_eq!(strip_extension("src/a.b/c"), "src/a.b/c");
    assert_eq!(strip_extension("src/a.test.ts"), "src/a.test");
    assert_eq!(base_name("src/lib/a.ts"), "a");
    assert_eq!(base_name("Makefile"), "Makefile");
    assert_eq!(tested_name("src/a.test.ts"), "a");
    assert_eq!(tested_name("src/a.spec.test.ts"), "a");
    assert_eq!(tested_name("src/__tests__/a.ts"), "a");
    assert_eq!(tested_name("tests/a.b.rs"), "a.b");
    assert_eq!(tested_name("tests/a."), "a.");
    assert_eq!(resolve_relative("src/ui/a.ts", "../lib/./b"), "src/lib/b");
    assert_eq!(resolve_relative("a.ts", "../../b"), "b");
    assert_eq!(tier("Cargo.lock"), Tier::Generated);
    assert_eq!(tier("crates/x/Cargo.lock"), Tier::Generated);
    assert_eq!(tier("build.rs"), Tier::Source);
    assert_eq!(tier("build/out.rs"), Tier::Generated);
    assert_eq!(tier("src/testing.rs"), Tier::Source);
    assert_eq!(tier("tests/cli.rs"), Tier::Test);
    assert_eq!(tier("src/a.spec.ts"), Tier::Test);
    assert_eq!(tier("tests/__snapshots__/a.snap"), Tier::Generated);
}
