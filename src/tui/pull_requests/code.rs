//! The Code tab of a link's details: the pull request's diff, as the desktop's
//! `PullRequestCodeTab` shows it (apps/web/src/components/pullRequest/PullRequestCodeTab.tsx at
//! f570bd2).
//!
//! The tab reads the diff through T3's `/api/pull-requests/diff` once it first shows, a slice at
//! a time, and reads the next slice once the end of the files comes within a page of the view,
//! as the desktop's sentinel does 240 pixels ahead. It reads the diff again from its first slice
//! when it shows after `FRESH` or more, after a read of it failed, and after `r`, a new refresh
//! number from T3 or a new `updatedAt` on the detail, as the desktop's refresh token says
//! (PullRequestDetailPanel.tsx:832-847). The slices read show until the new first one comes.
//!
//! Each slice's files show in the desktop's reading order, source before tests before the files
//! a tool wrote, as `order` says. Each file shows folded on one row, as the desktop's do by
//! default: whether the change added,
//! deleted, renamed or modified it, its path, and the lines it adds and removes. Enter or a
//! click unfolds it into its hunks. Each line has one number, the one it has after the change,
//! or before it for a removed line, as the desktop's stacked view has, and the unchanged lines
//! between two hunks are counted on a row of their own.
//!
//! Where the conversation read lists commits, a row above the files says which diff the tab
//! shows, `All commits` or a commit's headline, and opens a list of the commits, newest first,
//! 10 at a time, as the desktop's scope menu does (PullRequestCodeTab.tsx:1063-1119). Picking
//! one reads that commit's own diff in place of the whole change's, from its first slice, with
//! the files folded and the list of conversations off the diff closed. A read of the
//! conversation that no longer lists the commit picked goes back to the whole change, as after
//! a force-push (1050-1062).
//!
//! The review threads the conversation read show under the lines they are on, in the files
//! unfolded, as the desktop's annotations do. A thread shows under a line only where a line of
//! the file's hunks has its number on its side, as `isLineInFileDiff` asks
//! (pullRequestDiff.logic.ts:12-22). The rest, on a line the diff doesn't show, on a file or on
//! a file not read yet, show above the files in a list that starts closed, by file. Under a
//! commit's own diff, where a line's number may mean other code than the thread's, every thread
//! shows in that list (PullRequestCodeTab.tsx:454-458).
//!
//! The unchanged lines before each hunk of a file changed in place or renamed and changed, and
//! after its last, can show, as the desktop's diff viewer expands them (@pierre/diffs 1.5.2).
//! Enter or a click on the row that counts them reads the file's whole text once, through
//! `pullRequests.diffFileContents`, then shows up to `EXPANSION` more of them at each end
//! between two hunks, before the first hunk at the end nearest it, and after the last at the
//! end nearest it. A run of one line shows whole once the text came. The row after the last
//! hunk says only that there may be more until the text says how many. The text read stays
//! while a read of the diff again leaves the file's hunks as they were, and goes with another
//! commit's diff.
//!
//! Where the detail says the host or T3 keeps the reader's marks on files viewed, the tab reads
//! them through `pullRequests.filesViewed` once it first shows, again when it shows after
//! `VIEWED_FRESH`, and again with the diff. The count of files says how many of them are
//! viewed, and each file's header says `✓ Viewed`, or `Changed since viewed` where it was
//! pushed to since, as the desktop's checkbox and its `Changed` say (PullRequestCodeTab.tsx:
//! 832-870 and 1127-1180). The marks are the reader's whatever diff shows, the whole change's
//! or a commit's.
//!
//! The changes to whitespace start hidden, as the desktop's `diffIgnoreWhitespace` setting is
//! true until changed (packages/contracts/src/settings.ts:377): a line changed only in its
//! whitespace shows unchanged, with its text after the change, as `whitespace` says. A row
//! above the files shows them, or hides them again, as the desktop's toolbar toggle does
//! (PullRequestCodeTab.tsx:1211-1234), for every file and every diff the tab shows after.
//!
//! The lines stack at first, as the desktop's `diffLayout` setting is `stacked` until changed
//! (packages/contracts/src/settings.ts:53-55). A row under the fold row sets the lines before
//! the change on the left and those after it on the right, or stacks them again, as the
//! desktop's toolbar toggle group does (PullRequestCodeTab.tsx:1259-1278). The lines pair as
//! @pierre/diffs pairs them, as `align` says, and a file added or deleted keeps one column.
//! Unwrapped, both sides' text scrolls sideways by the same columns.
//!
//! The lines wrap at first, as the desktop's `wordWrap` setting is true until changed
//! (packages/contracts/src/settings.ts:506), each where a browser would break it, as `wrapped`
//! says. A row under the layout row lets them run on past
//! the end of their rows, or wraps them again, as the desktop's toolbar toggle does
//! (PullRequestCodeTab.tsx:1279-1298). Each line then takes one row, and the panel scrolls the
//! text of those rows sideways while their numbers stay, as `Sideways` says, as the desktop's
//! numbers stay while its lines scroll.
//!
//! A row under the wrap row shows the files in a tree, or hides it again, as the desktop's
//! toolbar toggle does (PullRequestCodeTab.tsx:1299-1318). It starts hidden, as the desktop's
//! `t3code.pullRequestFileTreeOpen` is false until changed. The details have no room for the
//! desktop's tree beside the files, so it shows above them, as `tree` lays it out, under a row
//! that counts its paths and one that closes every folder or opens them all. A folder's row
//! opens or closes it, and a file's row unfolds the file and shows it at the top of the
//! details, as the desktop's `revealFile` does (665-673). While slices are left, a row under the
//! tree reads the next, as the desktop's footer does (1530-1549).
//!
//! Only the files unfolded are laid out, each once for each width. Each line of them is cleaned
//! of control characters, its tabs set every `TAB` columns, and cut at `LINE` characters as it
//! is.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use textwrap::WordSeparator;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::setup::bounded;
use super::super::theme::Theme;
use super::super::{clip, row};
use super::conversation::{Commit, Control, Shown, Targets, Thread};
use super::{FAILURE, FRESH, flowed, grouped, whole};
use crate::pull_request::{
    self, ChangeType, FileContents, FilesViewed, Side, ViewedFilesStore, ViewedState, patch,
};
use crate::transcript::clusters;
use tree::Tree;

/// The columns between two tab stops in a line of the diff.
const TAB: usize = 4;
/// The most characters of a line of the diff shown. A longer one, such as a line of a minified
/// file, is cut with `…`.
const LINE: usize = 2000;
/// The longest path shown, in characters.
const PATH: usize = 1024;
/// The most unchanged lines one Enter shows at each end of a run of them, as the desktop's
/// `expansionLineCount` (@pierre/diffs 1.5.2, DiffHunksRenderer.js:381).
const EXPANSION: u64 = 100;
/// The most files whose text the tab keeps. Reading one more forgets the first read.
const MOST_TEXTS: usize = 64;
/// The spans a side of a row of the lines side by side takes: the number, the mark and the
/// text. The left side's, with empty ones in front where it has fewer, are followed by one
/// that pads it to its columns and one of `BETWEEN`, then come the right side's.
pub(super) const SIDE: usize = 3;
/// The columns between the two sides of a row of the lines side by side: a bar, then a space
/// so the right side's numbers stand apart from it as the left side's stand apart from the
/// details' edge.
const BETWEEN: &str = "│ ";

const LOADING: &str = "Loading pull request diff…";
const LOADING_MORE: &str = "Loading more files…";
const EMPTY: &str = "This pull request has no file changes.";
const EMPTY_COMMIT: &str = "This commit has no file changes.";
/// How many more commits the list shows at a time, as the desktop's `COMMIT_PAGE_SIZE`.
const COMMIT_PAGE: usize = 10;
const WITHHELD: &str =
    "The host withheld part of this diff, such as a binary file or a change too large to show.";
const REST_FAILED: &str = "t3term couldn't read the rest of this diff.";
const STALE: &str = "May be out of date: t3term couldn't read the diff again.";
const UNREAD: &str = "t3term couldn't split this patch into files.";
const MORE_CONTEXT: &str = "More unchanged context may be available";
const READING: &str = "Reading…";
const UNMATCHED: &str =
    "This file's text doesn't match the diff, which may be out of date. r reads it again.";
/// How long the marks on files viewed stay fresh, in milliseconds, as the desktop's query keeps
/// them (packages/client-runtime/src/state/pullRequests.ts:301-305).
const VIEWED_FRESH: i64 = 15_000;
/// The row that shows the changes to whitespace, and the one that hides them, after the
/// desktop's toggle with its pilcrow.
const WHITESPACE_HIDDEN: &str = "¶ Show whitespace changes";
const WHITESPACE_SHOWN: &str = "¶ Hide whitespace changes";
/// The row that unfolds every file, and the one that folds them all, after the desktop's
/// `Expand all files` and `Collapse all files` with their chevrons.
const FOLDED_ALL: &str = "⇊ Expand all files";
const UNFOLDED_ANY: &str = "⇈ Collapse all files";
/// The row that shows the lines side by side, and the one that shows them one under the other
/// again, after the desktop's `Split diff view` and `Stacked diff view` with their icons of two
/// columns and of rows.
const STACKED: &str = "◫ Split diff view";
const SPLIT: &str = "⊟ Stacked diff view";
/// The row that lets the lines run on past their rows, and the one that wraps them again, after
/// the desktop's `Disable line wrapping` and `Enable line wrapping` with its icon of a line
/// that turns back.
const WRAPPED: &str = "↵ Disable line wrapping";
const UNWRAPPED: &str = "↵ Enable line wrapping";
/// The row that shows the file tree, and the one that hides it, after the desktop's `Show file
/// tree` and `Hide file tree` with its icon of a folder's branches.
const TREE_HIDDEN: &str = "⊢ Show file tree";
const TREE_SHOWN: &str = "⊢ Hide file tree";
/// The tree's row that closes every folder, and the one that opens them all, after the
/// desktop's `Collapse all folders` and `Expand all folders` with the chevrons of the fold row.
const FOLDERS_OPEN: &str = "⇈ Collapse all folders";
const FOLDERS_FOLDED: &str = "⇊ Expand all folders";
/// The tree's row that reads the next slice, as the desktop's footer says.
const MORE_FILES: &str = "Load more files";
const VIEWED_FAILED: &str = "t3term couldn't read which files you viewed.";
const VIEWED_STALE: &str = "The viewed marks may be out of date: t3term couldn't read them again.";
const VIEWED_SHORT: &str = "This change has more files than the host reports viewed marks for in one read, so the count is short.";
const VIEWED_MARK: &str = "✓ Viewed";
const VIEWED_CHANGED: &str = "Changed since viewed";

