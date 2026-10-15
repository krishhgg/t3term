//! The Code tab's file tree, as the desktop's `DiffFileTree` lists a diff's files beside them
//! (apps/web/src/components/diffs/DiffFileTree.tsx and diffFileTree.logic.ts at f570bd2, with
//! @pierre/trees 1.0.0-beta.6).
//!
//! Each path shows once, with how it changed, as `diffFileTreeEntries` keeps it. Where the files
//! with a path changed in different ways, as a change of a file's type deletes the file and adds
//! it again, it shows as modified. Each folder on the way to a file has a row above it, and a
//! folder takes the place of its first file, so the rows keep the order the files show in, as
//! `diffFileTreePositions` and `compareDiffFileTreeEntries` order them. A folder whose only child
//! is a folder shares its row with it, as `flattenEmptyDirectories` joins them, and the row opens
//! and closes the last of them, as pierre's `getFlattenedTerminalDirectoryId` says. Every folder
//! starts open, as `initialExpansion: "open"` has it.
//!
//! The folders are kept side by side rather than inside each other, and the rows are listed
//! without recursion, so a path of many parts costs no stack.

use std::collections::{HashMap, HashSet};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::super::clip;
use super::super::super::setup::bounded;
use super::super::super::theme::Theme;
use super::{PATH, broken, status};
use crate::pull_request::patch::Status;

/// The files of a diff as folders and files.
pub(super) struct Tree {
    /// Every folder, the root first.
    folders: Vec<Folder>,
    /// How many paths it has.
    count: usize,
}

/// A folder of the tree, with what is in it in the order it came.
#[derive(Default)]
struct Folder {
    /// The place among the tree's folders of each folder in it, by its name.
    named: HashMap<String, usize>,
    children: Vec<Child>,
}

enum Child {
    /// A folder's name, the place among the files of its first file, and its place among the
    /// tree's folders.
    Folder(String, usize, usize),
    /// A file's name, the place among the files of the first with its path, and how it changed.
    File(String, usize, Status),
}

/// A row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
    /// How many folders' rows it is under.
    pub depth: usize,
    /// The names of the folders it joins, between slashes, or the file's name.
    pub name: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Kind {
    /// A folder's row: the path of the last folder it joins, the number of parts in that path,
    /// the place among the files of its first file, and whether it is open.
    Folder {
        path: String,
        parts: usize,
        first: usize,
        open: bool,
    },
    /// A file's row: the place among the files of the first with its path, and how it changed.
    File { at: usize, status: Status },
}

