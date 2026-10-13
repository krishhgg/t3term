//! The pull requests linked to the open thread, in the details' second tab. Each shows its
//! number, title and state, and the checks, review and line counts the host reported, with
//! the repository and its branches under them. Enter or a second click opens a link's
//! details, with its whole URL as plain text. t3term opens no browser and changes nothing:
//! the list only reads.
//!
//! The links come from the thread's record that the details already hold, as the desktop's
//! `ThreadPullRequestsPanel` (apps/web/src/components/pullRequest/ThreadPullRequestsPanel.tsx
//! in the nightly) reads them from the thread's shell. The record's `pullRequests` holds them
//! (`ThreadPullRequestLink` in packages/contracts/src/threadPullRequest.ts). A record without
//! that list falls back to its one legacy `linkedPullRequest`, as `threadPullRequestsOf` in
//! packages/shared/src/threadPullRequests.ts does for older payloads, and an explicit empty
//! list means every link was removed. Links whose source is `stack-dismissed` are tombstones
//! the desktop hides, and so does this list. A host fills a link's `snapshot` in when it
//! syncs, so until then the link waits for its host state, and each optional field the host
//! left out shows as nothing rather than as a guess. A list on an environment whose
//! capabilities lack `threadPullRequests` reads unavailable, as the desktop's does.
//!
//! The links are worked out from the record only after an event that can change it, while
//! the details are open: a few strings of each, bounded and cleaned once. A frame lays out
//! only the links it shows, and lays them out again only when the width, the height, the
//! selection or the minute changes.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::details::Record;
use super::label::cut;
use super::setup::{bounded, middle};
use super::tasks::wrap;
use super::theme::{Theme, parse_iso_ms, relative_time};
use super::{clip, row, with_bg};

/// The most links read from a thread's record. The rest are counted and left unread.
const MOST_LINKS: usize = 1000;
/// The most characters kept of a title.
const TITLE: usize = 512;
/// The most characters kept of a host, a repository, a branch or an author's login.
const NAME: usize = 256;
/// The most characters kept of a URL.
const URL: usize = 2048;
/// The longest text, in bytes, that links, stacks and branches are matched by. A link whose
/// host or repository is longer is unreadable, and a longer branch or stack id matches none.
const KEY: usize = 1024;
/// The longest timestamp read, in bytes.
const STAMP: usize = 64;
/// The rows each link takes in the list.
const ENTRY_ROWS: usize = 3;
/// The most rows a selected link's title, its other values and its URL wrap to.
const TITLE_ROWS: usize = 4;
const VALUE_ROWS: usize = 3;
const URL_ROWS: usize = 8;
/// What a GitHub stack does, as the desktop's tooltip says it.
const NATIVE_STACK: &str = "merging a layer lands the ones below it.";
/// What watching a pull request does, as the desktop's tooltip says it.
const WATCHING: &str =
    "Watching: the agent wakes when checks finish, someone comments, or the branch conflicts";

/// A link's state as the host last reported it. An open draft is a draft, and a closed or
/// merged pull request is never one, as the desktop's `resolvePullRequestState` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Checks {
    Passing,
    Failing,
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Review {
    Approved,
    ChangesRequested,
    Required,
}

/// Who linked the pull request, as `ThreadPullRequestLinkSource` names it. A source this
/// version doesn't know yet is `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Manual,
    Created,
    Agent,
    Stack,
    Other,
}

impl Source {
    /// The desktop's `SOURCE_LABELS`.
    fn label(self) -> &'static str {
        match self {
            Source::Manual => "Linked by you",
            Source::Created => "Created from this thread",
            Source::Agent => "Linked by the agent",
            Source::Stack => "Found in the stack",
            Source::Other => "Linked",
        }
    }
}

/// A link's identity: its host and repository, trimmed and lowercased, and its number, as
/// `threadPullRequestKeyOf` compares links.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    host: String,
    repository: String,
    number: u64,
}

impl Key {
    fn of(host: &str, repository: &str, number: u64) -> Option<Key> {
        let readable = |text: &str| !text.is_empty() && text.len() <= KEY;
        let (host, repository) = (host.trim(), repository.trim());
        if !readable(host) || !readable(repository) {
            return None;
        }
        Some(Key {
            host: host.to_lowercase(),
            repository: repository.to_lowercase(),
            number,
        })
    }
}

/// What the host reported when it last synced a link, cleaned and bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    state: State,
    /// Open, not a draft, and the host says it conflicts with its base, which is when the
    /// desktop's `resolvePullRequestConflict` shows it.
    conflicting: bool,
    title: String,
    head: String,
    base: String,
    /// The branches as T3 sent them, to chain links by. None when longer than `KEY`.
    head_key: Option<String>,
    base_key: Option<String>,
    updated: Option<String>,
    synced: Option<String>,
    author: Option<String>,
    bot: bool,
    additions: Option<u64>,
    deletions: Option<u64>,
    checks: Option<Checks>,
    review: Option<Review>,
}

/// Whether the host has synced a link.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Synced {
    /// Its snapshot is null: the host hasn't synced it yet.
    Waiting,
    Read(Box<Snapshot>),
    /// Its snapshot is there, and t3term can't read it.
    Unreadable,
}

/// How a link sits in a stack of two or more, noted on its bottom layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Chain {
    /// A stack the host keeps, rather than one worked out from branches.
    native: bool,
    size: usize,
}

/// One link, cleaned and bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    key: Key,
    host: String,
    repository: String,
    number: u64,
    url: String,
    source: Source,
    linked_at: Option<String>,
    synced: Synced,
    watching: bool,
    /// How many layers of its stack lie under it.
    depth: usize,
    chain: Option<Chain>,
}

impl Entry {
    fn snapshot(&self) -> Option<&Snapshot> {
        match &self.synced {
            Synced::Read(snapshot) => Some(snapshot),
            Synced::Waiting | Synced::Unreadable => None,
        }
    }

    /// Whether the link counts as open. One the host hasn't synced does, as the desktop's
    /// footer counts it, and so does one t3term can't read.
    fn open(&self) -> bool {
        self.snapshot()
            .is_none_or(|snapshot| matches!(snapshot.state, State::Open | State::Draft))
    }

    /// The title the host reported, or the repository until it has.
    fn title(&self) -> &str {
        match self.snapshot() {
            Some(snapshot) if !snapshot.title.is_empty() => &snapshot.title,
            _ => &self.repository,
        }
    }
}