/// What the Code tab read of a pull request's diff.
pub(super) struct Code {
    /// The slices read, in the order T3 sent them.
    slices: Vec<Slice>,
    /// The files of every slice, as a slice's place and a file's place in it: the slices in the
    /// order T3 sent them, and each slice's files in reading order. A path a slice has twice
    /// shows once, as its last part, as the desktop's `orderDiffFiles` keeps it.
    files: Vec<(usize, usize)>,
    /// The lines the host counted for each file whose changes it withheld, by path, from every
    /// slice.
    omitted: HashMap<String, (u64, u64)>,
    /// The read under way.
    asked: Option<Asked>,
    /// Why the last read failed, cleaned and bounded, and whether it read the first slice.
    failed: Option<(String, bool)>,
    /// When the first slice shown came, which `FRESH` counts from.
    at: i64,
    /// Set when the diff is to be read from its first slice: before the first read, and once
    /// it may be out of date. A read under way then is dropped when it comes.
    again: bool,
    /// Set when the end of the files came within a page of the view, so the next slice is read.
    due: bool,
    /// Whether a file shows unfolded unless the reader folded it: false at first, as the
    /// desktop's `diffFilesCollapsed` setting is true until changed, and set by Expand all
    /// files, which a file read later follows too.
    unfolded: bool,
    /// The paths of the files the reader folded or unfolded apart from `unfolded`, which stay
    /// so when the diff is read again.
    toggled: HashSet<String>,
    /// Whether the changes to whitespace show: false at first, as the desktop's
    /// `diffIgnoreWhitespace` setting is true until changed. Another diff picked from the
    /// commits keeps it, as the desktop's toggle does.
    whitespace: bool,
    /// Whether the lines before and after the change show side by side: false at first, as
    /// the desktop's `diffLayout` setting is `stacked` until changed.
    split: bool,
    /// Whether the lines wrap: true at first, as the desktop's `wordWrap` setting is true until
    /// changed. Another diff picked from the commits keeps it, as the desktop's toggle does.
    wrap: bool,
    /// Whether the file tree shows: false at first, as the desktop's
    /// `t3code.pullRequestFileTreeOpen` is until changed.
    tree: bool,
    /// The paths of the tree's folders the reader closed. They open again when the tree hides
    /// or another diff shows, as the desktop's tree starts over when it shows again.
    folded: HashSet<String>,
    /// The rows of each unfolded file's lines, by its slice's place and its place in that
    /// slice, so a file is laid out again only when its slice, the width or its fold changes.
    bodies: HashMap<(usize, usize), Body>,
    /// The id of the commit whose own diff the tab shows, or None for the whole change.
    commit: Option<String>,
    /// Whether the list of commits is open.
    commits_open: bool,
    /// How many commits the list shows.
    commits_shown: usize,
    /// What the tab read of files' unchanged lines, in the order each was first wanted.
    contexts: Vec<Context>,
    /// What the tab read of which files the reader marked viewed.
    viewed: Viewed,
    /// How many times a file's rows were laid out for `bodies`.
    #[cfg(test)]
    pub(super) builds: usize,
}

/// An unfolded file's rows, and what they were laid out for.
struct Body {
    width: usize,
    /// Whether the host withheld part of the file, which adds a note.
    withheld: bool,
    lines: Vec<Line<'static>>,
    /// While its lines don't wrap, the rows they take, which scroll sideways past the first
    /// `lead` columns, those of the numbers and the marks.
    slides: Slides,
    lead: usize,
    marks: Vec<Mark>,
    /// The rows that count each run of unchanged lines Enter shows more of, with the run's
    /// place among the file's.
    stretches: Vec<(Range<usize>, usize)>,
}

impl Body {
    /// Whether it was laid out at `width`, for a file whose changes the host withheld or not.
    fn fits(&self, width: usize, withheld: bool) -> bool {
        (self.width, self.withheld) == (width, withheld)
    }
}

/// The rows a file's lines take while they don't wrap, one each, as `Body` keeps them.
#[derive(Default)]
struct Slides {
    /// The sides of the rows where the file shows its lines side by side.
    halves: Option<Halves>,
    /// Each run of rows that are lines, in order.
    runs: Vec<Range<usize>>,
    /// The columns of the widest of those rows, where each side of one side by side counts
    /// as wide as the row would be with that side's text whole.
    widest: usize,
}

impl Slides {
    /// Adds the rows of a line, or of two side by side, to `lines`, and where they don't wrap,
    /// to the runs.
    fn add(&mut self, lines: &mut Vec<Line<'static>>, rows: Vec<Line<'static>>, wrap: bool) {
        if !wrap {
            let (start, end) = (lines.len(), lines.len() + rows.len());
            let width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
            let widest = rows.iter().map(|row| match self.halves {
                None => row.width(),
                Some(halves) => {
                    let (left, right) = row.spans.split_at(SIDE.min(row.spans.len()));
                    let right = right.get(2..).unwrap_or_default();
                    let gap = BETWEEN.width();
                    (width(left) + gap + halves.right).max(halves.left + gap + width(right))
                }
            });
            self.widest = self.widest.max(widest.max().unwrap_or_default());
            match self.runs.last_mut() {
                Some(run) if run.end == start => run.end = end,
                _ => self.runs.push(start..end),
            }
        }
        lines.extend(rows);
    }
}

/// The columns of the two sides of a row of the lines side by side, with `BETWEEN` in the
/// columns between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Halves {
    left: usize,
    right: usize,
}

impl Halves {
    /// The sides of a row `width` columns wide, the right one a column wider where the two
    /// can't be as wide, or None where the left would leave no column for its text after its
    /// mark.
    fn of(width: usize) -> Option<Halves> {
        let gap = BETWEEN.width();
        let left = width.saturating_sub(gap) / 2;
        (left >= 2).then(|| Halves {
            left,
            right: width - gap - left,
        })
    }
}

/// How a file's lines lay out: wrapped or not, and side by side or one under the other.
#[derive(Debug, Clone, Copy)]
struct Layout {
    wrap: bool,
    split: bool,
}

/// The rows of the tab's lines while they don't wrap, which scroll sideways past their numbers
/// and marks, as the desktop's numbers stay while its lines scroll (@pierre/diffs 1.5.2, whose
/// style sets `[data-overflow=scroll] [data-gutter]{position:sticky}`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Sideways {
    /// Each run of those rows, with the columns of the numbers and marks before their text,
    /// and where they show the lines side by side, the columns of the left side, in order.
    pub runs: Vec<(Range<usize>, usize, Option<usize>)>,
    /// The columns of the widest of them.
    pub widest: usize,
}

/// What the tab read of one file's text after the change, which fills the runs of unchanged
/// lines around its hunks, and how much of each run shows.
struct Context {
    /// The file's path, as `File::path` gives it.
    path: String,
    /// The fingerprint of the file's paths and hunks it was read for, as `fingerprint` gives
    /// it.
    print: u64,
    change: ChangeType,
    old_path: String,
    new_path: String,
    text: Text,
    /// The run Enter was last pressed on, which says that the read is under way or why it
    /// failed, and shows more lines once the text comes.
    pressed: usize,
    /// How many lines of each run show from its start and from its end, by its place.
    shown: HashMap<usize, (u64, u64)>,
}

/// What the tab read of which files the reader marked viewed, as the desktop's
/// `usePullRequestFilesViewed` holds it.
struct Viewed {
    /// Each file's state by path, as the last read that came named it, or None before one
    /// came. A path named twice has the state named last, as the desktop's map keeps it.
    states: Option<HashMap<String, ViewedState>>,
    /// Whether the host had more files than the last read that came covered.
    truncated: bool,
    /// Whether t3term left marks of the last read that came out past its bound.
    cut: bool,
    /// The ticket of the read under way.
    asked: Option<u64>,
    /// Set when the marks are to be read: before the first read, and once they may be out of
    /// date. A read under way then is dropped when it comes.
    again: bool,
    /// Why the last read failed, cleaned and bounded. The marks read before stay.
    failed: Option<String>,
    /// When the last read that came came, which `VIEWED_FRESH` counts from.
    at: i64,
}

/// What the pull request's other reads give the tab: the review threads and the commits the
/// conversation read lists, and who keeps the reader's marks on files viewed, as the detail's
/// `capabilities.viewedFiles` says. None there hides the marks, as the desktop does.
#[derive(Clone, Copy, Default)]
pub(super) struct Reads<'a> {
    pub threads: &'a [Thread],
    pub commits: &'a [&'a Commit],
    pub viewed: Option<ViewedFilesStore>,
}

/// A file's text after the change, as far as the tab read it.
enum Text {
    /// Enter asked for it, and no read is under way.
    Wanted,
    /// The read with this ticket is under way.
    Asked(u64),
    /// Why the read failed, cleaned and bounded.
    Failed(String),
    /// The text, with its number of lines.
    Read { text: String, count: u64 },
}

/// A run of unchanged lines around a file's hunks: before a hunk, or after the last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stretch {
    /// The number after the change of its first line.
    first: u64,
    /// The number before the change of its first line.
    old: u64,
    /// How many lines it has, or None after the last hunk until the file's text says.
    size: Option<u64>,
}

/// How a run of unchanged lines shows: the lines from its start and at its end, and the row
/// between them that counts the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Showing {
    head: u64,
    tail: u64,
    rest: Rest,
}

/// The row that counts the lines of a run that don't show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rest {
    /// None, as every line shows.
    None,
    Count(u64),
    /// Some may be left, after the last hunk while the file's text isn't read.
    Unknown,
}

/// Where a line of the diff ends among its file's rows, with its numbers before and after the
/// change, so the review threads on it show under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mark {
    old: Option<u64>,
    new: Option<u64>,
    /// The row after the line's last, and after the note that it has no line end.
    end: usize,
}

/// Where the review threads show, each by its place among them.
struct Placed {
    /// Those on a line of a file shown, by the file's place among those shown.
    on: HashMap<usize, Vec<usize>>,
    /// The rest, which show in the list of those off the diff.
    off: Vec<usize>,
}

/// A read of a slice under way.
#[derive(Clone, Copy)]
struct Asked {
    ticket: u64,
    /// Whether it reads the first slice, which replaces those shown.
    first: bool,
}

/// One slice of the diff, as T3 sent it and split into files.
struct Slice {
    diff: pull_request::Diff,
    patch: patch::Patch,
    /// The places of its files in reading order, as `order::reading_order` says.
    order: Vec<usize>,
    /// Each file's fingerprint, by its place, as `fingerprint` gives it.
    prints: Vec<u64>,
    /// Its files with the changes to whitespace hidden, by their places in `patch`.
    hidden: whitespace::Hidden,
}

impl Slice {
    /// Its files as they show, by their places: as the patch has them where `whitespace` says
    /// the changes to whitespace show, and else with those hidden.
    fn files(&self, whitespace: bool) -> &[patch::File] {
        match whitespace {
            true => &self.patch.files,
            false => &self.hidden.files,
        }
    }
}

impl Code {
    /// The tab before anything was read, which wants the first slice.
    pub(super) fn new() -> Code {
        Code {
            slices: Vec::new(),
            files: Vec::new(),
            omitted: HashMap::new(),
            asked: None,
            failed: None,
            at: 0,
            again: true,
            due: false,
            unfolded: false,
            toggled: HashSet::new(),
            whitespace: false,
            split: false,
            wrap: true,
            tree: false,
            folded: HashSet::new(),
            bodies: HashMap::new(),
            commit: None,
            commits_open: false,
            commits_shown: COMMIT_PAGE,
            contexts: Vec::new(),
            viewed: Viewed {
                states: None,
                truncated: false,
                cut: false,
                asked: None,
                again: true,
                failed: None,
                at: 0,
            },
            #[cfg(test)]
            builds: 0,
        }
    }

    /// Forgets the files' rows laid out in the old colors, after the theme changed, so each
    /// unfolded file is laid out again in the new ones.
    pub(super) fn restyle(&mut self) {
        self.bodies.clear();
    }

    /// Whether showing the tab at `now` should read the diff again: the first slice came
    /// `FRESH` or more ago, or a read of it failed, as the desktop reads a stale diff again when
    /// its tab mounts (`Atom.swr` with `revalidateOnMount` in
    /// packages/client-runtime/src/state/runtime.ts).
    pub(super) fn stale(&self, now: i64) -> bool {
        if self.asked.is_some() || self.again {
            return false;
        }
        let failed = self.failed.as_ref().is_some_and(|(_, first)| *first);
        failed || (!self.slices.is_empty() && now.saturating_sub(self.at) >= FRESH)
    }

    /// Notes that the diff may be out of date, so it is read again from its first slice, and
    /// the marks on files viewed with it, as the desktop's refresh token reads both again
    /// (PullRequestCodeTab.tsx:431-441).
    pub(super) fn again(&mut self) {
        self.again = true;
        self.viewed.again = true;
    }

    /// Whether showing the tab at `now` should read the marks on files viewed again: the last
    /// read came `VIEWED_FRESH` or more ago, or failed, as the desktop's query reads stale marks
    /// again when the tab mounts.
    pub(super) fn viewed_stale(&self, now: i64) -> bool {
        let viewed = &self.viewed;
        if viewed.asked.is_some() || viewed.again {
            return false;
        }
        viewed.failed.is_some() || now.saturating_sub(viewed.at) >= VIEWED_FRESH
    }