impl Tree {
    /// The tree of `files`, each file's place among the files, its path and how it changed, in
    /// the order the files show.
    pub(super) fn new<'a>(files: impl IntoIterator<Item = (usize, &'a str, Status)>) -> Tree {
        let mut entries: Vec<(&str, usize, Status)> = Vec::new();
        let mut places: HashMap<&str, usize> = HashMap::new();
        for (at, path, status) in files {
            match places.get(path) {
                Some(&place) if entries[place].2 != status => entries[place].2 = Status::Modified,
                Some(_) => {}
                None => {
                    places.insert(path, entries.len());
                    entries.push((path, at, status));
                }
            }
        }
        let mut folders = vec![Folder::default()];
        let mut count = 0;
        for (path, at, status) in entries {
            let mut parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
            let Some(name) = parts.pop() else {
                continue;
            };
            count += 1;
            let mut folder = 0;
            for part in parts {
                folder = match folders[folder].named.get(part) {
                    Some(&inner) => inner,
                    None => {
                        let inner = folders.len();
                        folders.push(Folder::default());
                        let outer = &mut folders[folder];
                        outer.named.insert(part.to_string(), inner);
                        outer
                            .children
                            .push(Child::Folder(part.to_string(), at, inner));
                        inner
                    }
                };
            }
            let file = Child::File(name.to_string(), at, status);
            folders[folder].children.push(file);
        }
        Tree { folders, count }
    }

    /// How many paths the tree has.
    pub(super) fn count(&self) -> usize {
        self.count
    }

    /// The rows that show with the folders whose paths are in `folded` closed, in order: each
    /// folder's row, then the rows in it while it is open.
    pub(super) fn rows(&self, folded: &HashSet<String>) -> Vec<Row> {
        /// A folder being listed: its place, its path, the parts in that path, and the place
        /// of the next of its children to list.
        struct Listing {
            folder: usize,
            path: String,
            parts: usize,
            next: usize,
        }
        let mut rows = Vec::new();
        let mut stack = vec![Listing {
            folder: 0,
            path: String::new(),
            parts: 0,
            next: 0,
        }];
        loop {
            let depth = stack.len().saturating_sub(1);
            let Some(listing) = stack.last_mut() else {
                break;
            };
            let Some(child) = self.folders[listing.folder].children.get(listing.next) else {
                stack.pop();
                continue;
            };
            listing.next += 1;
            let (name, first, mut folder) = match child {
                Child::File(name, at, status) => {
                    rows.push(Row {
                        depth,
                        name: name.clone(),
                        kind: Kind::File {
                            at: *at,
                            status: *status,
                        },
                    });
                    continue;
                }
                Child::Folder(name, first, folder) => (name, *first, *folder),
            };
            let mut names = name.clone();
            let mut path = joined(&listing.path, name);
            let mut parts = listing.parts + 1;
            // A folder whose only child is a folder shares its row.
            while let [Child::Folder(name, _, inner)] = self.folders[folder].children.as_slice() {
                names.push('/');
                names.push_str(name);
                path.push('/');
                path.push_str(name);
                parts += 1;
                folder = *inner;
            }
            let open = !folded.contains(&path);
            rows.push(Row {
                depth,
                name: names,
                kind: Kind::Folder {
                    path: path.clone(),
                    parts,
                    first,
                    open,
                },
            });
            if open {
                stack.push(Listing {
                    folder,
                    path,
                    parts,
                    next: 0,
                });
            }
        }
        rows
    }

    /// The path of every folder's row, open or not.
    pub(super) fn folders(&self) -> Vec<String> {
        let rows = self.rows(&HashSet::new()).into_iter();
        rows.filter_map(|row| match row.kind {
            Kind::Folder { path, .. } => Some(path),
            Kind::File { .. } => None,
        })
        .collect()
    }
}

/// `name` in the folder at `path`, or at the root where `path` is empty.
fn joined(path: &str, name: &str) -> String {
    match path {
        "" => name.to_string(),
        _ => format!("{path}/{name}"),
    }
}

/// The rows `row` takes at `width`: two columns for each folder it is under, at most half the
/// width, then a folder's chevron or a file's letter in the color of how it changed, then its
/// name, cleaned and bounded, on as many rows as it takes. A file's name has the color of how
/// it changed, as pierre colors it. Where the name has less than two columns, room for a wide
/// character, the row shows what fits of it on one row.
pub(super) fn lines(row: &Row, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let indent = (2 * row.depth).min(width / 2);
    let (lead, lead_color, name_color) = match &row.kind {
        Kind::Folder { open, .. } => {
            let chevron = if *open { "⌄" } else { "›" };
            (chevron, theme.muted, theme.fg)
        }
        Kind::File { status: how, .. } => {
            let (letter, color) = status(*how, theme);
            (letter, color, color)
        }
    };
    let name = bounded(&row.name, PATH);
    let pad = indent + 2;
    if width < pad + 2 {
        let text = format!("{}{lead} {name}", " ".repeat(indent));
        return vec![Line::from(Span::raw(clip(&text, width)))];
    }
    let room = width - pad;
    let rows = broken(&name, room).into_iter().enumerate();
    rows.map(|(at, piece)| {
        let mut spans = match at {
            0 => vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(lead, Style::new().fg(lead_color)),
                Span::raw(" "),
            ],
            _ => vec![Span::raw(" ".repeat(pad))],
        };
        let piece = clip(&piece, room);
        spans.push(Span::styled(piece, Style::new().fg(name_color)));
        Line::from(spans)
    })
    .collect()
}

#[cfg(test)]
mod tests;