/// The open thread's links, worked out from its record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct List {
    /// The open thread these are of. None when no thread is open.
    thread_id: Option<String>,
    /// Whether the environment links pull requests to threads.
    supported: bool,
    body: Body,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Body {
    /// No thread is open.
    None,
    Loading,
    Unavailable,
    /// The environment's capabilities lack `threadPullRequests`.
    Unsupported,
    /// The record's `pullRequests` isn't a list.
    Unreadable,
    Links(Links),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Links {
    /// Stack by stack, the newest first, each from its bottom layer up.
    entries: Vec<Entry>,
    /// Links the record held that t3term couldn't read.
    skipped: usize,
    /// Links past the first `MOST_LINKS`, never read.
    unread: usize,
    open: usize,
    /// The latest time the host synced one of them.
    synced: Option<String>,
}

impl List {
    /// The links of thread `open_id`, the open one, from its `record`. `supported` says
    /// whether the environment links pull requests to threads.
    pub(super) fn of(open_id: Option<&str>, record: Record, supported: bool) -> List {
        let body = match (open_id, record) {
            (None, _) | (_, Record::None) => Body::None,
            _ if !supported => Body::Unsupported,
            (_, Record::Loading) => Body::Loading,
            (_, Record::Unavailable) => Body::Unavailable,
            (_, Record::Found(thread)) => links_of(thread),
        };
        List {
            thread_id: open_id.map(str::to_string),
            supported,
            body,
        }
    }

    /// How many links show, once they are known.
    fn count(&self) -> Option<usize> {
        match &self.body {
            Body::Links(links) => Some(links.entries.len()),
            _ => None,
        }
    }
}

/// A link as one read of the record finds it, before it is placed in a stack.
struct Raw {
    entry: Entry,
    /// Its host-native stack's id and the numbers of the stack's layers, bottom to top.
    stack: Option<(String, Vec<u64>)>,
    /// When it last changed, for the order of the stacks.
    activity: i64,
}

enum Parsed {
    Shown(Box<Raw>),
    Dismissed,
}

/// The links of `thread`'s record: its `pullRequests`, or else its legacy
/// `linkedPullRequest`, without tombstones, each identity once, in stacks.
fn links_of(thread: &Value) -> Body {
    let mut raws = Vec::new();
    let mut skipped = 0;
    let mut unread = 0;
    match thread.get("pullRequests") {
        Some(Value::Array(links)) => {
            unread = links.len().saturating_sub(MOST_LINKS);
            for link in links.iter().take(MOST_LINKS) {
                match link_of(link) {
                    Some(Parsed::Shown(raw)) => raws.push(*raw),
                    Some(Parsed::Dismissed) => {}
                    None => skipped += 1,
                }
            }
        }
        Some(_) => return Body::Unreadable,
        None => match thread.get("linkedPullRequest") {
            None | Some(Value::Null) => {}
            Some(linked) => match legacy_of(linked) {
                Some(raw) => raws.push(raw),
                None => skipped += 1,
            },
        },
    }
    // The first of the links with one identity stands for them all.
    let mut seen = HashSet::new();
    raws.retain(|raw| seen.insert(raw.entry.key.clone()));
    let entries = chained(raws);
    let open = entries.iter().filter(|entry| entry.open()).count();
    let synced = entries
        .iter()
        .filter_map(|entry| entry.snapshot()?.synced.as_deref())
        .max_by_key(|at| parse_iso_ms(at).unwrap_or(0))
        .map(str::to_string);
    Body::Links(Links {
        entries,
        skipped,
        unread,
        open,
        synced,
    })
}

/// A pull request's number, when it is a positive integer.
fn number_of(value: &Value) -> Option<u64> {
    value.get("number")?.as_u64().filter(|&number| number > 0)
}

/// One of the record's `pullRequests`, a `ThreadPullRequestLink`. None when it lacks a field
/// the contract requires.
fn link_of(link: &Value) -> Option<Parsed> {
    let source = match link.get("source")?.as_str()? {
        "stack-dismissed" => return Some(Parsed::Dismissed),
        "manual" => Source::Manual,
        "created" => Source::Created,
        "agent" => Source::Agent,
        "stack" => Source::Stack,
        _ => Source::Other,
    };
    let host = link.get("host")?.as_str()?;
    let repository = link.get("repository")?.as_str()?;
    let number = number_of(link)?;
    let url = link.get("url")?.as_str()?;
    let key = Key::of(host, repository, number)?;
    let synced = match link.get("snapshot") {
        None | Some(Value::Null) => Synced::Waiting,
        Some(snapshot) => match snapshot_of(snapshot) {
            Some(snapshot) => Synced::Read(Box::new(snapshot)),
            None => Synced::Unreadable,
        },
    };
    let linked_at = stamp(link.get("linkedAt"));
    // The desktop sorts by `snapshot.updatedAt ?? linkedAt`, and a time it can't read sorts as
    // the epoch.
    let updated = match &synced {
        Synced::Read(snapshot) => snapshot.updated.as_deref(),
        Synced::Waiting | Synced::Unreadable => None,
    };
    let at = updated.or(linked_at.as_deref());
    let activity = at.and_then(parse_iso_ms).unwrap_or(0);
    let entry = Entry {
        host: bounded(host, NAME),
        repository: bounded(repository, NAME),
        key,
        number,
        url: bounded(url, URL),
        source,
        linked_at,
        synced,
        watching: link.get("watch").is_some_and(Value::is_object),
        depth: 0,
        chain: None,
    };
    let raw = Raw {
        entry,
        stack: stack_of(link.get("stack")),
        activity,
    };
    Some(Parsed::Shown(Box::new(raw)))
}

/// The legacy `linkedPullRequest`, a `ThreadLinkedPullRequest`, as `threadPullRequestsOf`
/// turns it into a link: linked by you, never synced, its host read from its URL and its
/// repository lowercased.
fn legacy_of(linked: &Value) -> Option<Raw> {
    let repository = linked.get("repository")?.as_str()?;
    let number = number_of(linked)?;
    let url = linked.get("url")?.as_str()?;
    let key = Key::of(&host_of(url), repository, number)?;
    let entry = Entry {
        host: bounded(&key.host, NAME),
        repository: bounded(&key.repository, NAME),
        number,
        url: bounded(url, URL),
        source: Source::Manual,
        // T3 stamps the link with the epoch, which says nothing worth showing.
        linked_at: None,
        synced: Synced::Waiting,
        watching: false,
        depth: 0,
        chain: None,
        key,
    };
    Some(Raw {
        entry,
        stack: None,
        activity: 0,
    })
}

/// The host of `url`, as `legacyThreadPullRequestKey` reads it with `new URL(url).hostname`
/// for the URLs hosts send: what follows `scheme://` up to a path, a query or a fragment,
/// without user info or a port, lowercased. A URL without a scheme or a host reads
/// `unknown`, as one `new URL` refuses does. The desktop also recovers a Forgejo port or an
/// Azure path from the URL, which this doesn't.
fn host_of(url: &str) -> String {
    let unknown = || "unknown".to_string();
    // A host longer than `KEY` reads unknown, so a URL is read no further than twice that.
    let url = match url.char_indices().nth(2 * KEY) {
        Some((at, _)) => &url[..at],
        None => url,
    };
    let Some((scheme, rest)) = url.trim().split_once("://") else {
        return unknown();
    };
    if !is_scheme(scheme) {
        return unknown();
    }
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or("");
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match authority.find(']') {
        Some(end) if authority.starts_with('[') => &authority[..=end],
        _ => authority.split(':').next().unwrap_or(""),
    };
    let host = host.trim();
    if host.is_empty() || host.len() > KEY {
        unknown()
    } else {
        host.to_lowercase()
    }
}

/// Whether `scheme` can start a URL: a letter, then letters, digits, `+`, `-` or `.`.
fn is_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
}

/// A link's `snapshot`, a `ThreadPullRequestSnapshot`. None when it lacks a field the
/// contract requires. The optional fields left out, null or not of the contract's values
/// read as unknown.
fn snapshot_of(snapshot: &Value) -> Option<Snapshot> {
    let draft = snapshot.get("isDraft")?.as_bool()?;
    let state = match snapshot.get("state")?.as_str()? {
        "open" if draft => State::Draft,
        "open" => State::Open,
        "merged" => State::Merged,
        "closed" => State::Closed,
        _ => return None,
    };
    let title = snapshot.get("title")?.as_str()?;
    let head = snapshot.get("headBranch")?.as_str()?;
    let base = snapshot.get("baseBranch")?.as_str()?;
    let text = |key: &str| snapshot.get(key).and_then(Value::as_str);
    let checks = match text("checksState") {
        Some("passing") => Some(Checks::Passing),
        Some("failing") => Some(Checks::Failing),
        Some("pending") => Some(Checks::Pending),
        _ => None,
    };
    let review = match text("reviewDecision") {
        Some("approved") => Some(Review::Approved),
        Some("changes-requested") => Some(Review::ChangesRequested),
        Some("review-required") => Some(Review::Required),
        _ => None,
    };
    let author = snapshot
        .pointer("/author/login")
        .and_then(Value::as_str)
        .map(|login| bounded(login, NAME))
        .filter(|login| !login.is_empty());
    let count = |key: &str| snapshot.get(key).and_then(Value::as_u64);
    let key = |branch: &str| (branch.len() <= KEY).then(|| branch.to_string());
    Some(Snapshot {
        state,
        conflicting: state == State::Open && text("mergeability") == Some("conflicting"),
        title: bounded(title, TITLE),
        head: bounded(head, NAME),
        base: bounded(base, NAME),
        head_key: key(head),
        base_key: key(base),
        updated: stamp(snapshot.get("updatedAt")),
        synced: stamp(snapshot.get("syncedAt")),
        author,
        bot: snapshot.pointer("/author/isBot") == Some(&Value::Bool(true)),
        additions: count("additions"),
        deletions: count("deletions"),
        checks,
        review,
    })
}

/// A link's host-native `stack`: its id and its layers' numbers, bottom to top.
fn stack_of(stack: Option<&Value>) -> Option<(String, Vec<u64>)> {
    let stack = stack?;
    let id = stack.get("id")?.as_str().filter(|id| id.len() <= KEY)?;
    let layers = stack.get("layers").and_then(Value::as_array);
    let numbers: Vec<u64> = layers
        .into_iter()
        .flatten()
        .take(MOST_LINKS)
        .filter_map(|layer| layer.get("number")?.as_u64())
        .collect();
    Some((id.to_string(), numbers))
}

/// A timestamp, when it is a string short enough to be one.
fn stamp(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|at| at.len() <= STAMP)
        .map(str::to_string)
}

/// `raws` in stacks, as `pullRequestListLines` orders them: the stack whose newest layer
/// changed last comes first, and each reads from its bottom layer up.
fn chained(raws: Vec<Raw>) -> Vec<Entry> {
    let count = raws.len();
    let mut stacks = stacks_of(&raws);
    stacks.sort_by_key(|(_, layers)| {
        let newest = layers.iter().map(|&index| raws[index].activity).max();
        Reverse(newest.unwrap_or(0))
    });
    let mut slots: Vec<Option<Entry>> = raws.into_iter().map(|raw| Some(raw.entry)).collect();
    let mut entries = Vec::with_capacity(count);
    for (native, layers) in stacks {
        let size = layers.len();
        for (depth, index) in layers.into_iter().enumerate() {
            if let Some(mut entry) = slots[index].take() {
                entry.depth = depth;
                entry.chain = (depth == 0 && size > 1).then_some(Chain { native, size });
                entries.push(entry);
            }
        }
    }
    entries
}