    /// Notes that the marks on files viewed may be out of date, so they are read again.
    pub(super) fn view_again(&mut self) {
        self.viewed.again = true;
    }

    /// Whether to read the marks on files viewed with `ticket`: they are wanted, and no read of
    /// them is under way.
    pub(super) fn ask_viewed(&mut self, ticket: u64) -> bool {
        let viewed = &mut self.viewed;
        if viewed.asked.is_some() || !viewed.again {
            return false;
        }
        viewed.again = false;
        viewed.asked = Some(ticket);
        true
    }

    /// Takes the answer to read `ticket` of the marks on files viewed, which came at `now`. A
    /// failure keeps the marks read before, as the desktop's query keeps its last answer. Says
    /// whether what the tab shows may have changed. An answer to another read, or one that came
    /// after the marks were wanted again, is dropped.
    pub(super) fn take_viewed(
        &mut self,
        ticket: u64,
        answer: Result<Box<FilesViewed>, String>,
        now: i64,
    ) -> bool {
        let viewed = &mut self.viewed;
        if viewed.asked.take_if(|asked| *asked == ticket).is_none() || viewed.again {
            return false;
        }
        match answer {
            Ok(read) => {
                let states = read.files.into_iter().map(|file| (file.path, file.state));
                viewed.states = Some(states.collect());
                viewed.truncated = read.truncated;
                viewed.cut = read.cut;
                viewed.failed = None;
                viewed.at = now;
            }
            Err(failure) => viewed.failed = Some(bounded(&failure, FAILURE)),
        }
        true
    }

    /// The state of the file at `path` with the reader, where the marks show.
    fn viewed_state(&self, store: Option<ViewedFilesStore>, path: &str) -> Option<ViewedState> {
        let states = self.viewed.states.as_ref().filter(|_| store.is_some())?;
        states.get(path).copied()
    }

    /// The cursor of the slice to read with `ticket`, or `None` inside for the first, when one
    /// is wanted and none is under way.
    pub(super) fn ask(&mut self, ticket: u64) -> Option<Option<String>> {
        if self.asked.is_some() {
            return None;
        }
        let cursor = if self.again {
            None
        } else if self.due && self.failed.is_none() {
            Some(self.next_cursor()?.to_string())
        } else {
            return None;
        };
        self.again = false;
        self.due = false;
        let first = cursor.is_none();
        self.asked = Some(Asked { ticket, first });
        Some(cursor)
    }

    /// Takes the answer to read `ticket`, which came at `now`. A first slice replaces the
    /// slices shown, and a later one follows them. A failure keeps them. Says whether what the
    /// tab shows changed, and if so whether it was a read of the first slice. An answer to
    /// another read, or one that came after the diff was wanted again, is dropped.
    pub(super) fn take(
        &mut self,
        ticket: u64,
        answer: Result<Box<pull_request::Diff>, String>,
        now: i64,
    ) -> Option<bool> {
        let asked = self.asked.take_if(|asked| asked.ticket == ticket)?;
        if self.again {
            return None;
        }
        match answer {
            Ok(diff) => {
                if asked.first {
                    self.slices.clear();
                    self.bodies.clear();
                    self.at = now;
                }
                let patch = patch::Patch::read(&diff.patch);
                let order = order::reading_order(&diff.patch, &patch);
                let prints = patch
                    .files
                    .iter()
                    .map(|file| fingerprint(&diff.patch, file))
                    .collect();
                let hidden = whitespace::hide(&diff.patch, &patch.files);
                self.slices.push(Slice {
                    diff: *diff,
                    patch,
                    order,
                    prints,
                    hidden,
                });
                self.failed = None;
                self.index();
            }
            Err(failure) => self.failed = Some((bounded(&failure, FAILURE), asked.first)),
        }
        Some(asked.first)
    }

    /// Works out the files of every slice and the counts of those the host withheld.
    fn index(&mut self) {
        self.files.clear();
        self.omitted.clear();
        for (at, slice) in self.slices.iter().enumerate() {
            self.files
                .extend(slice.order.iter().map(|&file| (at, file)));
            for omitted in &slice.diff.omitted {
                let counts = (omitted.additions, omitted.deletions);
                self.omitted.insert(omitted.path.clone(), counts);
            }
        }
        // The text read of a file whose hunks changed is read again, and that of a file the
        // diff no longer has goes once every slice came.
        let whole = self.next_cursor().is_none();
        let mut contexts = std::mem::take(&mut self.contexts);
        contexts.retain(|context| {
            let mut prints = self
                .files
                .iter()
                .map(|&(slice, place)| (&self.slices[slice], place))
                .filter(|(slice, place)| slice.patch.files[*place].path() == context.path)
                .map(|(slice, place)| slice.prints[place])
                .peekable();
            match prints.peek() {
                None => !whole,
                Some(_) => prints.any(|print| print == context.print),
            }
        });
        self.contexts = contexts;
    }

    /// The cursor of the slice after those read, while there is one.
    fn next_cursor(&self) -> Option<&str> {
        self.slices.last()?.diff.next_cursor.as_deref()
    }

    /// Whether the files read end in a row that asks for more: there is a next slice and its
    /// read didn't fail.
    pub(super) fn wants_more(&self) -> bool {
        self.next_cursor().is_some() && self.failed.is_none()
    }

    /// Notes that the end of the files came within a page of the view, so the next slice is
    /// read.
    pub(super) fn near_end(&mut self) {
        if self.wants_more() && self.asked.is_none() {
            self.due = true;
        }
    }

    /// Whether the next slice is to be read.
    pub(super) fn due(&self) -> bool {
        self.due && self.asked.is_none()
    }

    /// The id of the commit whose own diff the tab shows, or None for the whole change.
    pub(super) fn commit(&self) -> Option<&str> {
        self.commit.as_deref()
    }

    /// Whether `control` is one of the tab's and shows, with `commits` newest first in its
    /// list.
    pub(super) fn has(&self, control: Control, commits: &[&Commit]) -> bool {
        let open = self.commits_open && !commits.is_empty();
        match control {
            Control::File(at) => at < self.files.len(),
            Control::Whitespace
            | Control::Folds
            | Control::Layout
            | Control::Wrap
            | Control::Tree => !self.files.is_empty(),
            Control::Folders => self.shows_tree() && !self.tree().folders().is_empty(),
            Control::Folder(first, parts) => self.tree_rows().iter().any(|row| {
                matches!(row.kind, tree::Kind::Folder { first: f, parts: p, .. } if (f, p) == (first, parts))
            }),
            Control::TreeFile(at) => self.tree_rows().iter().any(|row| {
                matches!(row.kind, tree::Kind::File { at: a, .. } if a == at)
            }),
            Control::MoreFiles => self.shows_tree() && self.offers_more(),
            Control::Retry => self.retries(),
            Control::Commits => !commits.is_empty(),
            Control::AllCommits => open,
            Control::Commit(at) => open && at < self.listed(commits),
            Control::MoreCommits => open && commits.len() > self.listed(commits),
            Control::Stretch(at, place) => {
                self.rest(at, place).is_some_and(|rest| rest != Rest::None)
            }
            _ => false,
        }
    }

    /// The row that counts the lines left of the run at `place` around the hunks of the file
    /// at `at` among those shown, where the file is unfolded and its text can fill the run.
    fn rest(&self, at: usize, place: usize) -> Option<Rest> {
        let &(slice, index) = self.files.get(at)?;
        let slice = &self.slices[slice];
        let file = &slice.files(self.whitespace)[index];
        if !self.open(file.path()) {
            return None;
        }
        let stretch = *stretches(file)?.get(place)?;
        let context = find(&self.contexts, file.path(), slice.prints[index]);
        Some(showing(stretch, context, place).rest)
    }

    /// How many of `commits` the list shows while it is open.
    pub(super) fn listed(&self, commits: &[&Commit]) -> usize {
        commits.len().min(self.commits_shown)
    }

    /// Whether the end of the files offers Retry: the read of a later slice failed, and slices
    /// show above it with more after them.
    fn retries(&self) -> bool {
        matches!(self.failed, Some((_, false)))
            && !self.slices.is_empty()
            && self.next_cursor().is_some()
    }

    /// Does what `control` does, with `commits` newest first in the list: shows or hides the
    /// changes to whitespace, folds or unfolds a file or all of them, reads again the slice
    /// whose read failed, opens or closes the list of commits, lists more of them, or picks the
    /// diff the tab shows. Says whether that diff changed.
    pub(super) fn act(&mut self, control: Control, commits: &[&Commit]) -> bool {
        match control {
            Control::Commits => self.commits_open = !self.commits_open,
            Control::MoreCommits => self.commits_shown += COMMIT_PAGE,
            Control::AllCommits => return self.scope(None),
            Control::Commit(at) => {
                if let Some(commit) = commits.get(at) {
                    return self.scope(Some(commit.oid.clone()));
                }
            }
            Control::File(at) => {
                let Some(path) = self.file(at).map(|(_, file)| file.path().to_string()) else {
                    return false;
                };
                if !self.toggled.remove(&path) {
                    self.toggled.insert(path);
                }
            }
            // In every file, those read later too, as the desktop's toggle parses every slice
            // again (PullRequestCodeTab.tsx:384-402).
            Control::Whitespace => {
                self.whitespace = !self.whitespace;
                self.bodies.clear();
            }
            // Every file the other way, those read later too, as the desktop's toolbar sets
            // its fold override and forgets each file's own (PullRequestCodeTab.tsx:675-681).
            Control::Folds => {
                self.unfolded = self.all_folded();
                self.toggled.clear();
            }
            Control::Layout => {
                self.split = !self.split;
                self.bodies.clear();
            }
            Control::Wrap => {
                self.wrap = !self.wrap;
                self.bodies.clear();
            }
            Control::Tree => {
                self.tree = !self.tree;
                self.folded.clear();
            }
            // Every folder the other way, as the desktop's button sets each folder
            // (fileTreeExpansion.ts:44-56).
            Control::Folders => match self.folders_open() {
                true => self.folded.extend(self.tree().folders()),
                false => self.folded.clear(),
            },
            Control::Folder(first, parts) => {
                let row = self.tree_rows().into_iter().find_map(|row| match row.kind {
                    tree::Kind::Folder {
                        path,
                        first: f,
                        parts: p,
                        open,
                    } if (f, p) == (first, parts) => Some((path, open)),
                    _ => None,
                });
                match row {
                    Some((path, true)) => {
                        self.folded.insert(path);
                    }
                    // A row shows closed while any folder it joins is, and the folders above
                    // it are open, as it shows.
                    Some((path, false)) => {
                        for path in tree::ancestors(&path) {
                            self.folded.remove(path);
                        }
                    }
                    None => {}
                }
            }
            // A file folded unfolds, and one unfolded stays so.
            Control::TreeFile(at) => {
                let Some(path) = self.file(at).map(|(_, file)| file.path().to_string()) else {
                    return false;
                };
                if !self.open(&path) && !self.toggled.remove(&path) {
                    self.toggled.insert(path);
                }
            }
            Control::Retry | Control::MoreFiles => {
                if let Some((_, first)) = self.failed.take() {
                    self.again = first;
                    self.due = !first;
                } else if control == Control::MoreFiles {
                    self.near_end();
                }
            }
            Control::Stretch(at, place) => self.widen(at, place),
            _ => {}
        }
        false
    }

    /// Shows more of the run of unchanged lines at `place` around the hunks of the file at
    /// `at` among those shown, or wants the file's text read first where it isn't. While a
    /// read is under way, the run notes that it is.
    fn widen(&mut self, at: usize, place: usize) {
        let offered = self.rest(at, place).is_some_and(|rest| rest != Rest::None);
        let Some(&(slice, index)) = self.files.get(at).filter(|_| offered) else {
            return;
        };
        let slice = &self.slices[slice];
        let (file, print) = (&slice.files(self.whitespace)[index], slice.prints[index]);
        let Some(change) = change_type(file) else {
            return;
        };
        let last = file.hunks.len();
        let path = file.path().to_string();
        let found = self
            .contexts
            .iter_mut()
            .find(|context| context.path == path && context.print == print);
        match found {
            Some(context) => {
                context.pressed = place;
                match context.text {
                    Text::Read { .. } => context.widen(place, last),
                    Text::Failed(_) => context.text = Text::Wanted,
                    Text::Wanted | Text::Asked(_) => {}
                }
            }
            None => {
                let context = Context {
                    path: path.clone(),
                    print,
                    change,
                    old_path: file.old_path.clone().unwrap_or_default(),
                    new_path: file.new_path.clone().unwrap_or_default(),
                    text: Text::Wanted,
                    pressed: place,
                    shown: HashMap::new(),
                };
                if self.contexts.len() >= MOST_TEXTS {
                    let first = self.contexts.remove(0);
                    self.unlay(&first.path);
                }
                self.contexts.push(context);
            }
        }
        self.unlay(&path);
    }

