//! A unified diff, as git writes it and T3 sends it for a pull request, split into files,
//! hunks and lines. Nothing is copied out of the patch: each line is a range of it. The
//! parser reads what git and T3's own patch for a page of files write
//! (apps/server/src/pullRequest/gitHubPullRequestJson.ts:3219-3266): a `diff --git` line,
//! extended headers, `---` and `+++`, then hunks. A line it can't place is skipped, and the
//! file it is in says so.

use std::ops::Range;

/// The most files read from one patch. GitHub's diff holds at most 300 and a page of its files
/// 100, so a patch past this isn't a pull request's.
pub const MOST_FILES: usize = 3000;
/// The most hunks and lines of hunks read from one patch, in all its files. A hunk's header
/// counts as a line, so a patch of headers alone can't pass it.
pub const MOST_LINES: usize = 200_000;
/// The longest path kept, in bytes, as T3's `FilePath` (packages/contracts/src/pullRequest.ts).
const PATH: usize = 4096;

/// A patch, split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub files: Vec<File>,
    /// Whether the patch has more files than `MOST_FILES`, which are left out.
    pub files_cut: bool,
}

/// One file's changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Its path before the change, or `None` where the change adds it.
    pub old_path: Option<String>,
    /// Its path after the change, or `None` where the change deletes it.
    pub new_path: Option<String>,
    pub status: Status,
    /// Whether git says the file is binary, so its changes aren't lines.
    pub binary: bool,
    pub hunks: Vec<Hunk>,
    /// The lines the hunks add and remove.
    pub additions: u64,
    pub deletions: u64,
    /// Whether hunks or their lines are left out, because the patch passed `MOST_LINES`.
    pub lines_cut: bool,
    /// Whether a line in it couldn't be read, such as a hunk header that isn't one, or a hunk
    /// has fewer or more lines than its header says.
    pub unreadable: bool,
}

/// How a file changed, as the desktop's file tree names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Added,
    Deleted,
    Renamed,
    Modified,
}