/// The stacks of `raws`, each its layers' places in `raws` from the bottom up and whether the
/// host keeps it, as `resolveThreadPullRequestChains` groups them. Links in one host-native
/// stack keep the order of the first one's layers. The others chain where one's base branch
/// is another's head branch in the same repository, except where two links share that head.
/// A cycle leaves each of its links alone.
fn stacks_of(raws: &[Raw]) -> Vec<(bool, Vec<usize>)> {
    let count = raws.len();
    let mut placed = vec![false; count];
    let mut stacks: Vec<(bool, Vec<usize>)> = Vec::new();

    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_of: HashMap<(&str, &str, &str), usize> = HashMap::new();
    for (index, raw) in raws.iter().enumerate() {
        let Some((id, _)) = raw.stack.as_ref() else {
            continue;
        };
        let key = &raw.entry.key;
        let group_key = (key.host.as_str(), key.repository.as_str(), id.as_str());
        let next = groups.len();
        let group = *group_of.entry(group_key).or_insert(next);
        if group == next {
            groups.push(Vec::new());
        }
        groups[group].push(index);
    }
    for mut members in groups {
        let layers = raws[members[0]].stack.as_ref().map(|(_, layers)| layers);
        let order: HashMap<u64, usize> = layers
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(at, &number)| (number, at))
            .collect();
        let layer = |index: usize| order.get(&raws[index].entry.number).copied();
        members.sort_by_key(|&index| layer(index).unwrap_or(0));
        for &member in &members {
            placed[member] = true;
        }
        stacks.push((true, members));
    }

    let remaining: Vec<usize> = (0..count).filter(|&index| !placed[index]).collect();
    let branch = |index: usize, base: bool| {
        let raw = &raws[index];
        let snapshot = raw.entry.snapshot()?;
        let name = if base {
            &snapshot.base_key
        } else {
            &snapshot.head_key
        };
        let name = name.as_deref()?;
        let key = &raw.entry.key;
        Some((key.host.as_str(), key.repository.as_str(), name))
    };
    // A head two links share can't say which of them a third builds on.
    let mut by_head: HashMap<(&str, &str, &str), Option<usize>> = HashMap::new();
    for &index in &remaining {
        if let Some(key) = branch(index, false) {
            by_head
                .entry(key)
                .and_modify(|slot| *slot = None)
                .or_insert(Some(index));
        }
    }
    let parent_of = |index: usize| {
        let base = branch(index, true)?;
        by_head.get(&base).copied().flatten()
    };
    let mut has_child = vec![false; count];
    for &index in &remaining {
        if let Some(parent) = parent_of(index).filter(|&parent| parent != index) {
            has_child[parent] = true;
        }
    }
    // From each top, a link nothing builds on, down its bases.
    for &top in &remaining {
        if has_child[top] {
            continue;
        }
        let mut layers = Vec::new();
        let mut cursor = Some(top);
        while let Some(index) = cursor.filter(|&index| !placed[index]) {
            placed[index] = true;
            layers.push(index);
            cursor = parent_of(index);
        }
        layers.reverse();
        if !layers.is_empty() {
            stacks.push((false, layers));
        }
    }
    // A cycle has no top, so each of its links stands alone.
    for &index in &remaining {
        if !placed[index] {
            stacks.push((false, vec![index]));
        }
    }
    stacks
}

/// The links of `list`, none when it has no links.
fn entries_of(list: Option<&List>) -> &[Entry] {
    match list.map(|list| &list.body) {
        Some(Body::Links(links)) => &links.entries,
        _ => &[],
    }
}

/// Where `key`'s link is in `entries`.
fn position(entries: &[Entry], key: &Key) -> Option<usize> {
    entries.iter().position(|entry| &entry.key == key)
}

/// What a frame lays the list out for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shape {
    width: usize,
    height: usize,
    /// The first link shown.
    top: usize,
    selected: usize,
    expanded: Option<usize>,
    /// The minute the relative times read from.
    minute: i64,
}

/// The rows a frame shows.
pub(super) struct Page {
    shape: Shape,
    pub lines: Vec<Line<'static>>,
    /// For each row, the link whose own rows it is on. None for the rows of a link's details,
    /// the blank rows and the footer.
    rows: Vec<Option<usize>>,
    /// Whether links lie above or below the page.
    pub above: bool,
    pub below: bool,
    /// The links a page holds, which PageUp and PageDown move by.
    per_page: usize,
}

/// A selected link's details at one width and minute.
struct Detail {
    index: usize,
    /// The width and the minute these lines are of.
    at: (usize, i64),
    lines: Vec<Line<'static>>,
}

/// The pull request list as the reader left it in this window.
#[derive(Default)]
pub(super) struct PullRequests {
    list: Option<List>,
    /// Set when an event may have changed the links, so the next frame that draws the details
    /// works them out again.
    stale: bool,
    selected: usize,
    /// The selected link, which stays selected when the list changes around it.
    chosen: Option<Key>,
    /// The link whose details show.
    expanded: Option<usize>,
    /// The first link the last frame showed.
    top: usize,
    page: Option<Page>,
    detail: Option<Detail>,
    /// How many times the links were worked out and a page laid out, so the tests can tell
    /// what a frame or an event did.
    #[cfg(test)]
    pub builds: usize,
    #[cfg(test)]
    pub pages: usize,
}

impl PullRequests {
    /// Notes that the links may have changed.
    pub fn mark(&mut self) {
        self.stale = true;
    }

    /// Follows another thread that opened, from the top of its list.
    pub fn follow(&mut self) {
        self.stale = true;
        self.selected = 0;
        self.chosen = None;
        self.expanded = None;
        self.top = 0;
        self.page = None;
        self.detail = None;
    }

    /// Whether this frame has to work the links out for thread `open_id`: an event may have
    /// changed them, they are of another thread, or the environment's support changed.
    pub fn wants(&self, open_id: Option<&str>, supported: bool) -> bool {
        let Some(list) = self.list.as_ref() else {
            return true;
        };
        self.stale || list.thread_id.as_deref() != open_id || list.supported != supported
    }

    /// Takes the links a frame worked out. The selection stays on its link, and the details
    /// that showed stay open, while those links are still listed.
    pub fn set(&mut self, list: List) {
        #[cfg(test)]
        {
            self.builds += 1;
        }
        self.stale = false;
        if self.list.as_ref() == Some(&list) {
            return;
        }
        let expanded = self
            .expanded
            .and_then(|index| entries_of(self.list.as_ref()).get(index))
            .map(|entry| entry.key.clone());
        self.list = Some(list);
        self.page = None;
        self.detail = None;
        let entries = entries_of(self.list.as_ref());
        let selected = self
            .chosen
            .as_ref()
            .and_then(|key| position(entries, key))
            .unwrap_or(self.selected)
            .min(entries.len().saturating_sub(1));
        self.expanded = expanded.and_then(|key| position(entries, &key));
        self.chosen = entries.get(selected).map(|entry| entry.key.clone());
        self.selected = selected;
    }

    /// How many links show, once they are known.
    pub fn count(&self) -> Option<usize> {
        self.list.as_ref().and_then(List::count)
    }

    /// Moves the selection `delta` links, down for a positive one, and stops at the ends.
    pub fn move_by(&mut self, delta: isize) {
        let count = entries_of(self.list.as_ref()).len();
        if count > 0 {
            self.select(self.selected.saturating_add_signed(delta).min(count - 1));
        }
    }

    /// Moves the selection a page down, or up when `down` is false.
    pub fn page_by(&mut self, down: bool) {
        let step = self.page.as_ref().map_or(1, |page| page.per_page) as isize;
        self.move_by(if down { step } else { -step });
    }

    fn select(&mut self, index: usize) {
        self.selected = index;
        self.chosen = entries_of(self.list.as_ref())
            .get(index)
            .map(|entry| entry.key.clone());
    }

    /// Opens the selected link's details, or closes them.
    pub fn toggle(&mut self) {
        if entries_of(self.list.as_ref()).is_empty() {
            return;
        }
        self.expanded = if self.expanded == Some(self.selected) {
            None
        } else {
            Some(self.selected)
        };
    }

    /// A click on row `offset` of the last frame's page: on a link's rows it selects the link,
    /// or opens or closes its details when it is selected already. Says whether it landed on
    /// a link.
    pub fn click(&mut self, offset: usize) -> bool {
        let hit = self.page.as_ref().and_then(|page| page.rows.get(offset));
        let Some(&Some(index)) = hit else {
            return false;
        };
        if index == self.selected {
            self.toggle();
        } else {
            self.select(index);
        }
        true
    }