    /// Forgets the rows laid out for the files at `path`, so they are laid out again.
    fn unlay(&mut self, path: &str) {
        let slices = &self.slices;
        self.bodies
            .retain(|&(slice, place), _| slices[slice].patch.files[place].path() != path);
    }

    /// The read of a file's text that Enter on one of its runs of unchanged lines wants, with
    /// `ticket`: how the file changed and its paths before and after the change.
    pub(super) fn ask_text(&mut self, ticket: u64) -> Option<(ChangeType, String, String)> {
        let context = self
            .contexts
            .iter_mut()
            .find(|context| matches!(context.text, Text::Wanted))?;
        context.text = Text::Asked(ticket);
        let paths = (context.old_path.clone(), context.new_path.clone());
        Some((context.change, paths.0, paths.1))
    }

    /// Takes the answer to read `ticket` of a file's text, or why it failed. Text that matches
    /// the file's hunks shows more of the run Enter was pressed on. Says whether what the tab
    /// shows changed. An answer to a read the tab no longer wants, as for a file the diff no
    /// longer shows, is dropped.
    pub(super) fn take_text(
        &mut self,
        ticket: u64,
        answer: Result<Box<FileContents>, String>,
    ) -> bool {
        let Some(found) = self
            .contexts
            .iter()
            .position(|context| matches!(context.text, Text::Asked(asked) if asked == ticket))
        else {
            return false;
        };
        let context = &self.contexts[found];
        let shown = self.files.iter().find_map(|&(slice, place)| {
            let slice = &self.slices[slice];
            let file = &slice.patch.files[place];
            let same = file.path() == context.path && slice.prints[place] == context.print;
            same.then_some((slice.diff.patch.as_str(), file))
        });
        let Some((patch, file)) = shown else {
            self.contexts.remove(found);
            return false;
        };
        let last = file.hunks.len();
        let text = match answer {
            Ok(contents) => match fits(&contents.new, patch, file) {
                Some(count) => Text::Read {
                    text: contents.new,
                    count,
                },
                None => Text::Failed(UNMATCHED.to_string()),
            },
            Err(failure) => Text::Failed(bounded(&failure, FAILURE)),
        };
        let context = &mut self.contexts[found];
        context.text = text;
        if matches!(context.text, Text::Read { .. }) {
            context.widen(context.pressed, last);
        }
        let path = context.path.clone();
        self.unlay(&path);
        true
    }

    /// Shows the diff of the commit `commit` names, or of the whole change, and closes the list
    /// of commits. Another diff than the one shown is read from its first slice at once, with
    /// its files folded and the list back to its first 10 commits, as the desktop's scope key
    /// starts over (PullRequestCodeTab.tsx:268-277). A read under way for the diff before is
    /// dropped when it comes. Says whether the diff changed.
    fn scope(&mut self, commit: Option<String>) -> bool {
        self.commits_open = false;
        if self.commit == commit {
            return false;
        }
        self.commit = commit;
        self.slices.clear();
        self.files.clear();
        self.omitted.clear();
        self.bodies.clear();
        self.unfolded = false;
        self.toggled.clear();
        self.folded.clear();
        self.contexts.clear();
        self.asked = None;
        self.failed = None;
        self.again = true;
        self.due = false;
        self.commits_shown = COMMIT_PAGE;
        true
    }

    /// Shows the diff of the commit `oid` names, as its row on the Timeline does. Says whether
    /// the diff changed.
    pub(super) fn show(&mut self, oid: String) -> bool {
        self.scope(Some(oid))
    }

    /// Shows the whole change's diff again where `commits`, from a read of the conversation,
    /// no longer list the commit shown, as after a rebase or a force-push. Says whether the
    /// diff changed.
    pub(super) fn follow(&mut self, commits: &[&Commit]) -> bool {
        let gone = self
            .commit
            .as_deref()
            .is_some_and(|oid| !commits.iter().any(|commit| commit.oid == oid));
        gone && self.scope(None)
    }

    /// What Enter does with `control`, for the status line, with `commits` newest first in
    /// the list.
    pub(super) fn action(&self, control: Control, commits: &[&Commit]) -> String {
        match control {
            Control::File(at) => {
                let open = self
                    .file(at)
                    .is_some_and(|(_, file)| self.open(file.path()));
                if open { "collapse file" } else { "expand file" }.to_string()
            }
            Control::Whitespace if self.whitespace => "hide whitespace changes".to_string(),
            Control::Whitespace => "show whitespace changes".to_string(),
            Control::Folds if self.all_folded() => "expand all files".to_string(),
            Control::Folds => "collapse all files".to_string(),
            Control::Layout if self.split => "show stacked diff view".to_string(),
            Control::Layout => "show split diff view".to_string(),
            Control::Wrap if self.wrap => "disable line wrapping".to_string(),
            Control::Wrap => "enable line wrapping".to_string(),
            Control::Tree if self.tree => "hide file tree".to_string(),
            Control::Tree => "show file tree".to_string(),
            Control::Folders if self.folders_open() => "collapse all folders".to_string(),
            Control::Folders => "expand all folders".to_string(),
            Control::Folder(first, parts) => {
                let open = self.tree_rows().iter().any(|row| {
                    matches!(row.kind, tree::Kind::Folder { first: f, parts: p, open: true, .. } if (f, p) == (first, parts))
                });
                if open {
                    "collapse folder"
                } else {
                    "expand folder"
                }
                .to_string()
            }
            Control::TreeFile(_) => "go to file".to_string(),
            Control::MoreFiles if self.failed.is_none() => "load more files".to_string(),
            Control::Commits if self.commits_open => "hide commits".to_string(),
            Control::Commits => "choose commit".to_string(),
            Control::AllCommits => "show all commits".to_string(),
            Control::Commit(at) => match commits.get(at) {
                Some(commit) => format!("show commit {}", commit.short),
                None => "show commit".to_string(),
            },
            Control::MoreCommits => "show more commits".to_string(),
            Control::Stretch(at, place) => match self.rest(at, place) {
                Some(Rest::Count(rest)) => {
                    let last = self.file(at).map_or(0, |(_, file)| file.hunks.len());
                    let ends = if place == 0 || place == last { 1 } else { 2 };
                    let count = rest.min(ends * EXPANSION);
                    let noun = if count == 1 { "line" } else { "lines" };
                    format!("show {} unmodified {noun}", grouped(count))
                }
                _ => "show unmodified lines".to_string(),
            },
            _ => "retry".to_string(),
        }
    }

    /// The file at `at` among those shown, with the patch its lines are ranges of.
    fn file(&self, at: usize) -> Option<(&str, &patch::File)> {
        let &(slice, file) = self.files.get(at)?;
        let slice = &self.slices[slice];
        Some((&slice.diff.patch, &slice.files(self.whitespace)[file]))
    }

    /// How `file`'s lines lay out. A file added or deleted has one side, as @pierre/diffs
    /// shows it in one column (renderers/DiffHunksRenderer.js:916-922).
    fn layout(&self, file: &patch::File) -> Layout {
        let one = matches!(file.status, patch::Status::Added | patch::Status::Deleted);
        Layout {
            wrap: self.wrap,
            split: self.split && !one,
        }
    }

    /// Whether the file at `path` shows unfolded.
    fn open(&self, path: &str) -> bool {
        self.unfolded != self.toggled.contains(path)
    }

    /// Whether every file shown is folded, as the desktop's `areAllDiffFilesCollapsed` says, so
    /// the row offers to unfold them all. Only a tab with files has the row.
    fn all_folded(&self) -> bool {
        let mut files = (0..self.files.len()).filter_map(|at| self.file(at));
        files.all(|(_, file)| !self.open(file.path()))
    }

    /// Whether the file tree shows: the reader showed it and the tab has files.
    fn shows_tree(&self) -> bool {
        self.tree && !self.files.is_empty()
    }

    /// The tree of the files shown.
    fn tree(&self) -> Tree {
        let files = (0..self.files.len()).filter_map(|at| Some((at, self.file(at)?.1)));
        Tree::new(files.map(|(at, file)| (at, file.path(), file.status)))
    }

    /// The rows of the file tree that show, or none while it doesn't.
    fn tree_rows(&self) -> Vec<tree::Row> {
        match self.shows_tree() {
            true => self.tree().rows(&self.folded),
            false => Vec::new(),
        }
    }

    /// Whether every folder of the file tree is open, so its row offers to close them all, as
    /// the desktop's `areAllDirectoriesExpanded` says. Only a tree with folders has the row. The
    /// outermost folder closed shows, as those above it are open, so the rows that show say.
    fn folders_open(&self) -> bool {
        let rows = self.tree().rows(&self.folded);
        let closed = |row: &tree::Row| matches!(row.kind, tree::Kind::Folder { open: false, .. });
        !rows.iter().any(closed)
    }

    /// Whether the end of the file tree offers to read the next slice: there is one, and no
    /// read of it is wanted or under way, or the last read failed, which it reads again even
    /// while the diff is read again from its first slice, as the files' own row does.
    fn offers_more(&self) -> bool {
        let idle = self.asked.is_none() && !self.due;
        self.next_cursor().is_some() && (self.failed.is_some() || idle)
    }

    /// Whether the tab lists files, rather than saying it has none: some slice came, and it
    /// has files or more are to be read.
    fn lists_files(&self) -> bool {
        !self.slices.is_empty() && (!self.files.is_empty() || self.next_cursor().is_some())
    }

    /// Where each of `threads` shows: under a line of each file shown with its path whose
    /// hunks have a line with its number on its side, or else in the list of those off the
    /// diff, as every thread does under a commit's own diff.
    fn place(&self, threads: &[Thread]) -> Placed {
        if self.commit.is_some() {
            let off = (0..threads.len()).collect();
            return Placed {
                on: HashMap::new(),
                off,
            };
        }
        let mut by_path: HashMap<&str, Vec<usize>> = HashMap::new();
        for at in 0..self.files.len() {
            let (_, file) = self.file(at).expect("a file shown");
            by_path.entry(file.path()).or_default().push(at);
        }
        let mut on: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut off = Vec::new();
        for (index, thread) in threads.iter().enumerate() {
            let mut placed = false;
            if let Some(line) = thread.line {
                for &at in by_path.get(thread.path.as_str()).into_iter().flatten() {
                    let (_, file) = self.file(at).expect("a file shown");
                    if has_line(file, thread.side, line) {
                        on.entry(at).or_default().push(index);
                        placed = true;
                    }
                }
            }
            if !placed {
                off.push(index);
            }
        }
        Placed { on, off }
    }

    /// The places among `threads` of those that show their remarks: those open on a line of a
    /// file unfolded, and while the list of those off the diff is open, those open in it.
    pub(super) fn threads_shown(&self, threads: &[Thread], shown: &Shown) -> Vec<usize> {
        if !self.lists_files() {
            return Vec::new();
        }
        let placed = self.place(threads);
        let mut showing = Vec::new();
        for (&at, on) in &placed.on {
            let (_, file) = self.file(at).expect("a file shown");
            if self.open(file.path()) {
                showing.extend(on);
            }
        }
        if shown.off_diff_open() {
            showing.extend(&placed.off);
        }
        showing.retain(|&index| shown.thread_open(&threads[index]));
        showing.sort_unstable();
        showing.dedup();
        showing
    }