/// One hunk: a run of lines around a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u64,
    pub old_count: u64,
    pub new_start: u64,
    pub new_count: u64,
    /// What follows the second `@@`, such as the function the hunk is in, without the space
    /// before it.
    pub section: Range<usize>,
    pub lines: Vec<Line>,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: Kind,
    /// Its number in the file before the change, where it is there.
    pub old: Option<u64>,
    /// Its number in the file after the change, where it is there.
    pub new: Option<u64>,
    /// Its text, without the mark in front and the line's end.
    pub text: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file`, which says the line before it has no line end.
    NoNewline,
}

impl File {
    /// The path to show: the path after the change, or the one before it for a deleted file.
    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or_default()
    }
}

/// Where the parser is.
struct Reading {
    files: Vec<File>,
    files_cut: bool,
    /// Lines read so far, in all files.
    lines: usize,
    /// The lines the open hunk still holds before and after the change.
    old_left: u64,
    new_left: u64,
    old_at: u64,
    new_at: u64,
    /// Whether the file being read is past `MOST_FILES`, so its lines are skipped.
    skipping: bool,
    /// Whether the open file has had its `+++` line, so another `---` starts a file.
    new_side: bool,
    /// Whether the open file has had a hunk, so a line like a hunk's outside one is one more
    /// than the last header counted.
    hunked: bool,
}

impl Patch {
    /// Splits `patch` into files. A patch with no file header, such as an empty one, has no
    /// files.
    pub fn read(patch: &str) -> Patch {
        let mut reading = Reading {
            files: Vec::new(),
            files_cut: false,
            lines: 0,
            old_left: 0,
            new_left: 0,
            old_at: 0,
            new_at: 0,
            skipping: false,
            new_side: false,
            hunked: false,
        };
        let mut at = 0;
        for raw in patch.split_inclusive('\n') {
            let start = at;
            at += raw.len();
            let line = raw.strip_suffix('\n').unwrap_or(raw);
            let line = line.strip_suffix('\r').unwrap_or(line);
            reading.line(line, start);
        }
        if reading.in_hunk() {
            // The patch ends before the last hunk's lines do.
            reading.cut_short();
        }
        Patch {
            files: reading.files,
            files_cut: reading.files_cut,
        }
    }
}

impl Reading {
    fn in_hunk(&self) -> bool {
        self.old_left > 0 || self.new_left > 0
    }

    /// Reads one line, without its end, which starts at byte `start` of the patch.
    fn line(&mut self, line: &str, start: usize) {
        if self.in_hunk() {
            let kind = match line.as_bytes().first() {
                Some(b' ') => Some(Kind::Context),
                Some(b'+') => Some(Kind::Added),
                Some(b'-') => Some(Kind::Removed),
                Some(b'\\') => Some(Kind::NoNewline),
                // An editor may drop the space of a blank line in context.
                None => Some(Kind::Context),
                Some(_) => None,
            };
            // A line belongs to the hunk only where each side it is on has lines left.
            let fits = |kind: &Kind| match kind {
                Kind::Context => self.old_left > 0 && self.new_left > 0,
                Kind::Removed => self.old_left > 0,
                Kind::Added => self.new_left > 0,
                Kind::NoNewline => true,
            };
            if let Some(kind) = kind.filter(fits) {
                self.hunk_line(kind, line, start);
                return;
            }
            // The hunk ends sooner than its header said, or has more lines on one side. The
            // line is read as one outside a hunk, as git reads it.
            self.cut_short();
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            self.start_file(git_paths(rest));
            return;
        }
        if self.skipping {
            return;
        }
        if line.starts_with("\\ ") {
            // The end of the last line of the hunk that just ended has no line end.
            self.hunk_line(Kind::NoNewline, line, start);
            return;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            self.start_hunk(rest, start + 3);
            return;
        }
        if let Some(path) = line.strip_prefix("--- ") {
            let ended = self.new_side || self.file().is_some_and(|file| !file.hunks.is_empty());
            if self.files.is_empty() || ended {
                // A plain unified diff has no `diff --git` line, so `---` starts the file.
                self.start_file((None, None));
            }
            let path = side_path(path, "a/");
            if let Some(file) = self.file() {
                file.old_path = path;
                if file.old_path.is_none() {
                    file.status = Status::Added;
                }
            }
            return;
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            self.new_side = true;
            let path = side_path(path, "b/");
            if let Some(file) = self.file() {
                file.new_path = path;
                if file.new_path.is_none() {
                    file.status = Status::Deleted;
                }
            }
            return;
        }
        let hunked = self.hunked;
        let Some(file) = self.file() else {
            return;
        };
        if hunked && matches!(line.as_bytes().first(), Some(b' ' | b'+' | b'-')) {
            file.unreadable = true;
        } else if line.starts_with("new file mode ") {
            file.status = Status::Added;
            file.old_path = None;
        } else if line.starts_with("deleted file mode ") {
            file.status = Status::Deleted;
            file.new_path = None;
        } else if let Some(path) = line.strip_prefix("rename from ") {
            file.status = Status::Renamed;
            file.old_path = Some(unquoted(path));
        } else if let Some(path) = line.strip_prefix("rename to ") {
            file.status = Status::Renamed;
            file.new_path = Some(unquoted(path));
        } else if let Some(path) = line.strip_prefix("copy from ") {
            file.status = Status::Renamed;
            file.old_path = Some(unquoted(path));
        } else if let Some(path) = line.strip_prefix("copy to ") {
            file.status = Status::Renamed;
            file.new_path = Some(unquoted(path));
        } else if line.starts_with("Binary files ") && line.ends_with(" differ")
            || line == "GIT binary patch"
        {
            file.binary = true;
        }
        // `index`, `similarity index`, `old mode`, `new mode` and the like say nothing shown. So
        // do the lines of a binary patch, which start with a letter and have no space.
    }

    /// Ends the open hunk before its header's counts are used up, which marks its file.
    fn cut_short(&mut self) {
        self.old_left = 0;
        self.new_left = 0;
        if let Some(file) = self.file() {
            file.unreadable = true;
        }
    }

    fn file(&mut self) -> Option<&mut File> {
        if self.skipping {
            return None;
        }
        self.files.last_mut()
    }

    fn start_file(&mut self, (old_path, new_path): (Option<String>, Option<String>)) {
        self.old_left = 0;
        self.new_left = 0;
        self.new_side = false;
        self.hunked = false;
        if self.files.len() == MOST_FILES {
            self.files_cut = true;
            self.skipping = true;
            return;
        }
        self.files.push(File {
            old_path,
            new_path,
            status: Status::Modified,
            binary: false,
            hunks: Vec::new(),
            additions: 0,
            deletions: 0,
            lines_cut: false,
            unreadable: false,
        });
    }

    /// Starts a hunk from its header after `@@ `, which starts at byte `start`.
    fn start_hunk(&mut self, header: &str, start: usize) {
        let full = self.lines >= MOST_LINES;
        let Some(file) = self.file() else {
            return;
        };
        let Some((old, new, section)) = hunk_header(header) else {
            file.unreadable = true;
            return;
        };
        if full {
            // Its lines are still read for the counts, as a kept hunk's past the bound are.
            file.lines_cut = true;
        } else {
            let section_at = start + (header.len() - section.len());
            file.hunks.push(Hunk {
                old_start: old.0,
                old_count: old.1,
                new_start: new.0,
                new_count: new.1,
                section: section_at..section_at + section.len(),
                lines: Vec::new(),
            });
            self.lines += 1;
        }
        self.hunked = true;
        self.old_left = old.1;
        self.new_left = new.1;
        self.old_at = old.0;
        self.new_at = new.0;
    }

    fn hunk_line(&mut self, kind: Kind, line: &str, start: usize) {
        let (old, new) = match kind {
            Kind::Context => (Some(self.old_at), Some(self.new_at)),
            Kind::Removed => (Some(self.old_at), None),
            Kind::Added => (None, Some(self.new_at)),
            Kind::NoNewline => (None, None),
        };
        // `line` passes a line on a side only while that side has lines left.
        if old.is_some() {
            self.old_at += 1;
            self.old_left -= 1;
        }
        if new.is_some() {
            self.new_at += 1;
            self.new_left -= 1;
        }
        let full = self.lines >= MOST_LINES;
        let Some(file) = self.file() else {
            return;
        };
        match kind {
            Kind::Added => file.additions += 1,
            Kind::Removed => file.deletions += 1,
            Kind::Context | Kind::NoNewline => {}
        }
        if full {
            file.lines_cut = true;
            return;
        }
        let Some(hunk) = file.hunks.last_mut() else {
            file.unreadable = true;
            return;
        };
        // The mark in front is one byte, and an empty line in context has none.
        let from = (start + 1).min(start + line.len());
        let text = match kind {
            Kind::NoNewline => start..start + line.len(),
            _ => from..start + line.len(),
        };
        hunk.lines.push(Line {
            kind,
            old,
            new,
            text,
        });
        self.lines += 1;
    }
}

/// The paths of a `diff --git` line after its prefix: `a/old b/new`, either of which git may
/// quote. Where neither is quoted and the two are the same, a space in the path is no
/// trouble. Otherwise it splits at the last ` b/`, and `---`, `+++` and the rename lines
/// correct it.
fn git_paths(rest: &str) -> (Option<String>, Option<String>) {
    if rest.starts_with('"') {
        let (old, after) = quoted(rest);
        let new = after.trim_start();
        let new = if new.starts_with('"') {
            quoted(new).0
        } else {
            new.to_string()
        };
        return (strip(&old, "a/"), strip(&new, "b/"));
    }
    // `a/P b/P`, the same path twice.
    if let Some(both) = rest.strip_prefix("a/") {
        let half = both.len().saturating_sub(3) / 2;
        if both.len() >= 3 && both.is_char_boundary(half) {
            let (old, new) = both.split_at(half);
            if new.strip_prefix(" b/") == Some(old) {
                return (Some(bounded(old)), Some(bounded(old)));
            }
        }
    }
    match rest.rfind(" b/") {
        Some(at) => (strip(&rest[..at], "a/"), strip(&rest[at + 1..], "b/")),
        None => (None, None),
    }
}

/// The path of a `---` or `+++` line, without git's `prefix`. `/dev/null` is `None`. Git ends
/// the line with a tab where the path has a space, and other tools put a time after one.
fn side_path(text: &str, prefix: &str) -> Option<String> {
    let path = if text.starts_with('"') {
        quoted(text).0
    } else {
        text.split('\t').next().unwrap_or_default().to_string()
    };
    if path == "/dev/null" {
        return None;
    }
    strip(&path, prefix).or(Some(bounded(&path)))
}

fn strip(path: &str, prefix: &str) -> Option<String> {
    path.strip_prefix(prefix).map(bounded)
}

/// A path from a rename or copy line, which git quotes as it quotes the others.
fn unquoted(text: &str) -> String {
    match text.starts_with('"') {
        true => bounded(&quoted(text).0),
        false => bounded(text),
    }
}

/// A string git quoted, as C quotes one, and what follows it. Git writes a byte outside ASCII
/// as three octal digits, and the bytes are read as UTF-8, any that aren't as U+FFFD. The
/// caller bounds the string.
fn quoted(text: &str) -> (String, &str) {
    let mut bytes = Vec::new();
    let mut chars = text.char_indices().skip(1).peekable();
    let mut end = text.len();
    let push = |bytes: &mut Vec<u8>, c: char| {
        let mut buffer = [0; 4];
        bytes.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
    };
    while let Some((at, c)) = chars.next() {
        if c == '"' {
            end = at + 1;
            break;
        }
        if c != '\\' {
            push(&mut bytes, c);
            continue;
        }
        let Some((_, escaped)) = chars.next() else {
            break;
        };
        match escaped {
            'n' => bytes.push(b'\n'),
            't' => bytes.push(b'\t'),
            'r' => bytes.push(b'\r'),
            'a' => bytes.push(7),
            'b' => bytes.push(8),
            'f' => bytes.push(12),
            'v' => bytes.push(11),
            '0'..='7' => {
                let mut value = escaped as u32 - '0' as u32;
                for _ in 0..2 {
                    match chars.peek() {
                        Some(&(_, digit @ '0'..='7')) => {
                            value = value * 8 + (digit as u32 - '0' as u32);
                            chars.next();
                        }
                        _ => break,
                    }
                }
                bytes.push(value as u8);
            }
            other => push(&mut bytes, other),
        }
    }
    (String::from_utf8_lossy(&bytes).into_owned(), &text[end..])
}

/// `path` cut to `PATH` bytes on a character boundary.
fn bounded(path: &str) -> String {
    if path.len() <= PATH {
        return path.to_string();
    }
    let mut end = PATH;
    while !path.is_char_boundary(end) {
        end -= 1;
    }
    path[..end].to_string()
}

/// A hunk header after `@@ `: `-a[,b] +c[,d] @@` and the section after it. A count left out is
/// 1, as in unified diffs.
#[allow(clippy::type_complexity)]
fn hunk_header(header: &str) -> Option<((u64, u64), (u64, u64), &str)> {
    let (ranges, section) = header.split_once(" @@")?;
    let (old, new) = ranges.split_once(' ')?;
    let range = |text: &str, mark: char| {
        let text = text.strip_prefix(mark)?;
        let (start, count) = match text.split_once(',') {
            Some((start, count)) => (start, count),
            None => (text, "1"),
        };
        let digits = |text: &str| {
            (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| text.parse::<u64>().ok())
                .flatten()
        };
        Some((digits(start)?, digits(count)?))
    };
    let section = section.strip_prefix(' ').unwrap_or(section);
    Some((range(old, '-')?, range(new, '+')?, section))
}

#[cfg(test)]
mod tests;