    /// The rows the list would like at `width`, for a dialog that is as tall as its text.
    pub fn wanted(&mut self, width: usize, now: i64, theme: &Theme) -> usize {
        let count = entries_of(self.list.as_ref()).len();
        if count == 0 {
            return message(self.list.as_ref(), width, theme).len();
        }
        let detail = match self.expanded {
            Some(index) => self.prepare_detail(index, width, now, theme),
            None => 0,
        };
        count * ENTRY_ROWS + detail + 1
    }

    /// The rows of link `index`'s details at `width`, worked out again only for another link,
    /// width or minute.
    fn prepare_detail(&mut self, index: usize, width: usize, now: i64, theme: &Theme) -> usize {
        let at = (width, now.div_euclid(60_000));
        let cached = self.detail.as_ref().map(|detail| (detail.index, detail.at));
        if cached != Some((index, at)) {
            let lines = match entries_of(self.list.as_ref()).get(index) {
                Some(entry) => detail_lines(entry, width, now, theme),
                None => Vec::new(),
            };
            self.detail = Some(Detail { index, at, lines });
        }
        self.detail.as_ref().map_or(0, |detail| detail.lines.len())
    }

    /// The page a frame shows in `height` rows of `width` columns, laid out again only when
    /// its shape changed. The selected link stays on it.
    pub fn page(&mut self, width: usize, height: usize, now: i64, theme: &Theme) -> &Page {
        let shape = Shape {
            width,
            height,
            top: self.top,
            selected: self.selected,
            expanded: self.expanded,
            minute: now.div_euclid(60_000),
        };
        if self.page.as_ref().map(|page| page.shape) != Some(shape) {
            #[cfg(test)]
            {
                self.pages += 1;
            }
            if let Some(index) = shape.expanded {
                self.prepare_detail(index, width, now, theme);
            }
            let (list, detail) = (self.list.as_ref(), self.detail.as_ref());
            let page = lay_out(list, detail, shape, now, theme);
            self.top = page.shape.top;
            self.page = Some(page);
        }
        self.page.as_ref().expect("laid out above")
    }
}

/// The page of `list` for `shape`: as many links as fit from the first shown, moved so the
/// selected one shows, and the footer on the last row. A list without links says why.
fn lay_out(
    list: Option<&List>,
    detail: Option<&Detail>,
    mut shape: Shape,
    now: i64,
    theme: &Theme,
) -> Page {
    let mut page = Page {
        shape,
        lines: Vec::new(),
        rows: Vec::new(),
        above: false,
        below: false,
        per_page: 1,
    };
    let (width, height) = (shape.width, shape.height);
    if width == 0 || height == 0 {
        return page;
    }
    let links = match list.map(|list| &list.body) {
        Some(Body::Links(links)) if !links.entries.is_empty() => links,
        _ => {
            let mut lines = message(list, width, theme);
            page.below = lines.len() > height;
            lines.truncate(height);
            page.lines = lines;
            return page;
        }
    };
    let entries = &links.entries;
    let count = entries.len();
    let footer = height >= 2;
    let room = height - usize::from(footer);
    let selected = shape.selected.min(count - 1);
    let expanded = shape.expanded.filter(|&index| index < count);
    let detail = detail.filter(|detail| Some(detail.index) == expanded);
    let detail_rows = detail.map_or(0, |detail| detail.lines.len());
    // The rows of the links from `from` to `to`, both included.
    let span = |from: usize, to: usize| {
        let open = expanded.is_some_and(|index| (from..=to).contains(&index));
        (to + 1 - from) * ENTRY_ROWS + if open { detail_rows } else { 0 }
    };
    let mut top = shape.top.min(selected);
    while top < selected && span(top, selected) > room {
        top += 1;
    }
    // A page that reaches the end of the list starts as early as it can.
    while top > 0 && span(top - 1, count - 1) <= room {
        top -= 1;
    }
    let mut index = top;
    while index < count && page.lines.len() < room {
        let lines = entry_lines(&entries[index], width, index == selected, now, theme);
        for line in lines.into_iter().take(room - page.lines.len()) {
            page.rows.push(Some(index));
            page.lines.push(line);
        }
        if let Some(detail) = detail.filter(|detail| detail.index == index) {
            for line in detail.lines.iter().take(room - page.lines.len()) {
                page.rows.push(None);
                page.lines.push(line.clone());
            }
        }
        index += 1;
    }
    page.above = top > 0;
    page.below = span(top, count - 1) > room;
    page.per_page = (room / ENTRY_ROWS).max(1);
    if footer {
        page.lines.resize(room, Line::default());
        page.rows.resize(room, None);
        page.lines.push(footer_line(links, width, now, theme));
        page.rows.push(None);
    }
    shape.top = top;
    page.shape = shape;
    page
}

/// What the list says without links: why there are none, or that none are linked.
fn message(list: Option<&List>, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let muted = Style::new().fg(theme.muted);
    let strong = Style::new().fg(theme.fg).add_modifier(Modifier::BOLD);
    let mut skipped = None;
    let (title, title_style, text) = match list.map(|list| &list.body) {
        None | Some(Body::None) => ("No thread is open.", muted, None),
        Some(Body::Loading) => ("Loading…", muted, None),
        Some(Body::Unavailable) => ("Unavailable", muted, None),
        Some(Body::Unsupported) => (
            "Linked pull requests unavailable",
            strong,
            Some("This environment does not support multiple linked pull requests."),
        ),
        Some(Body::Unreadable) => (
            "Linked pull requests unreadable",
            strong,
            Some("T3 sent this thread's pull requests in a form t3term can't read."),
        ),
        Some(Body::Links(links)) => {
            skipped = Some(links.skipped + links.unread).filter(|&skipped| skipped > 0);
            (
                "No linked pull requests",
                strong,
                Some("Pull requests the agent opens from this thread land here."),
            )
        }
    };
    let mut lines = wrapped(title, width, VALUE_ROWS, title_style);
    if let Some(text) = text {
        lines.extend(wrapped(text, width, VALUE_ROWS, muted));
    }
    if let Some(skipped) = skipped {
        let note = match skipped {
            1 => "1 link couldn't be read.".to_string(),
            _ => format!("{skipped} links couldn't be read."),
        };
        lines.push(Line::default());
        lines.extend(wrapped(&note, width, VALUE_ROWS, muted));
    }
    lines
}

/// The footer, as the desktop's reads: how many links are open and linked, and when the host
/// last synced one, then how many couldn't be read.
fn footer_line(links: &Links, width: usize, now: i64, theme: &Theme) -> Line<'static> {
    let mut parts = vec![
        format!("{} open", links.open),
        format!("{} linked", links.entries.len()),
    ];
    if let Some(ago) = time_ago(links.synced.as_deref(), now) {
        parts.push(format!("synced {ago}"));
    }
    if links.skipped > 0 {
        parts.push(format!("{} unreadable", links.skipped));
    }
    if links.unread > 0 {
        parts.push(format!("{} not read", links.unread));
    }
    let text = clip(&parts.join(" · "), width);
    Line::from(Span::styled(text, Style::new().fg(theme.muted)))
}

/// How long ago `at` was, as the desktop's `formatRelativeTimeLabel` writes it: `just now`
/// within a minute, then `5m ago`, `3h ago` or `12d ago`. None when `at` is missing or not a
/// time.
fn time_ago(at: Option<&str>, now: i64) -> Option<String> {
    match relative_time(at?, now).as_str() {
        "" => None,
        "now" => Some("just now".to_string()),
        ago => Some(format!("{ago} ago")),
    }
}

/// The columns `spans` take.
fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|span| span.content.width()).sum()
}

/// `spans` in at most `width` columns: the span that crosses the edge cut with `…` and the
/// ones after it dropped.
fn fitted(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut used = 0;
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let columns = span.content.width();
        if used + columns <= width {
            used += columns;
            out.push(span);
            continue;
        }
        let room = width - used;
        if room > 0 {
            out.push(Span::styled(clip(&span.content, room), span.style));
        }
        break;
    }
    out
}

/// `text` wrapped at `width` to at most `rows` rows, the last cut with `…`. Each row is one
/// span in `style`, so a caller can put the spans on rows of its own.
fn wrapped(text: &str, width: usize, rows: usize, style: Style) -> Vec<Line<'static>> {
    let mut pieces = wrap(text, width);
    if pieces.len() > rows {
        pieces.truncate(rows);
        if let Some(last) = pieces.last_mut() {
            last.push('…');
        }
    }
    pieces
        .iter()
        .map(|piece| Line::from(Span::styled(clip(piece, width), style)))
        .collect()
}

/// `number` with a comma between each three digits, as the desktop's `toLocaleString` writes
/// it in English.
fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The columns a link's rows step in for each layer under it in its stack, at most three
/// layers deep as the desktop caps it, on a list wide enough to spare them.
fn indent(entry: &Entry, width: usize) -> usize {
    if width >= 24 {
        2 * entry.depth.min(3)
    } else {
        0
    }
}