    /// Whether `control`, one of a thread's or the list of those off the diff, shows among
    /// `threads` while the reader opened what `shown` says. A thread's `Load more comments`
    /// shows while it is open and has a page left that no read is under way for.
    pub(super) fn shows(&self, control: Control, threads: &[Thread], shown: &Shown) -> bool {
        if !self.lists_files() {
            return false;
        }
        let placed = self.place(threads);
        let visible = |index: usize| {
            let on_open = placed.on.iter().any(|(&at, on)| {
                let (_, file) = self.file(at).expect("a file shown");
                on.contains(&index) && self.open(file.path())
            });
            on_open || (shown.off_diff_open() && placed.off.contains(&index))
        };
        match control {
            Control::Thread(index) => visible(index),
            Control::More(index) => {
                visible(index)
                    && threads
                        .get(index)
                        .is_some_and(|thread| shown.thread_open(thread) && thread.offers_more())
            }
            Control::OffDiff => !placed.off.is_empty(),
            _ => false,
        }
    }

    /// Lays out at `width` the lines of each unfolded file not yet laid out for it, and forgets
    /// those of the files folded, so `lay_out` builds only the files that changed.
    pub(super) fn prepare(&mut self, width: usize, theme: &Theme) {
        if width == 0 {
            return;
        }
        let mut bodies = HashMap::new();
        for &(at, place) in &self.files {
            let slice = &self.slices[at];
            let file = &slice.files(self.whitespace)[place];
            if !self.open(file.path()) {
                continue;
            }
            let withheld = self.omitted.contains_key(file.path());
            let laid = match self.bodies.remove(&(at, place)) {
                Some(laid) if laid.fits(width, withheld) => laid,
                _ => {
                    #[cfg(test)]
                    {
                        self.builds += 1;
                    }
                    let context = find(&self.contexts, file.path(), slice.prints[place]);
                    let text = &slice.diff.patch;
                    body(
                        text,
                        file,
                        withheld,
                        context,
                        width,
                        self.layout(file),
                        theme,
                    )
                }
            };
            bodies.insert((at, place), laid);
        }
        self.bodies = bodies;
    }

    /// The tab's rows at `width`, with the rows of each control: the list of the `reads`'
    /// commits, newest first, the rows that show or hide the changes to whitespace, that
    /// unfold or fold every file and that wrap the lines, each file's header,
    /// each review thread's first row, the list of those off the diff and Retry. First the row
    /// that picks the diff from the commits, and its list while it is open. Until the first
    /// slice came, or when its read failed, it says so. Then the count of files, `+` while more
    /// are to be read, and how many are viewed where the marks show, then a note when the host
    /// withheld part of the diff, some hunks show their whitespace changes or the marks are
    /// short or failed, then the rows for the whitespace, every file and the wrapping.
    /// Then the list of the review threads off the diff, then each file with its mark and the
    /// threads on its lines, and at the end `Loading more files…`, or Retry once that failed.
    /// `shown` says which threads and whether the list are open. While the lines don't wrap,
    /// the rows they take run on past `width`, and `Sideways` says which.
    pub(super) fn lay_out(
        &self,
        reads: Reads,
        shown: &Shown,
        width: usize,
        now: i64,
        theme: &Theme,
    ) -> (Vec<Line<'static>>, Targets, Sideways) {
        let muted = Style::new().fg(theme.muted);
        let mut lines = Vec::new();
        let mut targets = Vec::new();
        let mut sideways = Sideways::default();
        if width == 0 {
            return (lines, targets, sideways);
        }
        // Above the diff while it loads too, so the list that picked a commit stays where it
        // was while that commit's diff comes (PullRequestCodeTab.tsx:1322-1335).
        let Reads {
            threads, commits, ..
        } = reads;
        self.scope_lines(commits, &mut lines, &mut targets, width, theme);
        if self.slices.is_empty() {
            let text = match &self.failed {
                Some((failure, _)) => failure.as_str(),
                None => LOADING,
            };
            lines.extend(whole(text, width, muted));
            return (lines, targets, sideways);
        }
        let count = self.files.len();
        let noun = if count == 1 { "file" } else { "files" };
        let more = if self.next_cursor().is_some() {
            "+"
        } else {
            ""
        };
        let mut parts = vec![Span::styled(
            format!("{} {noun}{more}", grouped(count as u64)),
            muted,
        )];
        // Once a read of the marks came, as the desktop's count of none before it would be
        // wrong for a reader who viewed some.
        let states = self.viewed.states.as_ref().filter(|_| count > 0);
        if let Some((store, states)) = reads.viewed.zip(states) {
            let viewed = (0..count)
                .filter_map(|at| self.file(at))
                .filter(|(_, file)| states.get(file.path()) == Some(&ViewedState::Viewed))
                .count();
            let whose = match store {
                ViewedFilesStore::Host => "viewed",
                ViewedFilesStore::Environment => "viewed in T3 Code",
            };
            let (viewed, count) = (grouped(viewed as u64), grouped(count as u64));
            parts.push(Span::styled(format!("{viewed} / {count} {whose}"), muted));
        }
        lines.extend(flowed(parts, " · ", width, theme));
        let unread = self
            .slices
            .iter()
            .any(|slice| slice.patch.files.is_empty() && !slice.diff.patch.trim().is_empty());
        if unread || self.slices.iter().any(|slice| slice.diff.truncated) {
            lines.extend(whole(WITHHELD, width, Style::new().fg(theme.warning_fg)));
        }
        // A read again of the first slice that failed leaves the files read before, which may
        // be out of date.
        if let Some((failure, true)) = &self.failed {
            lines.extend(whole(STALE, width, Style::new().fg(theme.warning_fg)));
            lines.extend(whole(failure, width, muted));
        }
        if self.slices.iter().any(|slice| slice.patch.files_cut) {
            let note = format!(
                "T3 sent more files in one slice than the {} t3term reads.",
                grouped(patch::MOST_FILES as u64)
            );
            lines.extend(whole(&note, width, muted));
        }
        let kept: usize = match self.whitespace {
            true => 0,
            false => self.slices.iter().map(|slice| slice.hidden.kept).sum(),
        };
        if kept > 0 {
            let noun = if kept == 1 { "hunk" } else { "hunks" };
            let note = format!(
                "{} {noun} too large to compare show their whitespace changes.",
                grouped(kept as u64)
            );
            lines.extend(whole(&note, width, muted));
        }
        if reads.viewed.is_some() && count > 0 {
            self.viewed_notes(&mut lines, width, theme);
        }
        if !self.lists_files() {
            lines.push(Line::default());
            let text = match (unread, &self.commit) {
                (true, _) => UNREAD,
                (false, Some(_)) => EMPTY_COMMIT,
                (false, None) => EMPTY,
            };
            lines.extend(whole(text, width, muted));
            return (lines, targets, sideways);
        }
        // The toolbar's toggle that shows or hides the changes to whitespace, its button that
        // unfolds every file, or folds them all once one shows unfolded, and its toggle that
        // wraps the lines (PullRequestCodeTab.tsx:1211-1258 and 1279-1298).
        if count > 0 {
            let fg = Style::new().fg(theme.fg);
            let label = match self.whitespace {
                true => WHITESPACE_SHOWN,
                false => WHITESPACE_HIDDEN,
            };
            let start = lines.len();
            lines.extend(whole(label, width, fg));
            targets.push((start..lines.len(), Control::Whitespace));
            let label = match self.all_folded() {
                true => FOLDED_ALL,
                false => UNFOLDED_ANY,
            };
            let start = lines.len();
            lines.extend(whole(label, width, fg));
            targets.push((start..lines.len(), Control::Folds));
            let label = match self.split {
                true => SPLIT,
                false => STACKED,
            };
            let start = lines.len();
            lines.extend(whole(label, width, fg));
            targets.push((start..lines.len(), Control::Layout));
            let label = match self.wrap {
                true => WRAPPED,
                false => UNWRAPPED,
            };
            let start = lines.len();
            lines.extend(whole(label, width, fg));
            targets.push((start..lines.len(), Control::Wrap));
            let label = match self.tree {
                true => TREE_SHOWN,
                false => TREE_HIDDEN,
            };
            let start = lines.len();
            lines.extend(whole(label, width, fg));
            targets.push((start..lines.len(), Control::Tree));
            if self.tree {
                self.tree_lines(&mut lines, &mut targets, width, theme);
            }
        }
        let placed = self.place(threads);
        // A thread, open as the reader left it, with its first row as the target that opens
        // and closes it, and `Load more comments` as the one that reads its next page.
        let card = |lines: &mut Vec<Line<'static>>, targets: &mut Targets, index: usize| {
            let thread: &Thread = &threads[index];
            let card = thread.lines(shown.thread_open(thread), width, now, theme);
            let start = lines.len();
            targets.push((start..start + card.header, Control::Thread(index)));
            if let Some(more) = card.more {
                targets.push((start + more.start..start + more.end, Control::More(index)));
            }
            lines.extend(card.lines);
        };
        if !placed.off.is_empty() {
            lines.push(Line::default());
            let start = lines.len();
            let open = shown.off_diff_open();
            let chevron = if open { "⌄" } else { "›" };
            let count = placed.off.len();
            let noun = if count == 1 {
                "conversation"
            } else {
                "conversations"
            };
            // While slices are left, a thread may be on a file not read yet.
            let diff = match self.next_cursor() {
                Some(_) => "the diff read so far",
                None => "the current diff",
            };
            let label = format!("{chevron} {} {noun} not on {diff}", grouped(count as u64));
            lines.extend(whole(&label, width, Style::new().fg(theme.fg)));
            targets.push((start..lines.len(), Control::OffDiff));
            if open {
                // A file with several shows once, where its first is.
                let mut paths: Vec<&str> = Vec::new();
                let mut by_path: HashMap<&str, Vec<usize>> = HashMap::new();
                for &index in &placed.off {
                    let path = threads[index].path.as_str();
                    by_path.entry(path).or_insert_with(|| {
                        paths.push(path);
                        Vec::new()
                    });
                    by_path.get_mut(path).expect("just added").push(index);
                }
                for path in paths {
                    lines.push(Line::default());
                    lines.extend(whole(&bounded(path, PATH), width, muted));
                    for (place, &index) in by_path[path].iter().enumerate() {
                        if place > 0 {
                            lines.push(Line::default());
                        }
                        if let Some(line) = threads[index].line {
                            lines.extend(whole(&format!("Line {line}"), width, muted));
                        }
                        card(&mut lines, &mut targets, index);
                    }
                }
            }
        }
        for at in 0..self.files.len() {
            let (text, file) = self.file(at).expect("a file shown");
            let open = self.open(file.path());
            lines.push(Line::default());
            let start = lines.len();
            let counts = match (file.additions, file.deletions) {
                (0, 0) => self.omitted.get(file.path()).copied().unwrap_or_default(),
                counts => counts,
            };
            let mark = self.viewed_state(reads.viewed, file.path());
            lines.extend(header(file, open, counts, mark, width, theme));
            targets.push((start..lines.len(), Control::File(at)));
            if open {
                let withheld = self.omitted.contains_key(file.path());
                let fresh;
                let laid = match self.bodies.get(&self.files[at]) {
                    Some(laid) if laid.fits(width, withheld) => laid,
                    _ => {
                        let (slice, place) = self.files[at];
                        let print = self.slices[slice].prints[place];
                        let context = find(&self.contexts, file.path(), print);
                        let layout = self.layout(file);
                        fresh = body(text, file, withheld, context, width, layout, theme);
                        &fresh
                    }
                };
                let marks = &laid.marks;
                sideways.widest = sideways.widest.max(laid.slides.widest);
                // The file's rows from `rows.start` to `rows.end`, with the runs among them that
                // scroll sideways where they land.
                let mut slides = laid.slides.runs.iter().cloned().peekable();
                let mut copy = |lines: &mut Vec<Line<'static>>, rows: Range<usize>| {
                    let shift = lines.len() - rows.start;
                    while let Some(run) = slides.peek_mut().filter(|run| run.start < rows.end) {
                        let end = run.end.min(rows.end);
                        let left = laid.slides.halves.map(|halves| halves.left);
                        let rows = run.start + shift..end + shift;
                        sideways.runs.push((rows, laid.lead, left));
                        match end == run.end {
                            true => _ = slides.next(),
                            false => run.start = end,
                        }
                    }
                    lines.extend(laid.lines[rows].iter().cloned());
                };
                // The threads on each line, those on its number before the change first, as
                // the desktop's stacked view puts them, then in the order T3 sent them.
                let mut on: HashMap<(Side, u64), Vec<usize>> = HashMap::new();
                for &index in placed.on.get(&at).into_iter().flatten() {
                    let thread = &threads[index];
                    let line = thread.line.expect("a thread on a line");
                    on.entry((thread.side, line)).or_default().push(index);
                }
                // The rows that count unchanged lines, each where it lands among the threads, in
                // order with their targets.
                let base = lines.len();
                let mut added = 0;
                let mut stretches = laid.stretches.iter().peekable();
                let mut counted = |targets: &mut Targets, before: usize, added: usize| {
                    while let Some((rows, place)) =
                        stretches.next_if(|(rows, _)| rows.start < before)
                    {
                        let rows = base + added + rows.start..base + added + rows.end;
                        targets.push((rows, Control::Stretch(at, *place)));
                    }
                };
                let mut from = 0;
                for mark in marks.iter().filter(|_| !on.is_empty()) {
                    let left = mark.old.and_then(|line| on.get(&(Side::Left, line)));
                    let right = mark.new.and_then(|line| on.get(&(Side::Right, line)));
                    let here: Vec<usize> =
                        left.into_iter().chain(right).flatten().copied().collect();
                    if here.is_empty() {
                        continue;
                    }
                    counted(&mut targets, mark.end, added);
                    copy(&mut lines, from..mark.end);
                    from = mark.end;
                    let before = lines.len();
                    // A blank row keeps two threads on one line apart.
                    for (place, index) in here.into_iter().enumerate() {
                        if place > 0 {
                            lines.push(Line::default());
                        }
                        card(&mut lines, &mut targets, index);
                    }
                    added += lines.len() - before;
                }
                counted(&mut targets, usize::MAX, added);
                copy(&mut lines, from..laid.lines.len());
            }
        }
        if self.next_cursor().is_some() && !self.failed.as_ref().is_some_and(|(_, first)| *first) {
            lines.push(Line::default());
            match &self.failed {
                Some((failure, _)) => {
                    lines.extend(whole(REST_FAILED, width, muted));
                    lines.extend(whole(failure, width, muted));
                    let start = lines.len();
                    lines.extend(whole("↻ Retry", width, Style::new().fg(theme.fg)));
                    targets.push((start..lines.len(), Control::Retry));
                }
                None => lines.extend(whole(LOADING_MORE, width, muted)),
            }
        }
        (lines, targets, sideways)
    }

    /// The file tree's rows, after a blank row: its count of paths, the row that closes every
    /// folder or opens them all where it has one, a row for each folder and file that shows, and
    /// while slices are left, the row that reads the next, as the desktop's footer has `Load
    /// more files`, `Loading more files...` or `Retry` (PullRequestCodeTab.tsx:1530-1549).
    fn tree_lines(
        &self,
        lines: &mut Vec<Line<'static>>,
        targets: &mut Targets,
        width: usize,
        theme: &Theme,
    ) {
        let (fg, muted) = (Style::new().fg(theme.fg), Style::new().fg(theme.muted));
        let tree = self.tree();
        lines.push(Line::default());
        let count = grouped(tree.count() as u64);
        let parts = vec![Span::styled("Files", fg), Span::styled(count, muted)];
        lines.extend(flowed(parts, " · ", width, theme));
        if !tree.folders().is_empty() {
            let label = match self.folders_open() {
                true => FOLDERS_OPEN,
                false => FOLDERS_FOLDED,
            };
            let start = lines.len();
            lines.extend(whole(label, width, fg));
            targets.push((start..lines.len(), Control::Folders));
        }
        for row in tree.rows(&self.folded) {
            let control = match row.kind {
                tree::Kind::Folder { first, parts, .. } => Control::Folder(first, parts),
                tree::Kind::File { at, .. } => Control::TreeFile(at),
            };
            let start = lines.len();
            lines.extend(tree::lines(&row, width, theme));
            targets.push((start..lines.len(), control));
        }
        if self.next_cursor().is_none() {
            return;
        }
        let start = lines.len();
        match self.offers_more() {
            true => {
                let label = if self.failed.is_some() {
                    "↻ Retry"
                } else {
                    MORE_FILES
                };
                lines.extend(whole(label, width, fg));
                targets.push((start..lines.len(), Control::MoreFiles));
            }
            false => lines.extend(whole(LOADING_MORE, width, muted)),
        }
    }

    /// The notes on the marks on files viewed: that the last read failed, with its reason, and
    /// that the host or t3term left marks out, as the desktop's warnings beside its count say.
    fn viewed_notes(&self, lines: &mut Vec<Line<'static>>, width: usize, theme: &Theme) {
        let viewed = &self.viewed;
        let warning = Style::new().fg(theme.warning_fg);
        let muted = Style::new().fg(theme.muted);
        if let Some(failure) = &viewed.failed {
            let note = match viewed.states {
                Some(_) => VIEWED_STALE,
                None => VIEWED_FAILED,
            };
            lines.extend(whole(note, width, warning));
            lines.extend(whole(failure, width, muted));
        }
        if viewed.truncated {
            lines.extend(whole(VIEWED_SHORT, width, warning));
        }
        if viewed.cut {
            let note = format!(
                "T3 sent more viewed marks than the {} t3term reads, so the count may be short.",
                grouped(pull_request::MOST_VIEWED as u64)
            );
            lines.extend(whole(&note, width, muted));
        }
    }

    /// The row that says which diff the tab shows, `All commits` or the commit's headline and
    /// the first seven characters of its id, and opens the list of `commits`, newest first.
    /// While open, the list follows: `All commits`, then each commit's headline with those
    /// seven characters at the end, ✓ before the diff shown, and `Show more (N left)` while
    /// more are left, then a blank row. Nothing shows where there are no commits, as the
    /// desktop draws no menu with nothing to pick (PullRequestCodeTab.tsx:1067-1069).
    fn scope_lines(
        &self,
        commits: &[&Commit],
        lines: &mut Vec<Line<'static>>,
        targets: &mut Targets,
        width: usize,
        theme: &Theme,
    ) {
        if commits.is_empty() {
            return;
        }
        let fg = Style::new().fg(theme.fg);
        let muted = Style::new().fg(theme.muted);
        let chevron = if self.commits_open { "⌄" } else { "›" };
        let shown = self.commit.as_deref();
        let start = lines.len();
        match commits
            .iter()
            .find(|commit| Some(commit.oid.as_str()) == shown)
        {
            Some(commit) => {
                let parts = vec![
                    Span::styled(format!("{chevron} {}", commit.headline()), fg),
                    Span::styled(commit.short.clone(), muted),
                ];
                lines.extend(flowed(parts, " · ", width, theme));
            }
            None => lines.extend(whole(&format!("{chevron} All commits"), width, fg)),
        }
        targets.push((start..lines.len(), Control::Commits));
        if !self.commits_open {
            return;
        }
        targets.push((lines.len()..lines.len() + 1, Control::AllCommits));
        lines.push(entry(shown.is_none(), "All commits", "", fg, width, theme));
        let listed = self.listed(commits);
        for (at, commit) in commits[..listed].iter().enumerate() {
            let picked = shown == Some(commit.oid.as_str());
            let (headline, short) = (commit.headline(), commit.short.as_str());
            targets.push((lines.len()..lines.len() + 1, Control::Commit(at)));
            lines.push(entry(picked, headline, short, fg, width, theme));
        }
        if commits.len() > listed {
            let left = grouped((commits.len() - listed) as u64);
            let text = format!("Show more ({left} left)");
            targets.push((lines.len()..lines.len() + 1, Control::MoreCommits));
            lines.push(entry(false, &text, "", muted, width, theme));
        }
        lines.push(Line::default());
    }
}

