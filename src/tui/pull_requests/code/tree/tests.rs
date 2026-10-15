use super::*;
use crate::tui::theme::Depth;
use unicode_width::UnicodeWidthStr;

fn theme() -> Theme {
    Theme::new(Depth::TrueColor)
}

/// The tree of `paths`, each modified, at their places.
fn modified(paths: &[&str]) -> Tree {
    let files = paths.iter().enumerate();
    Tree::new(files.map(|(at, path)| (at, *path, Status::Modified)))
}

/// Each row as its depth, its name, and the place of its first file or its own.
fn outline(rows: &[Row]) -> Vec<(usize, &str, usize)> {
    rows.iter()
        .map(|row| {
            let at = match row.kind {
                Kind::Folder { first, .. } => first,
                Kind::File { at, .. } => at,
            };
            (row.depth, row.name.as_str(), at)
        })
        .collect()
}

#[test]
fn folders_come_before_their_files_in_the_order_the_files_show() {
    let tree = modified(&[
        "src/a/b/c.rs",
        "src/a/b/d.rs",
        "README.md",
        "src/e.rs",
        "docs/x/y.md",
    ]);
    assert_eq!(tree.count(), 5);
    let rows = tree.rows(&HashSet::new());
    // A folder whose only child is a folder shares its row, and a folder takes the place of
    // its first file, so `src` comes before `README.md` and `e.rs` after `b`'s files.
    assert_eq!(
        outline(&rows),
        [
            (0, "src", 0),
            (1, "a/b", 0),
            (2, "c.rs", 0),
            (2, "d.rs", 1),
            (1, "e.rs", 3),
            (0, "README.md", 2),
            (0, "docs/x", 4),
            (1, "y.md", 4),
        ]
    );
    // The row a folder shares opens and closes the last folder it joins.
    assert_eq!(
        rows[1].kind,
        Kind::Folder {
            path: "src/a/b".into(),
            parts: 3,
            first: 0,
            open: true,
        }
    );
    assert_eq!(tree.folders(), ["src", "src/a/b", "docs/x"]);
}

#[test]
fn a_folder_closed_hides_what_is_in_it() {
    let tree = modified(&["src/a/b/c.rs", "src/e.rs", "README.md"]);
    let closed =
        |paths: &[&str]| -> HashSet<String> { paths.iter().map(|path| path.to_string()).collect() };
    let rows = tree.rows(&closed(&["src/a/b"]));
    assert_eq!(
        outline(&rows),
        [
            (0, "src", 0),
            (1, "a/b", 0),
            (1, "e.rs", 1),
            (0, "README.md", 2)
        ]
    );
    assert!(matches!(rows[1].kind, Kind::Folder { open: false, .. }));
    assert!(matches!(rows[0].kind, Kind::Folder { open: true, .. }));
    let rows = tree.rows(&closed(&["src"]));
    assert_eq!(outline(&rows), [(0, "src", 0), (0, "README.md", 2)]);
    // A folder inside a row it shares has no row of its own to close.
    let rows = tree.rows(&closed(&["src/a"]));
    assert_eq!(rows, tree.rows(&HashSet::new()));
    // The folders closed don't change which folders there are.
    assert_eq!(tree.folders(), ["src", "src/a/b"]);
}

#[test]
fn a_path_shows_once_with_how_it_changed() {
    // A file whose type changed is deleted and added again, and shows as modified, at the
    // place of its first.
    let tree = Tree::new([
        (0, "link", Status::Deleted),
        (1, "new.rs", Status::Added),
        (2, "link", Status::Added),
        (3, "new.rs", Status::Added),
    ]);
    assert_eq!(tree.count(), 2);
    let rows = tree.rows(&HashSet::new());
    assert_eq!(outline(&rows), [(0, "link", 0), (0, "new.rs", 1)]);
    assert_eq!(
        rows[0].kind,
        Kind::File {
            at: 0,
            status: Status::Modified
        }
    );
    assert_eq!(
        rows[1].kind,
        Kind::File {
            at: 1,
            status: Status::Added
        }
    );
    assert!(tree.folders().is_empty());
}