/// A link's mark, the name of its state and their color.
fn state_of(entry: &Entry, theme: &Theme) -> (&'static str, &'static str, Color) {
    match &entry.synced {
        Synced::Waiting => ("○", "Waiting for host state", theme.muted),
        Synced::Unreadable => ("○", "Host state unreadable", theme.warning_fg),
        Synced::Read(snapshot) => match snapshot.state {
            State::Open => ("●", "Open", theme.success),
            State::Draft => ("◌", "Draft", theme.muted),
            State::Merged => ("●", "Merged", theme.violet),
            State::Closed => ("●", "Closed", theme.error_fg),
        },
    }
}

/// What a link's second row says before its time: the stack it heads, its state, then while
/// it is open whether T3 watches it, whether it conflicts, and its checks and review, each
/// only when the host reported it.
fn status_spans(entry: &Entry, theme: &Theme) -> Vec<Span<'static>> {
    let mut parts: Vec<(String, Color)> = Vec::new();
    if let Some(chain) = entry.chain {
        let text = if chain.native {
            format!("Stack of {}", chain.size)
        } else {
            format!("{} chained", chain.size)
        };
        parts.push((text, theme.fg));
    }
    let (_, state, color) = state_of(entry, theme);
    parts.push((state.to_string(), color));
    if entry.open() && entry.watching {
        parts.push(("Watching".into(), theme.info));
    }
    if let Some(snapshot) = entry.snapshot().filter(|_| entry.open()) {
        if snapshot.conflicting {
            parts.push(("Conflicts".into(), theme.error_fg));
        }
        let checks = snapshot.checks.map(|checks| match checks {
            Checks::Passing => ("Checks passing", theme.success),
            Checks::Failing => ("Checks failing", theme.error_fg),
            Checks::Pending => ("Checks pending", theme.warning_fg),
        });
        let review = snapshot.review.map(|review| match review {
            Review::Approved => ("Approved", theme.success),
            Review::ChangesRequested => ("Changes requested", theme.warning_fg),
            Review::Required => ("Awaiting review", theme.muted),
        });
        for (text, color) in checks.into_iter().chain(review) {
            parts.push((text.to_string(), color));
        }
    }
    let mut spans = Vec::new();
    for (text, color) in parts {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", Style::new().fg(theme.muted)));
        }
        spans.push(Span::styled(text, Style::new().fg(color)));
    }
    spans
}

/// A link's three rows. The first has its state's mark, its number and its title, with the
/// lines it adds and removes at the end when the host counted them. The second has its
/// status, with when it last changed at the end. The third has its repository and branches,
/// or its host and repository until the host syncs it.
fn entry_lines(
    entry: &Entry,
    width: usize,
    selected: bool,
    now: i64,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme.muted);
    let value = Style::new().fg(theme.fg);
    let pad = " ".repeat(indent(entry, width));
    let lead = format!("{pad}  ");
    let snapshot = entry.snapshot();

    let (mark, _, color) = state_of(entry, theme);
    let number = format!("#{}", entry.number);
    let diff = diff_spans(snapshot, theme);
    let diff_width = spans_width(&diff);
    let used = pad.len() + 2 + number.len() + 1;
    // The counts show only beside at least four columns of the title.
    let (diff, room) = if diff_width > 0 && used + 5 + diff_width <= width {
        (diff, width - used - 1 - diff_width)
    } else {
        (Vec::new(), width.saturating_sub(used))
    };
    let first = vec![
        Span::raw(pad),
        Span::styled(mark, Style::new().fg(color)),
        Span::raw(" "),
        Span::styled(number, value.add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(cut(entry.title(), room), value),
    ];

    let mut status = vec![Span::raw(lead.clone())];
    status.extend(status_spans(entry, theme));
    let at = snapshot.and_then(|snapshot| snapshot.updated.as_deref());
    let ago = match time_ago(at, now) {
        Some(ago) if ago.width() + 12 <= width => vec![Span::styled(ago, muted)],
        _ => Vec::new(),
    };
    let status_room = match spans_width(&ago) {
        0 => width,
        columns => width - columns - 1,
    };

    let room = width.saturating_sub(lead.len());
    let place = match snapshot {
        Some(snapshot) => branches(&entry.repository, &snapshot.head, &snapshot.base, room),
        None => format!("{}/{}", entry.host, entry.repository),
    };
    let third = vec![Span::raw(lead), Span::styled(place, muted)];

    let lines = [
        row(fitted(first, width), diff, width),
        row(fitted(status, status_room), ago, width),
        row(fitted(third, width), Vec::new(), width),
    ];
    if !selected {
        return Vec::from(lines);
    }
    lines
        .into_iter()
        .map(|line| Line::from(with_bg(line.spans, theme.highlight)))
        .collect()
}

/// The lines a link adds and removes, as the desktop's `PullRequestDiffStat` shows them, or
/// nothing when the host counted none, so a missing count never reads as an empty change. A
/// count the host left out shows as nothing rather than as zero.
fn diff_spans(snapshot: Option<&Snapshot>, theme: &Theme) -> Vec<Span<'static>> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let (additions, deletions) = (snapshot.additions, snapshot.deletions);
    if additions.unwrap_or(0) == 0 && deletions.unwrap_or(0) == 0 {
        return Vec::new();
    }
    let mut spans = Vec::new();
    if let Some(additions) = additions {
        let text = format!("+{}", grouped(additions));
        spans.push(Span::styled(text, Style::new().fg(theme.success)));
    }
    if let Some(deletions) = deletions {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        let text = format!("-{}", grouped(deletions));
        spans.push(Span::styled(text, Style::new().fg(theme.error_fg)));
    }
    spans
}

/// `repository · head → base` in `room` columns. When it doesn't fit, each is cut in the
/// middle, as the desktop's `MiddleTruncate` cuts a repository, so both ends of each stay:
/// the repository to a third of the room, the base to a quarter and the head to the rest.
fn branches(repository: &str, head: &str, base: &str, room: usize) -> String {
    let whole = format!("{repository} · {head} → {base}");
    if whole.width() <= room {
        return whole;
    }
    let room = room.saturating_sub(6);
    let repository = middle(repository, room / 3);
    let base = middle(base, room / 4);
    let rest = room.saturating_sub(repository.width() + base.width());
    let head = middle(head, rest);
    format!("{repository} · {head} → {base}")
}