/// An entry of the list of commits on one row of `width`: ✓ where it is `picked`, its `text`
/// in `style` cut to fit, and `short`, a commit's id's first seven characters, at the end
/// beside at least four columns of the text.
fn entry(
    picked: bool,
    text: &str,
    short: &str,
    style: Style,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let lead = if picked { "  ✓ " } else { "    " };
    let indent = lead.width();
    if width <= indent {
        return Line::from(Span::styled(clip(text, width), style));
    }
    let id = short.width();
    let (right, room) = match id > 0 && indent + 4 + 1 + id <= width {
        true => (
            vec![Span::styled(
                short.to_string(),
                Style::new().fg(theme.muted),
            )],
            width - indent - 1 - id,
        ),
        false => (Vec::new(), width - indent),
    };
    let left = vec![
        Span::styled(lead, Style::new().fg(theme.fg)),
        Span::styled(clip(text, room), style),
    ];
    row(left, right, width)
}

/// A file's header: a chevron that says whether it is unfolded, a letter for how it changed,
/// its path, or both paths of a rename, then the lines it adds and removes unless both are 0,
/// as the desktop's `PullRequestDiffStat` leaves out, and `mark`: `✓ Viewed`, or `Changed since
/// viewed` for a file pushed to since. The path wraps under itself, and a character wider than
/// the room past the lead shows as `…`, as a line's text does.
fn header(
    file: &patch::File,
    open: bool,
    (additions, deletions): (u64, u64),
    mark: Option<ViewedState>,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let chevron = if open { "⌄" } else { "›" };
    let (letter, color) = status(file.status, theme);
    let path = |path: Option<&str>| bounded(path.unwrap_or_default(), PATH);
    let name = match file.status {
        patch::Status::Renamed => {
            format!(
                "{} → {}",
                path(file.old_path.as_deref()),
                path(file.new_path.as_deref())
            )
        }
        _ => path(Some(file.path())),
    };
    let lead = [
        Span::styled(chevron, Style::new().fg(theme.muted)),
        Span::raw(" "),
        Span::styled(letter, Style::new().fg(color)),
        Span::raw(" "),
    ];
    let indent = 4;
    let mut stat = Vec::new();
    if additions != 0 || deletions != 0 {
        stat.push(Span::styled(
            format!("+{}", grouped(additions)),
            Style::new().fg(theme.success),
        ));
        stat.push(Span::raw(" "));
        stat.push(Span::styled(
            format!("-{}", grouped(deletions)),
            Style::new().fg(theme.error_fg),
        ));
    }
    if width <= indent {
        return vec![Line::from(Span::raw(clip(
            &format!("{chevron} {letter} {name}"),
            width,
        )))];
    }
    let fg = Style::new().fg(theme.fg);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let room = width - indent;
    for (row, piece) in broken(&name, room).into_iter().enumerate() {
        let mut spans = match row {
            0 => lead.to_vec(),
            _ => vec![Span::raw(" ".repeat(indent))],
        };
        spans.push(Span::styled(clip(&piece, room), fg));
        lines.push(Line::from(spans));
    }
    // The counts, then the mark, each after the last row where it fits and on a row of its
    // own otherwise. The counts don't show where they can't fit whole, and the mark is cut.
    let mut pieces = Vec::new();
    if !stat.is_empty() {
        pieces.push((" ", stat));
    }
    let mark = match mark {
        Some(ViewedState::Viewed) => Some((VIEWED_MARK, theme.muted)),
        Some(ViewedState::Dismissed) => Some((VIEWED_CHANGED, theme.warning_fg)),
        Some(ViewedState::Unviewed) | None => None,
    };
    if let Some((mark, color)) = mark {
        let joint = if pieces.is_empty() { " " } else { " · " };
        let mark = clip(mark, width - indent);
        pieces.push((joint, vec![Span::styled(mark, Style::new().fg(color))]));
    }
    for (joint, piece) in pieces {
        let piece_width: usize = piece.iter().map(|span| span.width()).sum();
        let last = lines.last_mut().expect("a row for the path");
        if last.width() + joint.width() + piece_width <= width {
            last.spans
                .push(Span::styled(joint, Style::new().fg(theme.muted)));
            last.spans.extend(piece);
        } else if indent + piece_width <= width {
            let mut spans = vec![Span::raw(" ".repeat(indent))];
            spans.extend(piece);
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// The letter for how a change touched a file, as git's short status writes it, and its color.
fn status(status: patch::Status, theme: &Theme) -> (&'static str, Color) {
    match status {
        patch::Status::Added => ("A", theme.success),
        patch::Status::Deleted => ("D", theme.error_fg),
        patch::Status::Renamed => ("R", theme.info),
        patch::Status::Modified => ("M", theme.warning_fg),
    }
}

/// An unfolded file's rows: its hunks, each after a row that counts the unchanged lines before
/// it, and a note for each part t3term couldn't show. A file without hunks says why: it is
/// binary, the host withheld its changes, or the change touched no line, as a rename or a mode
/// change does. Where `context` holds the file's text, the unchanged lines it shows go around
/// those rows, and a row after the last hunk counts the lines after it. Where `wrap` is false,
/// each line takes one row, as long as its text.
fn body(
    text: &str,
    file: &patch::File,
    withheld: bool,
    context: Option<&Context>,
    width: usize,
    layout: Layout,
    theme: &Theme,
) -> Body {
    let Layout { wrap, split } = layout;
    let muted = Style::new().fg(theme.muted);
    let mut lines = Vec::new();
    let mut marks: Vec<Mark> = Vec::new();
    let mut counted = Vec::new();
    // A file past the bound or with a broken hunk header can have no hunks, and its notes
    // below say why.
    if file.hunks.is_empty() && !file.lines_cut && !file.unreadable {
        let note = if file.binary {
            "A binary file, whose changes have no lines to show."
        } else if withheld {
            "The host withheld this file's changes."
        } else {
            "No lines changed."
        };
        return Body {
            width,
            withheld,
            lines: whole(note, width, muted),
            slides: Slides::default(),
            lead: 0,
            marks,
            stretches: counted,
        };
    }
    // The runs of unchanged lines the file's text can fill, how each shows, and the lines of
    // the text that show, in order.
    let stretches = stretches(file).unwrap_or_default();
    let showings: Vec<Showing> = stretches
        .iter()
        .enumerate()
        .map(|(place, &stretch)| showing(stretch, context, place))
        .collect();
    let contents = match context.map(|context| &context.text) {
        Some(Text::Read { text, .. }) => text.as_str(),
        _ => "",
    };
    let mut wanted = Vec::new();
    // The largest number before the change of those lines, which only the left side shows.
    let mut old_last = 0;
    for (stretch, showing) in stretches.iter().zip(&showings) {
        let end = stretch.first + showing.head + showing.tail + rest(showing.rest);
        let before = wanted.len();
        wanted.extend(stretch.first..stretch.first + showing.head);
        wanted.extend(end - showing.tail..end);
        if let Some(&number) = wanted[before..].last() {
            old_last = old_last.max(stretch.old + (number - stretch.first));
        }
    }
    let mut found = lines_at(contents, &wanted).into_iter();
    // Side by side, each side has its own numbers and marks, as the narrower left one has room
    // for them.
    let halves = split.then(|| Halves::of(width)).flatten();
    let last = file
        .hunks
        .iter()
        .map(|hunk| {
            let old = hunk.old_start + hunk.old_count.saturating_sub(1);
            old.max(hunk.new_start + hunk.new_count.saturating_sub(1))
        })
        .chain(wanted.last().copied())
        .chain(halves.map(|_| old_last))
        .max()
        .unwrap_or_default();
    let digits = last.max(1).ilog10() as usize + 1;
    let mut slides = Slides {
        halves,
        ..Slides::default()
    };
    let side = halves.map_or(width, |halves| halves.left);
    // The number, a space, the mark and a space, while the text keeps a few columns.
    let gutter = if side >= digits + 3 + 8 {
        Some(digits)
    } else {
        None
    };
    let lead = lead(gutter, side);
    // A run of unchanged lines: those from its start, the row that counts the rest, then those
    // at its end.
    let mut stretch = |lines: &mut Vec<Line<'static>>, slides: &mut Slides, place: usize| {
        let (Some(stretch), Some(showing)) = (stretches.get(place), showings.get(place)) else {
            return;
        };
        let mut unchanged = |lines: &mut Vec<Line<'static>>, numbers: Range<u64>| {
            for number in numbers {
                let Some(text) = found.next() else {
                    return;
                };
                let line = patch::Line {
                    kind: patch::Kind::Context,
                    old: Some(stretch.old + (number - stretch.first)),
                    new: Some(number),
                    text,
                };
                let rows = match halves {
                    Some(halves) => {
                        let pair = (Some(&line), Some(&line));
                        beside(contents, pair, gutter, halves, wrap, theme)
                    }
                    None => line_rows(contents, &line, gutter, width, wrap, theme),
                };
                slides.add(lines, rows, wrap);
            }
        };
        let end = stretch.first + showing.head + showing.tail + rest(showing.rest);
        unchanged(lines, stretch.first..stretch.first + showing.head);
        let pressed = context.filter(|context| context.pressed == place);
        let mut note = match showing.rest {
            Rest::None => None,
            Rest::Count(count) => {
                let noun = if count == 1 { "line" } else { "lines" };
                Some(format!("⋯ {} unmodified {noun}", grouped(count)))
            }
            Rest::Unknown => Some(format!("⋯ {MORE_CONTEXT}")),
        };
        if let Some(note) = &mut note {
            if pressed.is_some_and(|context| matches!(context.text, Text::Wanted | Text::Asked(_)))
            {
                note.push_str(&format!(" · {READING}"));
            }
            let start = lines.len();
            lines.extend(whole(note, width, muted));
            counted.push((start..lines.len(), place));
            if let Some(Text::Failed(failure)) = pressed.map(|context| &context.text) {
                lines.extend(whole(failure, width, muted));
            }
        }
        unchanged(lines, end - showing.tail..end);
    };
    // Where the line after the last hunk is in the file before the change.
    let mut after = 1;
    for (place, hunk) in file.hunks.iter().enumerate() {
        // A hunk that removes nothing names the line before it, as git writes `-5,0`.
        let first = side_first(hunk.old_start, hunk.old_count);
        if stretches.is_empty() {
            let gap = first.saturating_sub(after);
            if gap > 0 {
                let noun = if gap == 1 { "line" } else { "lines" };
                let note = format!("⋯ {} unmodified {noun}", grouped(gap));
                lines.extend(whole(&note, width, muted));
            }
        } else {
            stretch(&mut lines, &mut slides, place);
        }
        after = first + hunk.old_count;
        let Some(halves) = halves else {
            for line in align::stacked(text, &hunk.lines) {
                let rows = line_rows(text, line, gutter, width, wrap, theme);
                slides.add(&mut lines, rows, wrap);
                let end = lines.len();
                match (line.kind, marks.last_mut()) {
                    (patch::Kind::NoNewline, Some(last)) => last.end = end,
                    (patch::Kind::NoNewline, None) => {}
                    _ => marks.push(Mark {
                        old: line.old,
                        new: line.new,
                        end,
                    }),
                }
            }
            continue;
        };
        for pair in align::pairs(text, &hunk.lines) {
            let rows = beside(text, pair, gutter, halves, wrap, theme);
            slides.add(&mut lines, rows, wrap);
            marks.push(Mark {
                old: pair.0.and_then(|line| line.old),
                new: pair.1.and_then(|line| line.new),
                end: lines.len(),
            });
        }
        // The note that a side's last line has no line end shows under the hunk's last row,
        // on that side, as @pierre/diffs puts it (renderers/DiffHunksRenderer.js:863-898).
        let (old_end, new_end) = ends(&hunk.lines);
        if old_end || new_end {
            let note = patch::Line {
                kind: patch::Kind::NoNewline,
                old: None,
                new: None,
                text: 0..0,
            };
            let pair = (old_end.then_some(&note), new_end.then_some(&note));
            let rows = beside(text, pair, gutter, halves, wrap, theme);
            slides.add(&mut lines, rows, wrap);
            if let Some(last) = marks.last_mut() {
                last.end = lines.len();
            }
        }
    }
    stretch(&mut lines, &mut slides, file.hunks.len());
    if file.lines_cut {
        let note = format!(
            "t3term reads at most {} lines of a slice, so the rest of this file doesn't show.",
            grouped(patch::MOST_LINES as u64)
        );
        lines.extend(whole(&note, width, muted));
    }
    if file.unreadable {
        lines.extend(whole(
            "t3term couldn't read some lines of this file's patch.",
            width,
            muted,
        ));
    }
    Body {
        width,
        withheld,
        lines,
        slides,
        lead,
        marks,
        stretches: counted,
    }
}

/// A line of a hunk, or none, on each side of a row of the lines side by side.
type Pair<'a> = (Option<&'a patch::Line>, Option<&'a patch::Line>);

/// Whether the last line of a hunk's side before the change, and of its side after, has no
/// line end, as @pierre/diffs reads the note: after a line in context it is both sides', and
/// after a line removed or added that side's (utils/parsePatchFiles.js:225-230).
fn ends(lines: &[patch::Line]) -> (bool, bool) {
    let (mut old, mut new) = (false, false);
    let mut before = None;
    for line in lines {
        match (line.kind, before) {
            (patch::Kind::NoNewline, Some(patch::Kind::Context)) => (old, new) = (true, true),
            (patch::Kind::NoNewline, Some(patch::Kind::Removed)) => old = true,
            (patch::Kind::NoNewline, Some(patch::Kind::Added)) => new = true,
            (patch::Kind::NoNewline, _) => {}
            (kind, _) => before = Some(kind),
        }
    }
    (old, new)
}

/// The rows of a pair of lines side by side in `halves`, for as many as the taller side takes:
/// each line in its side with its number on that side, its mark and its text, as `line_rows`
/// lays it out, and a side with no line blank. Where the lines don't wrap, each side keeps the
/// whole of its text, so `Sideways` scrolls each within its columns.
fn beside(
    text: &str,
    (old, new): Pair,
    gutter: Option<usize>,
    halves: Halves,
    wrap: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let side = |line: Option<&patch::Line>, before: bool, width: usize| {
        let Some(line) = line else {
            return Vec::new();
        };
        // `line_rows` shows the number after the change where a line has one, so the left
        // side drops it to show the number before.
        let line = patch::Line {
            new: line.new.filter(|_| !before),
            ..line.clone()
        };
        line_rows(text, &line, gutter, width, wrap, theme)
    };
    let left = side(old, true, halves.left);
    let right = side(new, false, halves.right);
    let count = left.len().max(right.len());
    let (mut left, mut right) = (left.into_iter(), right.into_iter());
    let between = Style::new().fg(theme.muted);
    (0..count)
        .map(|_| {
            let mut spans = left.next().map(|row| row.spans).unwrap_or_default();
            while spans.len() < SIDE {
                spans.insert(0, Span::raw(""));
            }
            let used: usize = spans.iter().map(Span::width).sum();
            spans.push(Span::raw(" ".repeat(halves.left.saturating_sub(used))));
            spans.push(Span::styled(BETWEEN, between));
            spans.extend(right.next().map(|row| row.spans).unwrap_or_default());
            Line::from(spans)
        })
        .collect()
}

/// The lines a row counts as left.
fn rest(rest: Rest) -> u64 {
    match rest {
        Rest::Count(count) => count,
        Rest::None | Rest::Unknown => 0,
    }
}

/// The number of the first line a hunk's side covers, from its header's `start` and `count`.
/// A side with no lines names the line before it, as git writes `-5,0`.
fn side_first(start: u64, count: u64) -> u64 {
    match count {
        0 => start.saturating_add(1),
        _ => start,
    }
}

/// How `file` changed, as T3 names it for a read of its text, where that text can fill the
/// runs of unchanged lines around its hunks: a file changed in place, or renamed and changed,
/// as the desktop's `canHydrateCollapsedContext` asks, whose hunks t3term read whole.
fn change_type(file: &patch::File) -> Option<ChangeType> {
    let readable = !file.hunks.is_empty() && !file.binary && !file.lines_cut && !file.unreadable;
    if !readable || file.old_path.is_none() || file.new_path.is_none() {
        return None;
    }
    match file.status {
        patch::Status::Modified => Some(ChangeType::Change),
        patch::Status::Renamed => Some(ChangeType::RenameChanged),
        patch::Status::Added | patch::Status::Deleted => None,
    }
}

/// The runs of unchanged lines around `file`'s hunks that its text can fill, by place: one
/// before each hunk, empty where the hunk follows the last at once, then one after the last.
/// None where `change_type` says the text can't fill them, or where the hunks' headers don't
/// leave as many lines between them on both sides, which unchanged lines would.
fn stretches(file: &patch::File) -> Option<Vec<Stretch>> {
    change_type(file)?;
    let mut stretches = Vec::with_capacity(file.hunks.len() + 1);
    let (mut old_after, mut new_after) = (1u64, 1u64);
    for hunk in &file.hunks {
        let old_first = side_first(hunk.old_start, hunk.old_count);
        let new_first = side_first(hunk.new_start, hunk.new_count);
        let size = new_first.checked_sub(new_after)?;
        if old_first.checked_sub(old_after)? != size {
            return None;
        }
        stretches.push(Stretch {
            first: new_after,
            old: old_after,
            size: Some(size),
        });
        old_after = old_first.saturating_add(hunk.old_count);
        new_after = new_first.saturating_add(hunk.new_count);
    }
    stretches.push(Stretch {
        first: new_after,
        old: old_after,
        size: None,
    });
    Some(stretches)
}

/// How the run `stretch` at `place` shows with what `context` read of its file. Until the
/// text came, no line of it shows. After, a run of one line shows whole, as the desktop's
/// `collapsedContextThreshold` of 1 has it, and the rest shows as far as Enter widened it.
fn showing(stretch: Stretch, context: Option<&Context>, place: usize) -> Showing {
    let read = context.and_then(|context| match context.text {
        Text::Read { count, .. } => Some((context, count)),
        _ => None,
    });
    let Some((context, count)) = read else {
        let rest = match stretch.size {
            Some(0) => Rest::None,
            Some(size) => Rest::Count(size),
            None => Rest::Unknown,
        };
        return Showing {
            head: 0,
            tail: 0,
            rest,
        };
    };
    let size = stretch
        .size
        .unwrap_or_else(|| count.saturating_add(1).saturating_sub(stretch.first));
    if size <= 1 {
        return Showing {
            head: size,
            tail: 0,
            rest: Rest::None,
        };
    }
    let (from_start, from_end) = context.shown.get(&place).copied().unwrap_or_default();
    let head = from_start.min(size);
    let tail = from_end.min(size - head);
    let rest = match size - head - tail {
        0 => Rest::None,
        rest => Rest::Count(rest),
    };
    Showing { head, tail, rest }
}

impl Context {
    /// Shows up to `EXPANSION` more lines of the run at `place` at each end that touches a
    /// hunk, as the desktop's buttons do: at its end before the first hunk, at its start after
    /// the last at `last`, and at both between two.
    fn widen(&mut self, place: usize, last: usize) {
        let (from_start, from_end) = self.shown.entry(place).or_default();
        if place != 0 {
            *from_start = from_start.saturating_add(EXPANSION);
        }
        if place != last {
            *from_end = from_end.saturating_add(EXPANSION);
        }
    }
}

/// What `contexts` read of the file at `path` whose fingerprint is `print`.
fn find<'a>(contexts: &'a [Context], path: &str, print: u64) -> Option<&'a Context> {
    contexts
        .iter()
        .find(|context| context.path == path && context.print == print)
}