#[test]
fn empty_parts_of_a_path_are_skipped() {
    let tree = modified(&["/a//b.rs", "", "/", "a/c.rs"]);
    assert_eq!(tree.count(), 2);
    let rows = tree.rows(&HashSet::new());
    assert_eq!(
        outline(&rows),
        [(0, "a", 0), (1, "b.rs", 0), (1, "c.rs", 3)]
    );
}

#[test]
fn a_path_of_many_parts_takes_no_stack() {
    let deep = vec!["d"; 200_000].join("/") + "/f.rs";
    let tree = modified(&[&deep, "top.rs"]);
    let rows = tree.rows(&HashSet::new());
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].name.len(), 2 * 200_000 - 1);
    assert_eq!((rows[1].depth, rows[1].name.as_str()), (1, "f.rs"));
    // Folders that each hold a file nest, each a row deeper.
    let paths: Vec<String> = (1..=2_000)
        .map(|depth| vec!["d"; depth].join("/") + "/f")
        .collect();
    let tree = modified(&paths.iter().map(String::as_str).collect::<Vec<_>>());
    let rows = tree.rows(&HashSet::new());
    assert_eq!(rows.len(), 4_000);
    assert_eq!((rows[3_998].depth, rows[3_999].depth), (1_999, 2_000));
}

#[test]
fn rows_fit_every_width_and_hostile_names_are_cleaned() {
    let tree = Tree::new([
        (0, "src/\u{1b}[2Jevil/漢字漢字漢字.rs", Status::Added),
        (1, "src/\u{1b}[2Jevil/b\u{7}.rs", Status::Deleted),
        (2, "x", Status::Renamed),
    ]);
    let rows = tree.rows(&HashSet::new());
    for width in 1..=40 {
        for row in &rows {
            for line in lines(row, width, &theme()) {
                let text = line.to_string();
                assert!(text.width() <= width, "{width}: {text:?}");
                assert!(!text.contains(|c: char| c.is_control()), "{text:?}");
            }
        }
    }
    let text = |row: &Row, width| -> Vec<String> {
        let lines = lines(row, width, &theme());
        lines.iter().map(|line| line.to_string()).collect()
    };
    assert_eq!(text(&rows[0], 20), ["⌄ src/[2Jevil"]);
    // A name too long for its row goes on under it, past the folder's chevron or the file's
    // letter.
    assert_eq!(text(&rows[1], 10), ["  A 漢字漢", "    字漢字", "    .rs"]);
    assert_eq!(text(&rows[2], 10), ["  D b.rs"]);
    // The indent takes at most half the row.
    assert_eq!(
        text(&rows[1], 6),
        [
            "  A 漢", "    字", "    漢", "    字", "    漢", "    字", "    .r", "    s"
        ]
    );
    // Where the name has less than two columns past its lead, the row shows what fits.
    assert_eq!(text(&rows[1], 5), ["  A …"]);
    assert_eq!(text(&rows[1], 4), ["  A…"]);
    assert_eq!(text(&rows[0], 3), ["⌄ …"]);
    assert_eq!(text(&rows[0], 1), ["…"]);
}

#[test]
fn a_row_colors_its_lead_and_name_by_how_the_file_changed() {
    let theme = theme();
    let tree = Tree::new([(0, "a/b.rs", Status::Deleted)]);
    let rows = tree.rows(&HashSet::new());
    let styled = |row: &Row| -> Vec<(String, Option<ratatui::style::Color>)> {
        let line = lines(row, 40, &theme).remove(0);
        line.spans
            .iter()
            .filter(|span| !span.content.trim().is_empty())
            .map(|span| (span.content.to_string(), span.style.fg))
            .collect()
    };
    assert_eq!(
        styled(&rows[0]),
        [
            ("⌄".to_string(), Some(theme.muted)),
            ("a".to_string(), Some(theme.fg))
        ]
    );
    assert_eq!(
        styled(&rows[1]),
        [
            ("D".to_string(), Some(theme.error_fg)),
            ("b.rs".to_string(), Some(theme.error_fg))
        ]
    );
    let closed = HashSet::from(["a".to_string()]);
    let rows = tree.rows(&closed);
    assert_eq!(styled(&rows[0])[0].0, "›");
}