/// A selected link's details under its rows: its whole title and URL, the URL as plain text
/// to copy, then its status, who linked it and when, its author, when it last changed and
/// synced, its repository and branches, what watching it does and the stack it heads. The URL
/// comes early, so a short pane still shows it.
fn detail_lines(entry: &Entry, width: usize, now: i64, theme: &Theme) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme.muted);
    let value = Style::new().fg(theme.fg);
    let info = Style::new().fg(theme.info);
    let pad = " ".repeat(indent(entry, width) + 2);
    let room = width.saturating_sub(pad.len());
    if room == 0 {
        return Vec::new();
    }
    let snapshot = entry.snapshot();
    let mut parts: Vec<(String, usize, Style)> = Vec::new();
    if let Some(snapshot) = snapshot.filter(|snapshot| !snapshot.title.is_empty()) {
        parts.push((snapshot.title.clone(), TITLE_ROWS, value));
    }
    parts.push((entry.url.clone(), URL_ROWS, info));
    let status: String = status_spans(entry, theme)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    parts.push((status, VALUE_ROWS, value));
    let mut linked = entry.source.label().to_string();
    if let Some(ago) = time_ago(entry.linked_at.as_deref(), now) {
        linked.push_str(" · ");
        linked.push_str(&ago);
    }
    parts.push((linked, VALUE_ROWS, muted));
    if let Some(snapshot) = snapshot {
        if let Some(author) = snapshot.author.as_deref() {
            let bot = if snapshot.bot { " (bot)" } else { "" };
            parts.push((format!("By {author}{bot}"), VALUE_ROWS, muted));
        }
        if let Some(ago) = time_ago(snapshot.updated.as_deref(), now) {
            parts.push((format!("Updated {ago}"), VALUE_ROWS, muted));
        }
        if let Some(ago) = time_ago(snapshot.synced.as_deref(), now) {
            parts.push((format!("Synced {ago}"), VALUE_ROWS, muted));
        }
    }
    let place = format!("{}/{}", entry.host, entry.repository);
    parts.push((place, VALUE_ROWS, muted));
    if let Some(snapshot) = snapshot {
        let branches = format!("{} → {}", snapshot.head, snapshot.base);
        parts.push((branches, VALUE_ROWS, muted));
    }
    if entry.open() && entry.watching {
        parts.push((WATCHING.to_string(), VALUE_ROWS, info));
    }
    if let Some(chain) = entry.chain {
        let size = chain.size;
        let text = if chain.native {
            format!("GitHub stack of {size}: {NATIVE_STACK}")
        } else {
            format!("{size} pull requests chained by base branch.")
        };
        parts.push((text, VALUE_ROWS, muted));
    }
    let mut lines = Vec::new();
    for (text, rows, style) in parts {
        for line in wrapped(&text, room, rows, style) {
            let mut spans = vec![Span::raw(pad.clone())];
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Depth;
    use serde_json::json;

    const NOW: &str = "2026-10-13T12:00:00.000Z";

    fn now() -> i64 {
        parse_iso_ms(NOW).unwrap()
    }

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

    /// A link as T3 sends it, `ThreadPullRequestLink` with every field the pinned contract
    /// has, open on github.com, last synced a minute before `NOW`.
    fn link(repository: &str, number: u64, head: &str, base: &str) -> Value {
        json!({
            "host": "github.com",
            "repository": repository,
            "number": number,
            "url": format!("https://github.com/{repository}/pull/{number}"),
            "source": "agent",
            "linkedAt": "2026-10-13T10:00:00.000Z",
            "snapshot": {
                "state": "open",
                "title": format!("Change {number}"),
                "headBranch": head,
                "baseBranch": base,
                "isDraft": false,
                "updatedAt": "2026-10-13T11:14:00.000Z",
                "syncedAt": "2026-10-13T11:59:00.000Z",
                "closedAt": null,
                "mergedAt": null,
                "author": {"login": "krishhgg", "name": null, "avatarUrl": null},
                "additions": 821,
                "deletions": 51,
                "changedFiles": 9,
                "reviewDecision": "approved",
                "checksState": "passing",
                "mergeability": "mergeable"
            },
            "stack": null
        })
    }

    fn thread(links: Vec<Value>) -> Value {
        json!({"id": "t", "projectId": "p", "pullRequests": links})
    }

    fn list_of(thread: &Value) -> List {
        List::of(Some("t"), Record::Found(thread), true)
    }

    fn entries(list: &List) -> &[Entry] {
        entries_of(Some(list))
    }

    /// Each link's number and how many layers of its stack lie under it.
    fn numbers(list: &List) -> Vec<(u64, usize)> {
        entries(list)
            .iter()
            .map(|entry| (entry.number, entry.depth))
            .collect()
    }

    fn key_text(entry: &Entry) -> String {
        let key = &entry.key;
        format!("{}/{}#{}", key.host, key.repository, key.number)
    }

    /// The rows of `list`'s first page at `width` by `height`.
    fn page_rows(list: List, width: usize, height: usize) -> Vec<String> {
        let mut pulls = PullRequests::default();
        pulls.set(list);
        rows_of(&pulls.page(width, height, now(), &theme()).lines)
    }

    #[test]
    fn a_synced_link_shows_what_the_host_reported() {
        let pull = link("krishhgg/t3term", 31, "feature", "master");
        let rows = page_rows(list_of(&thread(vec![pull])), 44, 4);
        let gap = " ".repeat(21);
        assert_eq!(rows[0], format!("● #31 Change 31{gap}+821 -51"));
        assert_eq!(rows[1], "  Open · Checks passing · Approved   46m ago");
        assert_eq!(rows[2], "  krishhgg/t3term · feature → master");
        assert_eq!(rows[3], "1 open · 1 linked · synced 1m ago");
    }

    #[test]
    fn unknown_fields_show_as_nothing_and_never_as_good_news() {
        let mut bare = link("o/r", 7, "h", "b");
        let snapshot = bare["snapshot"].as_object_mut().unwrap();
        for key in [
            "additions",
            "deletions",
            "reviewDecision",
            "checksState",
            "author",
        ] {
            snapshot.remove(key);
        }
        snapshot.insert("updatedAt".into(), Value::Null);
        let mut nulls = link("o/r", 8, "h2", "b");
        nulls["snapshot"]["checksState"] = Value::Null;
        nulls["snapshot"]["reviewDecision"] = Value::Null;
        nulls["snapshot"]["additions"] = json!(0);
        nulls["snapshot"]["deletions"] = json!(0);
        let mut odd = link("o/r", 9, "h3", "b");
        odd["snapshot"]["checksState"] = json!("neutral");
        odd["snapshot"]["reviewDecision"] = json!(true);
        odd["snapshot"]["additions"] = json!(-4);
        odd["snapshot"]["deletions"] = json!("51");
        for pull in [bare, nulls, odd] {
            let rows = page_rows(list_of(&thread(vec![pull])), 44, 4);
            let status = rows[1].trim_end_matches("46m ago").trim_end();
            assert_eq!(status, "  Open", "{rows:#?}");
            let shown = rows.join("\n");
            for word in ["Checks", "Approved", "review", "+", "-0", "-51"] {
                assert!(!shown.contains(word), "{word} in {rows:#?}");
            }
        }
        // A count the host sent shows without the one it left out.
        let mut added = link("o/r", 6, "h", "b");
        added["snapshot"]["additions"] = json!(1_234);
        let snapshot = added["snapshot"].as_object_mut().unwrap();
        snapshot.remove("deletions");
        let rows = page_rows(list_of(&thread(vec![added])), 30, 4);
        assert!(rows[0].ends_with(" +1,234"), "{rows:#?}");
        assert!(!rows[0].contains('-'), "{rows:#?}");
        // A link the host hasn't synced waits for it, and shows its host and repository.
        let mut waiting = link("Owner/Repo", 3, "h", "b");
        waiting["snapshot"] = Value::Null;
        waiting["watch"] = json!({"startedAt": NOW, "headSha": null, "failedChecks": [],
            "passed": false, "passedChecks": [], "remarksThrough": NOW, "remarkIds": [],
            "conflicting": false, "wakes": 0});
        let rows = page_rows(list_of(&thread(vec![waiting])), 44, 4);
        assert_eq!(rows[0], "○ #3 Owner/Repo");
        assert_eq!(rows[1], "  Waiting for host state · Watching");
        assert_eq!(rows[2], "  github.com/Owner/Repo");
        assert_eq!(rows[3], "1 open · 1 linked");
        // A snapshot that breaks the contract is unreadable, not waiting.
        let mut broken = link("o/r", 4, "h", "b");
        broken["snapshot"]["state"] = json!("reopened");
        let rows = page_rows(list_of(&thread(vec![broken])), 44, 4);
        assert_eq!(rows[1], "  Host state unreadable");
        assert_eq!(rows[2], "  github.com/o/r");
    }

    #[test]
    fn states_drafts_conflicts_and_signals_follow_the_desktop() {
        let mut draft = link("o/r", 1, "a", "main");
        draft["snapshot"]["isDraft"] = json!(true);
        draft["snapshot"]["mergeability"] = json!("conflicting");
        let mut conflicting = link("o/r", 2, "b", "main");
        conflicting["snapshot"]["mergeability"] = json!("conflicting");
        conflicting["snapshot"]["checksState"] = json!("failing");
        conflicting["snapshot"]["reviewDecision"] = json!("changes-requested");
        let mut merged = link("o/r", 3, "c", "main");
        merged["snapshot"]["state"] = json!("merged");
        merged["snapshot"]["isDraft"] = json!(true);
        merged["watch"] = json!({});
        let mut closed = link("o/r", 4, "d", "main");
        closed["snapshot"]["state"] = json!("closed");
        let mut pending = link("o/r", 5, "e", "main");
        pending["snapshot"]["checksState"] = json!("pending");
        pending["snapshot"]["reviewDecision"] = json!("review-required");
        let cases = [
            (draft, "Draft · Checks passing · Approved"),
            (
                conflicting,
                "Open · Conflicts · Checks failing · Changes requested",
            ),
            // A closed or merged pull request shows no checks, review or watch.
            (merged, "Merged"),
            (closed, "Closed"),
            (pending, "Open · Checks pending · Awaiting review"),
        ];
        for (pull, status) in cases {
            let rows = page_rows(list_of(&thread(vec![pull])), 80, 4);
            let shown = rows[1].trim_end_matches("46m ago").trim_end();
            assert_eq!(shown, format!("  {status}"));
        }
    }

    #[test]
    fn the_legacy_link_stands_in_only_without_a_list() {
        let legacy = json!({"projectId": "p", "repository": " Owner/Repo ", "number": 12,
            "url": "https://User@GitHub.com:443/Owner/Repo/pull/12?x#y"});
        let only = json!({"id": "t", "linkedPullRequest": legacy.clone()});
        let list = list_of(&only);
        let [entry] = entries(&list) else {
            panic!("{list:?}")
        };
        let key = Key {
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 12,
        };
        assert_eq!(entry.key, key);
        assert_eq!(entry.source, Source::Manual);
        assert_eq!(entry.synced, Synced::Waiting);
        assert_eq!(entry.linked_at, None);
        let rows = page_rows(list, 40, 4);
        assert_eq!(rows[0], "○ #12 owner/repo");
        assert_eq!(rows[1], "  Waiting for host state");
        assert_eq!(rows[2], "  github.com/owner/repo");
        // An explicit empty list means the link was removed. No list and no link, or a null
        // link, is none at all.
        let unlinked = json!({"id": "t", "pullRequests": [], "linkedPullRequest": legacy});
        let bare = json!({"id": "t"});
        let null = json!({"id": "t", "linkedPullRequest": null});
        for thread in [unlinked, bare, null] {
            assert_eq!(list_of(&thread).count(), Some(0), "{thread}");
            let rows = page_rows(list_of(&thread), 40, 6);
            assert_eq!(rows[0], "No linked pull requests");
        }
        // A list wins over the legacy link.
        let mut both = thread(vec![link("o/r", 5, "h", "b")]);
        both["linkedPullRequest"] = json!({"projectId": "p", "repository": "x/y", "number": 1,
            "url": "https://github.com/x/y/pull/1"});
        assert_eq!(numbers(&list_of(&both)), [(5, 0)]);
    }

    #[test]
    fn hosts_read_as_new_url_reads_them() {
        for (url, host) in [
            ("https://github.com/o/r/pull/1", "github.com"),
            ("https://GitHub.COM:8443/o/r", "github.com"),
            ("http://user:pw@git.example.org/x", "git.example.org"),
            ("http://[::1]:3000/x", "[::1]"),
            ("https://host?q", "host"),
        ] {
            assert_eq!(host_of(url), host, "{url}");
        }
        for unknown in ["github.com/o/r", "", "://x", "1http://x/y", "file:///x"] {
            assert_eq!(host_of(unknown), "unknown", "{unknown}");
        }
        let long = format!("https://{}/o/r", "h".repeat(3 * KEY));
        assert_eq!(host_of(&long), "unknown");
    }

    #[test]
    fn tombstones_hide_and_each_identity_shows_once() {
        let mut dismissed = link("o/r", 2, "h2", "b");
        dismissed["source"] = json!("stack-dismissed");
        let mut again = link(" O/R ", 1, "h1", "b");
        again["host"] = json!("GITHUB.com ");
        again["snapshot"]["title"] = json!("The second copy");
        let mut elsewhere = link("o/r", 1, "h1", "b");
        elsewhere["host"] = json!("github.example.com");
        let list = list_of(&thread(vec![
            link("o/r", 1, "h1", "b"),
            dismissed,
            again,
            link("o/other", 1, "h1", "b"),
            elsewhere,
        ]));
        let keys: Vec<String> = entries(&list).iter().map(key_text).collect();
        let expected = [
            "github.com/o/r#1",
            "github.com/o/other#1",
            "github.example.com/o/r#1",
        ];
        assert_eq!(keys, expected);
        assert_eq!(entries(&list)[0].title(), "Change 1");
        // The tombstone is neither shown nor counted as unreadable.
        let rows = page_rows(list, 40, 10);
        assert_eq!(rows[9], "3 open · 3 linked · synced 1m ago");
    }

    #[test]
    fn stacks_read_bottom_up_and_the_newest_comes_first() {
        let at = |mut pull: Value, updated: &str| {
            pull["snapshot"]["updatedAt"] = json!(updated);
            pull
        };
        // A chain worked out from branches: 11 on main, 12 on 11's head, 13 on 12's.
        let derived = [
            at(link("o/r", 13, "c", "b"), "2026-10-13T09:00:00.000Z"),
            at(link("o/r", 11, "a", "main"), "2026-10-13T08:00:00.000Z"),
            at(link("o/r", 12, "b", "a"), "2026-10-13T07:00:00.000Z"),
        ];
        // A host-native stack, listed top first, ordered by its first member's layers.
        let late = "2026-10-13T11:00:00.000Z";
        let native = |number: u64| {
            let head = format!("n{number}");
            let mut pull = at(link("o/s", number, &head, "x"), late);
            pull["stack"] = json!({"kind": "native", "id": "s1", "number": 20,
                "url": "https://github.com/o/s/stack/s1", "base": "main",
                "layers": [{"number": 21, "headBranch": "n21", "state": "open"},
                           {"number": 22, "headBranch": "n22", "state": "open"}]});
            pull
        };
        // Two links share a head, so the one on it chains to neither.
        let shared = [
            at(link("o/t", 31, "same", "main"), "2026-10-13T01:00:00.000Z"),
            at(link("o/t", 32, "same", "main"), "2026-10-13T01:00:00.000Z"),
            at(link("o/t", 33, "top", "same"), "2026-10-13T02:00:00.000Z"),
        ];
        // A cycle has no top, so each of its links stands alone.
        let cycle = [
            at(link("o/u", 41, "x", "y"), "2026-10-12T00:00:00.000Z"),
            at(link("o/u", 42, "y", "x"), "2026-10-12T00:00:00.000Z"),
        ];
        let mut links = vec![native(22), native(21)];
        links.extend(derived);
        links.extend(shared);
        links.extend(cycle);
        let list = list_of(&thread(links));
        let expected = [
            (21, 0),
            (22, 1),
            (11, 0),
            (12, 1),
            (13, 2),
            (33, 0),
            (31, 0),
            (32, 0),
            (41, 0),
            (42, 0),
        ];
        assert_eq!(numbers(&list), expected);
        // Only a stack's bottom layer names it.
        let chain_of = |at: usize| entries(&list)[at].chain.map(|c| (c.native, c.size));
        assert_eq!(chain_of(0), Some((true, 2)));
        assert_eq!(chain_of(1), None);
        assert_eq!(chain_of(2), Some((false, 3)));
        assert!((3..10).all(|at| chain_of(at).is_none()));
        // The layers step in under the ones they build on.
        let rows = page_rows(list, 40, 16);
        assert!(rows[0].starts_with("● #21"), "{rows:#?}");
        assert!(rows[1].starts_with("  Stack of 2 · Open"), "{rows:#?}");
        assert!(rows[3].starts_with("  ● #22"), "{rows:#?}");
        assert!(rows[6].starts_with("● #11"), "{rows:#?}");
        assert!(rows[7].starts_with("  3 chained · Open"), "{rows:#?}");
        assert!(rows[9].starts_with("  ● #12"), "{rows:#?}");
        assert!(rows[12].starts_with("    ● #13"), "{rows:#?}");
    }

    #[test]
    fn unreadable_lists_and_links_are_named_and_never_guessed() {
        for value in [json!("x"), json!({}), Value::Null, json!(3)] {
            let thread = json!({"id": "t", "pullRequests": value});
            assert_eq!(list_of(&thread).count(), None);
            let rows = page_rows(list_of(&thread), 40, 6);
            assert_eq!(rows[0], "Linked pull requests unreadable", "{thread}");
        }
        let mut bad = Vec::new();
        for (key, value) in [
            ("number", json!(0)),
            ("number", json!(-1)),
            ("number", json!("5")),
            ("url", Value::Null),
            ("host", json!("  ")),
            ("source", json!(7)),
            ("repository", json!("r".repeat(KEY + 1))),
        ] {
            let mut pull = link("o/r", 9, "h", "b");
            pull[key] = value;
            bad.push(pull);
        }
        bad.push(json!("not a link"));
        let mut links = bad.clone();
        links.push(link("o/r", 1, "h", "b"));
        let list = list_of(&thread(links));
        assert_eq!(numbers(&list), [(1, 0)]);
        let rows = page_rows(list, 60, 4);
        assert_eq!(rows[3], "1 open · 1 linked · synced 1m ago · 8 unreadable");
        let rows = page_rows(list_of(&thread(bad)), 60, 8);
        assert_eq!(rows[0], "No linked pull requests");
        assert_eq!(rows.last().unwrap(), "8 links couldn't be read.");
    }

    #[test]
    fn the_thread_and_the_environment_decide_before_the_links() {
        let record = thread(vec![link("o/r", 1, "h", "b")]);
        let unsupported = "Linked pull requests unavailable";
        let cases = [
            (None, Record::Found(&record), true, "No thread is open."),
            (Some("t"), Record::None, true, "No thread is open."),
            (Some("t"), Record::Loading, true, "Loading…"),
            (Some("t"), Record::Unavailable, true, "Unavailable"),
            (Some("t"), Record::Found(&record), false, unsupported),
            (Some("t"), Record::Loading, false, unsupported),
        ];
        for (open_id, record, supported, first) in cases {
            let list = List::of(open_id, record, supported);
            assert_eq!(list.count(), None);
            assert_eq!(page_rows(list, 40, 6)[0], first);
        }
        let list = List::of(Some("t"), Record::Found(&record), false);
        let rows = page_rows(list, 40, 6);
        let expected = [
            "This environment does not support",
            "multiple linked pull requests.",
        ];
        assert_eq!(rows[1..3], expected);
    }

    #[test]
    fn hostile_and_huge_text_is_bounded_cleaned_and_fits_every_width() {
        let hostile = format!(
            "\u{1b}]52;c;cHduZWQ=\u{7}\u{1b}[2J{}\r\nevil",
            "👩\u{200d}💻e\u{301}名".repeat(200_000)
        );
        let mut huge = link("o/r\u{1b}[31m名", 1, &hostile, &"漢".repeat(3_000_000));
        huge["host"] = json!(format!("gh\u{9b}.com{}", "h".repeat(300)));
        huge["url"] = json!(format!("https://github.com/{}", "u".repeat(3_000_000)));
        huge["snapshot"]["title"] = json!(&hostile);
        huge["snapshot"]["author"]["login"] = json!(&hostile);
        let list = list_of(&thread(vec![huge]));
        let [entry] = entries(&list) else {
            panic!("not one link")
        };
        let snapshot = entry.snapshot().expect("read");
        let author = snapshot.author.as_ref().expect("an author");
        let fields = [
            (&entry.host, NAME),
            (&entry.repository, NAME),
            (&entry.url, URL),
            (&snapshot.title, TITLE),
            (&snapshot.head, NAME),
            (&snapshot.base, NAME),
            (author, NAME),
        ];
        for (text, max) in fields {
            let count = text.chars().count();
            assert!(count <= max + 1, "{count}");
            assert!(!text.contains(char::is_control), "{text:?}");
        }
        // Branches too long to match by chain to nothing.
        assert_eq!(snapshot.head_key, None);
        assert_eq!(snapshot.base_key, None);

        let mut pulls = PullRequests::default();
        pulls.set(list);
        pulls.toggle();
        let theme = theme();
        for width in [1, 2, 3, 7, 12, 20, 33, 52] {
            for height in [1, 2, 5, 40] {
                let page = pulls.page(width, height, now(), &theme);
                assert!(page.lines.len() <= height);
                for line in &page.lines {
                    let text = line.to_string();
                    assert!(line.width() <= width, "{text:?} at {width}x{height}");
                    assert!(!text.contains(char::is_control), "{text:?}");
                    assert!(!text.contains("evil"), "{text:?}");
                }
            }
        }
        let rows = rows_of(&pulls.page(40, 40, now(), &theme).lines);
        let start = "● #1 ]52;c;cHduZWQ=[2J👩\u{200d}💻e\u{301}名";
        assert!(rows[0].starts_with(start), "{rows:#?}");
        // The details wrap the URL to eight rows, the last cut with …
        let urls: Vec<&String> = rows.iter().filter(|row| row.contains("uuuu")).collect();
        assert_eq!(urls.len(), URL_ROWS, "{rows:#?}");
        assert!(urls[URL_ROWS - 1].ends_with('…'), "{rows:#?}");
    }

    #[test]
    fn a_thousand_links_are_read_once_and_a_frame_lays_out_only_its_page() {
        let links: Vec<Value> = (1..=1_500)
            .map(|number| link("o/r", number, &format!("h{number}"), "main"))
            .collect();
        let list = list_of(&thread(links));
        assert_eq!(list.count(), Some(MOST_LINKS));
        let mut pulls = PullRequests::default();
        pulls.set(list);
        let theme = theme();
        let page = pulls.page(36, 31, now(), &theme);
        // Thirty rows of links and the footer: ten links laid out of a thousand.
        let laid: HashSet<usize> = page.rows.iter().flatten().copied().collect();
        assert_eq!(laid.len(), 10);
        assert!(page.below && !page.above);
        let footer = page.lines.last().map(Line::to_string);
        let expected = "1000 open · 1000 linked · synced 1m…";
        assert_eq!(footer.as_deref(), Some(expected));
        // Frames of the same shape lay nothing out again.
        for _ in 0..5 {
            pulls.page(36, 31, now(), &theme);
        }
        assert_eq!((pulls.builds, pulls.pages), (1, 1));
        // The end of the list, a page at a time and then at once.
        pulls.page_by(true);
        assert_eq!(pulls.selected, 10);
        pulls.move_by(isize::MAX);
        let page = pulls.page(36, 31, now(), &theme);
        let laid: HashSet<usize> = page.rows.iter().flatten().copied().collect();
        let last: HashSet<usize> = (990..1000).collect();
        assert_eq!(laid, last);
        assert!(page.above && !page.below);
        assert_eq!(pulls.pages, 2);
        pulls.move_by(isize::MIN);
        assert_eq!((pulls.selected, pulls.top), (0, 990));
        pulls.page(36, 31, now(), &theme);
        assert_eq!(pulls.top, 0);
        // Wider, the footer counts the links past the first thousand.
        let page = pulls.page(80, 31, now(), &theme);
        let footer = page.lines.last().map(Line::to_string);
        let expected = "1000 open · 1000 linked · synced 1m ago · 500 not read";
        assert_eq!(footer.as_deref(), Some(expected));
        assert_eq!(pulls.builds, 1);
    }

    #[test]
    fn the_selection_and_the_details_stay_on_their_link_as_the_list_changes() {
        let mut pulls = PullRequests::default();
        let theme = theme();
        let dated = |number: u64| {
            let mut pull = link("o/r", number, &format!("h{number}"), "main");
            pull["snapshot"]["updatedAt"] = json!(format!("2026-10-1{number}T00:00:00.000Z"));
            pull
        };
        pulls.set(list_of(&thread(vec![dated(1), dated(2), dated(3)])));
        // Newest first: 3, 2, 1.
        pulls.move_by(1);
        pulls.toggle();
        assert_eq!((pulls.selected, pulls.expanded), (1, Some(1)));
        let rows = rows_of(&pulls.page(40, 30, now(), &theme).lines);
        let url = "https://github.com/o/r/pull/2";
        assert!(rows.iter().any(|row| row.contains(url)), "{rows:#?}");
        // A newer link comes first, and the selection and the details stay on #2.
        let four = vec![dated(1), dated(2), dated(3), dated(4)];
        pulls.set(list_of(&thread(four)));
        assert_eq!((pulls.selected, pulls.expanded), (2, Some(2)));
        // #2 goes. The selection stays where it was, and the details close.
        let without = vec![dated(1), dated(3), dated(4)];
        pulls.set(list_of(&thread(without.clone())));
        assert_eq!((pulls.selected, pulls.expanded), (2, None));
        assert_eq!(pulls.chosen.as_ref().map(|key| key.number), Some(1));
        // The same list again lays nothing out again.
        pulls.page(40, 30, now(), &theme);
        let pages = pulls.pages;
        pulls.set(list_of(&thread(without)));
        pulls.page(40, 30, now(), &theme);
        assert_eq!(pulls.pages, pages);
        // Only an event, another thread or another environment's support works them out again.
        assert!(!pulls.wants(Some("t"), true));
        assert!(pulls.wants(Some("t"), false));
        assert!(pulls.wants(Some("u"), true));
        assert!(pulls.wants(None, true));
        pulls.mark();
        assert!(pulls.wants(Some("t"), true));
        // Another thread starts at its top.
        pulls.follow();
        assert!(pulls.wants(Some("u"), true));
        assert_eq!((pulls.selected, pulls.expanded, pulls.top), (0, None, 0));
    }

    #[test]
    fn clicks_select_a_link_and_a_second_opens_its_details() {
        let mut pulls = PullRequests::default();
        let theme = theme();
        let links = vec![link("o/r", 1, "a", "m"), link("o/r", 2, "b", "m")];
        pulls.set(list_of(&thread(links)));
        // Before a frame lays the list out, a click lands on nothing.
        assert!(!pulls.click(0));
        pulls.page(40, 20, now(), &theme);
        // The second link's rows are the page's fourth to sixth.
        assert!(pulls.click(4));
        assert_eq!((pulls.selected, pulls.expanded), (1, None));
        pulls.page(40, 20, now(), &theme);
        assert!(pulls.click(3));
        assert_eq!(pulls.expanded, Some(1));
        // The details' rows, the blank rows and the footer take no click.
        pulls.page(40, 20, now(), &theme);
        for offset in [6, 7, 18, 19, 20, 200] {
            assert!(!pulls.click(offset), "{offset}");
        }
        assert_eq!((pulls.selected, pulls.expanded), (1, Some(1)));
        // A click on the first link selects it, and the second's details stay open.
        assert!(pulls.click(0));
        assert_eq!((pulls.selected, pulls.expanded), (0, Some(1)));
    }

    #[test]
    fn wanted_rows_follow_the_links_and_the_details() {
        let mut pulls = PullRequests::default();
        let theme = theme();
        let links = vec![link("o/r", 1, "a", "m"), link("o/r", 2, "b", "m")];
        pulls.set(list_of(&thread(links)));
        assert_eq!(pulls.wanted(40, now(), &theme), 2 * ENTRY_ROWS + 1);
        pulls.toggle();
        assert!(pulls.wanted(40, now(), &theme) > 2 * ENTRY_ROWS + 1);
        pulls.set(list_of(&thread(vec![])));
        assert_eq!(pulls.wanted(40, now(), &theme), 3);
    }

    #[test]
    fn counts_group_their_digits() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