/// A fingerprint of `file`'s paths and of its hunks in `patch`, so the text read for it stays
/// only while a read of the diff again leaves them as they were.
fn fingerprint(patch: &str, file: &patch::File) -> u64 {
    let mut hasher = DefaultHasher::new();
    (&file.old_path, &file.new_path).hash(&mut hasher);
    for hunk in &file.hunks {
        let header = (
            hunk.old_start,
            hunk.old_count,
            hunk.new_start,
            hunk.new_count,
        );
        header.hash(&mut hasher);
        for line in &hunk.lines {
            (line.kind, patch.get(line.text.clone())).hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// How many lines `text`, a file's text after the change, has, where each line `file`'s hunks
/// in `patch` have after the change is in it with its number, as when both come from one
/// revision. None where one isn't.
fn fits(text: &str, patch: &str, file: &patch::File) -> Option<u64> {
    let mut lines = file
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| matches!(line.kind, patch::Kind::Context | patch::Kind::Added))
        .filter_map(|line| Some((line.new?, patch.get(line.text.clone())?)))
        .peekable();
    let mut count = 0;
    for (number, line) in (1u64..).zip(text.split_inclusive('\n')) {
        count = number;
        if let Some((_, expected)) = lines.next_if(|&(at, _)| at == number)
            && without_end(line) != expected
        {
            return None;
        }
    }
    // A text too short for a hunk's lines ends before the line after the last hunk.
    let reach = file
        .hunks
        .iter()
        .map(|hunk| side_first(hunk.new_start, hunk.new_count).saturating_add(hunk.new_count))
        .max()
        .unwrap_or(1);
    (reach.saturating_sub(1) <= count).then_some(count)
}

/// The ranges in `text` of its lines numbered `wanted`, from 1 and in increasing order,
/// without their ends.
fn lines_at(text: &str, wanted: &[u64]) -> Vec<Range<usize>> {
    let mut found = Vec::with_capacity(wanted.len());
    let mut wanted = wanted.iter().peekable();
    let mut at = 0;
    for (number, line) in (1u64..).zip(text.split_inclusive('\n')) {
        if wanted.peek().is_none() {
            break;
        }
        if wanted.next_if(|&&wanted| wanted == number).is_some() {
            found.push(at..at + without_end(line).len());
        }
        at += line.len();
    }
    found
}

/// A line of a file without its end, as the patch's lines are read: `\n`, then a `\r`
/// before it.
fn without_end(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// Whether a line of `file`'s hunks has the number `line` on `side`. The desktop's
/// `isLineInFileDiff` asks only whether a hunk's header counts it, and a line in a hunk cut
/// short has none to show under, so its thread goes to the list of those off the diff. Only
/// the hunks whose headers count it are searched.
fn has_line(file: &patch::File, side: Side, line: u64) -> bool {
    file.hunks.iter().any(|hunk| {
        let (start, count) = match side {
            Side::Left => (hunk.old_start, hunk.old_count),
            Side::Right => (hunk.new_start, hunk.new_count),
        };
        (start..start.saturating_add(count)).contains(&line)
            && hunk.lines.iter().any(|read| match side {
                Side::Left => read.old == Some(line),
                Side::Right => read.new == Some(line),
            })
    })
}

/// The columns before a line's text at `width`: its number in `gutter` columns, a space, its
/// mark and a space where there is room for a number, else its mark alone.
fn lead(gutter: Option<usize>, width: usize) -> usize {
    match gutter {
        Some(digits) => digits + 3,
        // A column of its own leaves none for the text.
        None if width > 1 => 1,
        None => 0,
    }
}

/// A line of a hunk on as many rows as its text takes at `width`: its number in `digits`
/// columns where there is room for one, its mark and its text, in the color of what happened to
/// it. The rows after the first leave the number and the mark blank. Where `wrap` is false, it
/// takes one row, as long as its text.
fn line_rows(
    text: &str,
    line: &patch::Line,
    gutter: Option<usize>,
    width: usize,
    wrap: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme.muted);
    let (mark, style) = match line.kind {
        patch::Kind::Added => ("+", Style::new().fg(theme.success)),
        patch::Kind::Removed => ("-", Style::new().fg(theme.error_fg)),
        patch::Kind::Context => (" ", Style::new().fg(theme.fg)),
        patch::Kind::NoNewline => (" ", muted),
    };
    let shown = match line.kind {
        patch::Kind::NoNewline => "No newline at end of file".to_string(),
        _ => shown(text.get(line.text.clone()).unwrap_or_default()),
    };
    let number = match line.kind {
        patch::Kind::Removed => line.old,
        patch::Kind::NoNewline => None,
        _ => line.new.or(line.old),
    };
    let lead = lead(gutter, width);
    let room = width.saturating_sub(lead).max(1);
    let pieces = match wrap {
        true => wrapped(&shown, room),
        false => vec![shown],
    };
    let mut rows = Vec::new();
    for (at, piece) in pieces.into_iter().enumerate() {
        let mut spans = Vec::with_capacity(3);
        match (gutter, at) {
            (Some(digits), 0) => {
                let number = number.map(|n| n.to_string()).unwrap_or_default();
                spans.push(Span::styled(format!("{number:>digits$} "), muted));
                spans.push(Span::styled(format!("{mark} "), style));
            }
            (None, 0) if lead > 0 => spans.push(Span::styled(mark, style)),
            _ => spans.push(Span::raw(" ".repeat(lead))),
        }
        let piece = match wrap {
            true => clip(&piece, room),
            false => piece,
        };
        spans.push(Span::styled(piece, style));
        rows.push(Line::from(spans));
    }
    rows
}

/// A line of the diff as it shows: its tabs set every `TAB` columns, without control
/// characters, and cut with `…` past `LINE` characters.
fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(LINE * 4));
    let mut column = 0;
    for (count, c) in text.chars().enumerate() {
        if count == LINE {
            out.push('…');
            break;
        }
        match c {
            '\t' => {
                let spaces = TAB - column % TAB;
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            c if c.is_control() => {}
            c => {
                out.push(c);
                column += c.width().unwrap_or(0);
            }
        }
    }
    out
}

/// `text` in rows of at most `width` columns, as the desktop wraps a line of the diff with
/// `white-space: pre-wrap` and `word-break: break-word` (@pierre/diffs 1.5.2, dist/style.js). A
/// row ends only where the Unicode line breaking rules allow, as textwrap finds those places
/// with ICU's rules, such as after a run of spaces or between two CJK characters. Every space
/// stays, and those that end a row take no room, as a browser lets them hang past the edge, so
/// the row is cut where it shows. A word wider than a row starts one and breaks between any two
/// characters, as `broken` breaks it. An empty text takes one row.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    if columns(text) <= width {
        return vec![text.to_string()];
    }
    let mut rows = vec![String::new()];
    let mut used = 0;
    for word in WordSeparator::UnicodeBreakProperties.find_words(text) {
        let taken = columns(word.word);
        if used > 0 && used + taken > width {
            rows.push(String::new());
            used = 0;
        }
        if taken > width {
            // The row is empty, as nothing can come before a word wider than it.
            rows.pop();
            rows.extend(broken(word.word, width));
            used = columns(rows.last().map(String::as_str).unwrap_or_default());
        } else {
            rows.last_mut().expect("a row").push_str(word.word);
            used += taken;
        }
        // The spaces past the row's end hang unseen, so it keeps only those that fit, though
        // each takes its column.
        let fit = width.saturating_sub(used).min(word.whitespace.len());
        rows.last_mut()
            .expect("a row")
            .push_str(&word.whitespace[..fit]);
        used += word.whitespace.len();
    }
    rows
}

/// The columns `text` takes, each piece `clusters` finds measured whole. Text that is all
/// ASCII has no control characters here, so each of its characters takes one.
fn columns(text: &str) -> usize {
    match text.is_ascii() {
        true => text.len(),
        false => clusters(text).iter().map(|piece| piece.width()).sum(),
    }
}

/// `text` broken into rows of at most `width` columns, between any two characters, so its
/// spaces all stay. An empty text takes one row.
fn broken(text: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut used = 0;
    for piece in clusters(text) {
        let columns = piece.width();
        if used > 0 && used + columns > width {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().expect("a row").push_str(piece);
        used += columns;
    }
    rows
}

mod align;
mod order;
mod tree;
mod whitespace;

#[cfg(test)]
mod tests;
