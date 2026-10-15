//! The pull requests linked to the open thread, in the details' second tab. Each shows its
//! number, title and state, and the checks, review and line counts the host reported, with
//! the repository and its branches under them. Enter or a second click shows a link's details
//! in the list's place, its whole URL among them as plain text, and they scroll. ←, Enter or
//! a click on the row above them goes back to the list. t3term opens no browser and changes
//! nothing: the list only reads.
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
//! selection, the details' scroll or the minute changes.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::details::Record;
use super::label::cut;
use super::markdown::{self, Styles};
use super::setup::{bounded, middle};
use super::tasks::wrap;
use super::theme::{Theme, age, parse_iso_ms, relative_time};
use super::{clip, row, with_bg};
use crate::pull_request::{self, BaseComparison, CheckStatus, Mergeability};
use crate::transcript::without_controls;

mod conversation;

use conversation::{Control, Conversation, Reviewer, Shown, Targets};

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
/// The most rows each sentence of a list without links wraps to.
const VALUE_ROWS: usize = 3;
/// What a GitHub stack does, as the desktop's tooltip says it.
const NATIVE_STACK: &str = "merging a layer lands the ones below it.";
/// What watching a pull request does, as the desktop's tooltip says it.
const WATCHING: &str =
    "Watching: the agent wakes when checks finish, someone comments, or the branch conflicts";
/// How long a read of a link's pull request stands, in milliseconds, as the desktop keeps
/// `pullRequests.detail` for 60 seconds (packages/client-runtime/src/state/pullRequests.ts).
/// Opening the link's details again within it shows that read rather than asking again.
const FRESH: i64 = 60_000;
/// The most characters kept of why a read failed.
const FAILURE: usize = 1024;

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
    /// The identity of link `number` on `host`'s `repository`. None when either is longer
    /// than `KEY` bytes as T3 sent it, so no longer text is trimmed or lowercased, or when
    /// either is empty once trimmed.
    fn of(host: &str, repository: &str, number: u64) -> Option<Key> {
        let part = |text: &str| {
            if text.len() > KEY {
                return None;
            }
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_lowercase())
        };
        Some(Key {
            host: part(host)?,
            repository: part(repository)?,
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
    /// The host and the repository as T3 sent them, trimmed, which a read of the pull request
    /// names. Each is at most `KEY` bytes, as `Key::of` checked.
    ask_host: String,
    ask_repository: String,
    number: u64,
    url: String,
    /// Whether `url` is cut short of the one T3 sent.
    url_cut: bool,
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
    let (url, url_cut) = url_of(url);
    let entry = Entry {
        host: bounded(host, NAME),
        repository: bounded(repository, NAME),
        ask_host: host.trim().to_string(),
        ask_repository: repository.trim().to_string(),
        key,
        number,
        url,
        url_cut,
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
    let host = host_of(url);
    let key = Key::of(&host, repository, number)?;
    let (url, url_cut) = url_of(url);
    let entry = Entry {
        host: bounded(&key.host, NAME),
        repository: bounded(&key.repository, NAME),
        ask_host: host,
        ask_repository: repository.trim().to_string(),
        number,
        url,
        url_cut,
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

/// `url` bounded and cleaned, and whether that cut it short. `bounded` ends what it cuts with
/// `…`, and cuts only text longer than `URL` characters, so a URL that is both was cut.
fn url_of(url: &str) -> (String, bool) {
    let shown = bounded(url, URL);
    let cut = shown.ends_with('…') && url.char_indices().nth(URL).is_some();
    (shown, cut)
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
/// A cycle leaves each of its links alone, and the links that build on one stack apart from
/// it. The desktop's walk from them carries on into the cycle and shows them all as one stack,
/// in an order no branch gives. This walk stops where it comes back to a link it passed.
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
        // A walk that comes back to a link it passed has run into a cycle. That link and the
        // ones the walk passed after it stand alone, and the links that build on them stack
        // without them.
        let cycle = layers.iter().position(|&index| Some(index) == cursor);
        if let Some(start) = cycle {
            for index in layers.drain(start..) {
                stacks.push((false, vec![index]));
            }
        }
        layers.reverse();
        if !layers.is_empty() {
            stacks.push((false, layers));
        }
    }
    // A cycle nothing else builds on has no top, so each of its links stands alone.
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

/// What a frame lays the list or a link's details out for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shape {
    width: usize,
    height: usize,
    /// The first link shown.
    top: usize,
    selected: usize,
    /// Whether the selected link's details show in the list's place.
    viewing: bool,
    /// The first row of the details shown.
    scroll: usize,
    /// The minute the relative times read from.
    minute: i64,
    /// The control ↑ and ↓ chose in the details, which shows highlighted while they have the
    /// keyboard.
    pick: Option<Control>,
    focused: bool,
}

/// What a click on a row of a page reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spot {
    /// One of a link's rows in the list.
    Link(usize),
    /// The row above a link's details, which leads back to the list.
    Back,
    /// A row of a control in the details' Comments section.
    Control(Control),
}

/// The rows a frame shows.
pub(super) struct Page {
    shape: Shape,
    pub lines: Vec<Line<'static>>,
    /// For each row, what a click on it reaches. None for the blank rows, the footer and the
    /// text of a link's details.
    rows: Vec<Option<Spot>>,
    /// Whether links, or rows of a link's details, lie above or below the page.
    pub above: bool,
    pub below: bool,
    /// The links a page of the list holds, or the rows of details it shows, which PageUp and
    /// PageDown move by.
    per_page: usize,
}

/// The selected link's details at one width and minute.
struct Detail {
    index: usize,
    /// The width and the minute these lines are of.
    at: (usize, i64),
    lines: Vec<Line<'static>>,
    /// The rows of each control among the lines, in the order they show.
    targets: Targets,
}

/// How far the details are scrolled: a row from their top, or a number of rows up from where
/// their last row shows. End asks for the bottom before a frame knows how many rows that takes
/// at the width it draws, so the keys after it count from there and the later one wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Offset {
    Top(usize),
    End(usize),
}

impl Default for Offset {
    fn default() -> Offset {
        Offset::Top(0)
    }
}

/// What reading a link's pull request from its host names: the link's host, repository and
/// number as T3 sent them, and the ticket its answer comes back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Ask {
    pub ticket: u64,
    pub host: String,
    pub repository: String,
    pub number: u64,
}

/// Part of what a host said of a pull request. A read sends its detail and its conversation as
/// each arrives, then `Done`.
pub(super) enum Part {
    /// The answer to `pullRequests.detail`, or why it failed.
    Detail(Result<Box<pull_request::Detail>, String>),
    /// The answer to `pullRequests.activity`, or why it failed.
    Activity(Result<Box<pull_request::Activity>, String>),
    /// Both answers came.
    Done,
}

/// A read of one link's pull request from its host: `pullRequests.detail` and
/// `pullRequests.activity` at once, as the desktop's panel asks for them.
struct Read {
    key: Key,
    host: String,
    repository: String,
    number: u64,
    /// The read's ticket, once it was asked for. A read waits without one while another is
    /// under way.
    ticket: Option<u64>,
    /// The time it was asked for, or wanted while it waits.
    asked: i64,
    answer: Answer<Summary>,
    conversation: Answer<Conversation>,
}

enum Answer<T> {
    Reading,
    Read(Box<T>),
    /// Why it failed, cleaned and bounded.
    Failed(String),
}

impl Read {
    /// Whether opening the link's details again should leave it be: it is under way, or both
    /// its answers came within `FRESH` of `now`. A failure of either is never reused.
    fn stands(&self, now: i64) -> bool {
        match (&self.answer, &self.conversation) {
            (Answer::Failed(_), _) | (_, Answer::Failed(_)) => false,
            (Answer::Reading, _) | (_, Answer::Reading) => true,
            (Answer::Read(_), Answer::Read(_)) => now.saturating_sub(self.asked) < FRESH,
        }
    }
}

/// What the details show of a pull request's detail, as the desktop's Summary tab does. Each
/// text is cleaned and bounded once, when the answer arrives.
struct Summary {
    /// The commits the head is behind its base, when the host says an open pull request that
    /// merges cleanly is behind, as the desktop's `resolveBaseFreshness` warns of it. A
    /// conflict or an unknown answer leaves the warning out. The inner None is a count the
    /// host didn't give.
    behind: Option<Option<u64>>,
    /// The base branch, which the warning about being behind names.
    base: String,
    reviewers: Vec<Reviewer>,
    /// None where the pull request has no labels and the host offers none, which the desktop
    /// leaves out.
    labels: Option<Vec<String>>,
    checks: Vec<CheckRow>,
    /// The description with its control characters removed, or empty.
    body: String,
    /// Whether T3 sent a longer description than t3term keeps.
    body_cut: bool,
    /// Whether T3 sent more reviewers, labels or checks than t3term keeps.
    cut: bool,
    /// The description laid out at one width, so a new minute doesn't lay it out again.
    laid_out: Option<(usize, Vec<Line<'static>>)>,
}

/// One check on the pull request's head.
struct CheckRow {
    name: String,
    status: CheckStatus,
    /// A GitHub Actions run that waits for a maintainer's approval, which the desktop's
    /// `isWorkflowApprovalCheck` names.
    approval: bool,
}

impl Summary {
    fn of(detail: &pull_request::Detail) -> Summary {
        let behind = (detail.state == pull_request::State::Open
            && detail.mergeability == Mergeability::Mergeable
            && detail.base_comparison == Some(BaseComparison::Behind))
        .then_some(detail.behind_by);
        let reviewers = detail.reviewers.iter().map(Reviewer::of).collect();
        let labels = (!detail.labels.is_empty() || detail.capabilities.labels).then(|| {
            let names = detail.labels.iter();
            names.map(|label| bounded(&label.name, NAME)).collect()
        });
        let checks = detail
            .checks
            .iter()
            .map(|check| CheckRow {
                name: bounded(&check.name, NAME),
                status: check.status,
                approval: check.status == CheckStatus::ActionRequired
                    && check.url.as_deref().is_some_and(is_workflow_run),
            })
            .collect();
        Summary {
            behind,
            base: bounded(&detail.base_branch, NAME),
            reviewers,
            labels,
            checks,
            body: without_controls(detail.body.text.trim()),
            body_cut: detail.body.cut,
            cut: detail.cut,
            laid_out: None,
        }
    }

    /// Lays the description out at `width`, unless it already is.
    fn lay_out(&mut self, width: usize, theme: &Theme) {
        if self.laid_out.as_ref().is_some_and(|(at, _)| *at == width) {
            return;
        }
        let lines = if self.body.is_empty() {
            let none = Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC);
            whole("No description provided.", width, none)
        } else {
            let styles = Styles::new(theme, Style::new().fg(theme.fg));
            markdown::render(&self.body, width, &styles, 0)
        };
        self.laid_out = Some((width, lines));
    }
}

/// Whether `url` is a GitHub Actions run, as the desktop's `/\/actions\/runs\/\d+(?:\/|$)/`
/// matches it.
fn is_workflow_run(url: &str) -> bool {
    const RUNS: &str = "/actions/runs/";
    url.match_indices(RUNS).any(|(at, _)| {
        let rest = &url[at + RUNS.len()..];
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        digits > 0 && matches!(rest[digits..].chars().next(), None | Some('/'))
    })
}

impl CheckRow {
    /// The check's mark, its status as the desktop's `pullRequestCheckStatusLabel` names it,
    /// and their color.
    fn shown(&self, theme: &Theme) -> (&'static str, &'static str, Color) {
        match self.status {
            _ if self.approval => ("◉", "Awaiting approval", theme.warning_fg),
            CheckStatus::Pending => ("○", "Running", theme.warning_fg),
            CheckStatus::ActionRequired => ("◉", "Awaiting action", theme.warning_fg),
            CheckStatus::Success => ("✓", "Passed", theme.success),
            CheckStatus::Failure => ("✗", "Failed", theme.error_fg),
            CheckStatus::Cancelled => ("✗", "Cancelled", theme.error_fg),
            CheckStatus::Skipped => ("–", "Skipped", theme.muted),
            CheckStatus::Neutral => ("–", "Neutral", theme.muted),
        }
    }
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
    /// Whether the selected link's details show in the list's place.
    viewing: bool,
    /// How far the details are scrolled. A frame turns it into the first row it shows, kept
    /// within the details.
    scroll: Offset,
    /// The first link the last frame showed.
    top: usize,
    page: Option<Page>,
    /// Whether `page` is what the last frame drew. Moving the selection, scrolling the details
    /// or switching between them and the list clears it, so a click before the next frame
    /// lands on nothing rather than on a row that moved.
    current: bool,
    detail: Option<Detail>,
    /// The last read of a link's pull request from its host, of the link whose details opened
    /// last. No other is kept.
    read: Option<Read>,
    /// Set when a link's details open, so the caller asks the host for its pull request.
    asking: bool,
    /// The ticket of the last read asked for.
    tickets: u64,
    /// The ticket of the read under way, until its answer comes. One read at a time is under
    /// way, so links opened one after another don't each start a read. Another thread opening
    /// leaves it, since its answer still comes.
    running: Option<u64>,
    /// What the reader opened of the conversation, which starts over each time a link's
    /// details open.
    shown: Shown,
    /// The control in the Comments section that ↑ and ↓ chose, which Enter acts on.
    pick: Option<Control>,
    /// The control acted on and the row of the details' page it showed on. The next frame
    /// scrolls it back to that row, so what it showed or hid moves the rows around it rather
    /// than it.
    anchor: Option<(Control, usize)>,
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
        self.viewing = false;
        self.scroll = Offset::Top(0);
        self.top = 0;
        self.page = None;
        self.detail = None;
        self.read = None;
        self.asking = false;
        self.shown = Shown::default();
        self.pick = None;
    }

    /// Whether this frame has to work the links out for thread `open_id`: an event may have
    /// changed them, they are of another thread, or the environment's support changed.
    pub fn wants(&self, open_id: Option<&str>, supported: bool) -> bool {
        let Some(list) = self.list.as_ref() else {
            return true;
        };
        self.stale || list.thread_id.as_deref() != open_id || list.supported != supported
    }

    /// Takes the links a frame worked out. The selection stays on its link, and that link's
    /// details stay open at their scroll, while it is still listed. When it goes, the details
    /// close and the selection stays where it was.
    pub fn set(&mut self, list: List) {
        #[cfg(test)]
        {
            self.builds += 1;
        }
        self.stale = false;
        if self.list.as_ref() == Some(&list) {
            return;
        }
        self.list = Some(list);
        self.page = None;
        self.detail = None;
        let entries = entries_of(self.list.as_ref());
        let found = self.chosen.as_ref().and_then(|key| position(entries, key));
        if found.is_none() {
            self.viewing = false;
            self.scroll = Offset::Top(0);
            self.pick = None;
        }
        let selected = found
            .unwrap_or(self.selected)
            .min(entries.len().saturating_sub(1));
        self.chosen = entries.get(selected).map(|entry| entry.key.clone());
        self.selected = selected;
        if !self.viewing {
            self.unqueue();
        }
    }

    /// How many links show, once they are known.
    pub fn count(&self) -> Option<usize> {
        self.list.as_ref().and_then(List::count)
    }

    /// Whether the selected link's details show in the list's place.
    pub fn viewing(&self) -> bool {
        self.viewing
    }

    /// Moves the selection `delta` links, down for a positive one, and stops at the ends.
    /// While a link's details show, scrolls them `delta` rows instead, no further than the
    /// last frame showed they go, so a scroll back up after one past the end counts.
    pub fn move_by(&mut self, delta: isize) {
        if self.viewing {
            self.pick = None;
            self.scroll_rows(delta);
            return;
        }
        let count = entries_of(self.list.as_ref()).len();
        if count > 0 {
            self.select(self.selected.saturating_add_signed(delta).min(count - 1));
        }
    }

    /// Scrolls a link's details `delta` rows, no further than the last frame showed they go.
    fn scroll_rows(&mut self, delta: isize) {
        self.anchor = None;
        {
            let last = self.last_row();
            let within = |rows: usize| last.map_or(rows, |last| rows.min(last));
            let scroll = match self.scroll {
                Offset::Top(row) => Offset::Top(within(row.saturating_add_signed(delta))),
                Offset::End(up) => {
                    // Down leaves fewer rows under the page, and up more.
                    let up = up.saturating_add_signed(delta.saturating_neg());
                    Offset::End(within(up))
                }
            };
            self.scroll_to(scroll);
        }
    }

    /// ↓, or ↑ when `down` is false. In the list it selects the next link. In a link's
    /// details it chooses the next control in the Comments section where that control's rows
    /// are in view, so the text between two controls is read on the way. With none in view it
    /// scrolls a row, and the chosen control stays chosen while any of its rows show.
    pub fn step(&mut self, down: bool) {
        let delta = if down { 1 } else { -1 };
        if !self.viewing {
            self.move_by(delta);
            return;
        }
        let Some((view, targets)) = self.in_view() else {
            self.scroll_rows(delta);
            return;
        };
        let at = self
            .pick
            .and_then(|pick| targets.iter().position(|(_, control)| *control == pick));
        let next = match (at, down) {
            (Some(at), true) => targets.get(at + 1),
            (Some(at), false) => at.checked_sub(1).and_then(|at| targets.get(at)),
            (None, true) => targets.iter().find(|(rows, _)| shows(rows, &view)),
            (None, false) => targets.iter().rev().find(|(rows, _)| shows(rows, &view)),
        };
        if let Some((rows, control)) = next
            && shows(rows, &view)
        {
            self.pick = Some(*control);
            return;
        }
        let picked = at.map(|at| targets[at].0.clone());
        self.scroll_rows(delta);
        if let Some(rows) = picked
            && let Some((view, _)) = self.in_view()
            && !shows(&rows, &view)
        {
            self.pick = None;
        }
    }

    /// The rows of the details in view, where the last frame drew them and the keys since
    /// scrolled them, and the rows of each control. None until a frame drew these details.
    fn in_view(&self) -> Option<(Range<usize>, Targets)> {
        let last = self.last_row()?;
        let shape = self.page.as_ref()?.shape;
        let detail = self.detail.as_ref()?;
        let top = match self.scroll {
            Offset::Top(row) => row.min(last),
            Offset::End(up) => last - up.min(last),
        };
        // The row back to the list comes first.
        let rows = shape.height.saturating_sub(1);
        Some((top..top + rows, detail.targets.clone()))
    }

    /// Enter: shows the selected link's details, or does what the chosen control in them does,
    /// or else shows the list again.
    pub fn enter(&mut self) {
        match self.pick.filter(|_| self.viewing) {
            Some(control) => self.act(control),
            None => self.toggle(),
        }
    }

    /// Does what `control` does to the conversation in the details, and chooses it. When it
    /// goes because of that, the control that takes its place is chosen, if any.
    fn act(&mut self, control: Control) {
        let Some(read) = self.read.as_ref().filter(|read| self.reads_selected(read)) else {
            return;
        };
        let Answer::Read(conversation) = &read.conversation else {
            return;
        };
        if !conversation.has(control, &self.shown) {
            return;
        }
        let row = self.in_view().and_then(|(view, targets)| {
            let (rows, _) = targets.iter().find(|(_, c)| *c == control)?;
            shows(rows, &view).then(|| rows.start.saturating_sub(view.start))
        });
        self.shown.act(control, conversation);
        self.pick = Some(control)
            .filter(|control| conversation.has(*control, &self.shown))
            .or_else(|| control.partner())
            .filter(|control| conversation.has(*control, &self.shown));
        self.anchor = self.pick.zip(row);
        self.relay();
    }

    /// Whether `read` is of the selected link.
    fn reads_selected(&self, read: &Read) -> bool {
        entries_of(self.list.as_ref())
            .get(self.selected)
            .is_some_and(|entry| entry.key == read.key)
    }

    /// What Enter does with the chosen control, for the status line. None while none is.
    pub fn action(&self) -> Option<String> {
        let pick = self.pick.filter(|_| self.viewing)?;
        let read = self
            .read
            .as_ref()
            .filter(|read| self.reads_selected(read))?;
        match &read.conversation {
            Answer::Read(conversation) => Some(conversation.action(pick, &self.shown)),
            _ => None,
        }
    }

    /// Selects the first link, or scrolls the details to their top.
    pub fn home(&mut self) {
        if self.viewing {
            self.pick = None;
            self.scroll_to(Offset::Top(0));
        } else {
            self.move_by(isize::MIN);
        }
    }

    /// Selects the last link, or scrolls the details until their last row shows, however many
    /// rows the next frame finds they take.
    pub fn end(&mut self) {
        if self.viewing {
            self.pick = None;
            self.scroll_to(Offset::End(0));
        } else {
            self.move_by(isize::MAX);
        }
    }

    /// The furthest the details scroll, as the last frame laid them out: their rows past the
    /// ones a page shows. None until a frame drew the selected link's details.
    fn last_row(&self) -> Option<usize> {
        let shape = self.page.as_ref()?.shape;
        let detail = self.detail.as_ref()?;
        let drawn = shape.viewing && shape.selected == self.selected;
        if !drawn || detail.index != self.selected || detail.at.0 != shape.width {
            return None;
        }
        // The row back to the list comes first.
        let shown = shape.height.saturating_sub(1);
        Some(detail.lines.len().saturating_sub(shown))
    }

    /// Moves the selection, or scrolls the details, a page down, or up when `down` is false.
    pub fn page_by(&mut self, down: bool) {
        let step = self
            .page
            .as_ref()
            .filter(|page| page.shape.viewing == self.viewing)
            .map_or(1, |page| page.per_page) as isize;
        self.move_by(if down { step } else { -step });
    }

    fn select(&mut self, index: usize) {
        if index != self.selected {
            self.current = false;
        }
        self.selected = index;
        self.chosen = entries_of(self.list.as_ref())
            .get(index)
            .map(|entry| entry.key.clone());
    }

    fn scroll_to(&mut self, scroll: Offset) {
        if scroll != self.scroll {
            self.current = false;
            self.scroll = scroll;
        }
    }

    /// Shows the selected link's details in the list's place, from their top, or the list
    /// again.
    pub fn toggle(&mut self) {
        if self.viewing {
            self.back();
        } else if !entries_of(self.list.as_ref()).is_empty() {
            self.viewing = true;
            self.scroll = Offset::Top(0);
            self.current = false;
            self.asking = true;
            self.shown = Shown::default();
            self.pick = None;
        }
    }

    /// The read of the selected link's pull request that opening its details asks for. Opening
    /// them wants a read, unless `enabled` is false because the environment doesn't read pull
    /// requests or the thread names no project, or a read of the link is under way or one from
    /// the last minute stands. The details show that the read is under way until `answer`
    /// takes its answer. A read wanted while another is under way waits, and the caller calls
    /// again once that one's answer comes. None while nothing waits.
    pub fn take_ask(&mut self, enabled: bool, now: i64) -> Option<Ask> {
        if std::mem::take(&mut self.asking) && enabled && self.viewing {
            self.want(now);
        }
        if !enabled {
            self.unqueue();
            return None;
        }
        if self.running.is_some() {
            return None;
        }
        let read = self.read.as_mut().filter(|read| read.ticket.is_none())?;
        self.tickets += 1;
        read.ticket = Some(self.tickets);
        read.asked = now;
        self.running = read.ticket;
        Some(Ask {
            ticket: self.tickets,
            host: read.host.clone(),
            repository: read.repository.clone(),
            number: read.number,
        })
    }

    /// Wants a read of the selected link's pull request, unless one under way or from the last
    /// minute stands.
    fn want(&mut self, now: i64) {
        let Some(entry) = entries_of(self.list.as_ref()).get(self.selected) else {
            return;
        };
        let read = self.read.as_ref().filter(|read| read.key == entry.key);
        if read.is_some_and(|read| read.stands(now)) {
            return;
        }
        self.read = Some(Read {
            key: entry.key.clone(),
            host: entry.ask_host.clone(),
            repository: entry.ask_repository.clone(),
            number: entry.number,
            ticket: None,
            asked: now,
            answer: Answer::Reading,
            conversation: Answer::Reading,
        });
        self.relay();
    }

    /// Drops a read that waits to be asked for, once nothing shows it or it can't be asked.
    fn unqueue(&mut self) {
        if self.read.as_ref().is_some_and(|read| read.ticket.is_none()) {
            self.read = None;
            self.relay();
        }
    }

    /// Takes part of the answer to read `ticket`: the pull request's detail or conversation,
    /// or why reading it failed. Says whether it is the read the details wait for. An answer
    /// to an earlier read, or to a read of another thread's link, is dropped. Once the read is
    /// done, another can be asked for.
    pub fn answer(&mut self, ticket: u64, part: Part) -> bool {
        if let Part::Done = part {
            if self.running == Some(ticket) {
                self.running = None;
            }
            return false;
        }
        let Some(read) = self
            .read
            .as_mut()
            .filter(|read| read.ticket == Some(ticket))
        else {
            return false;
        };
        match part {
            Part::Detail(Ok(detail)) => read.answer = Answer::Read(Box::new(Summary::of(&detail))),
            Part::Detail(Err(failure)) => read.answer = Answer::Failed(bounded(&failure, FAILURE)),
            Part::Activity(Ok(activity)) => {
                read.conversation = Answer::Read(Box::new(Conversation::of(&activity)));
            }
            Part::Activity(Err(failure)) => {
                read.conversation = Answer::Failed(bounded(&failure, FAILURE));
            }
            Part::Done => {}
        }
        // The rows move, so no control stays chosen.
        self.pick = None;
        self.relay();
        true
    }

    /// Notes that the details' rows changed, so the next frame lays them out again and a click
    /// before it lands on nothing.
    fn relay(&mut self) {
        self.detail = None;
        self.page = None;
        self.current = false;
    }

    /// Shows the list again in place of a link's details, with that link still selected. Says
    /// whether the details showed.
    pub fn back(&mut self) -> bool {
        if !self.viewing {
            return false;
        }
        self.viewing = false;
        self.scroll = Offset::Top(0);
        self.current = false;
        self.pick = None;
        self.unqueue();
        true
    }

    /// A click on row `offset` of the page the last frame drew. On a link's rows it selects
    /// the link, or shows its details when it is selected already, and on the row above the
    /// details it shows the list again. Says whether it landed on one of those. Once the
    /// selection or the scroll moved, a click lands on nothing until a frame draws the page.
    pub fn click(&mut self, offset: usize) -> bool {
        let hit = self
            .page
            .as_ref()
            .filter(|_| self.current)
            .and_then(|page| page.rows.get(offset).copied().flatten());
        match hit {
            Some(Spot::Link(index)) if index == self.selected => self.toggle(),
            Some(Spot::Link(index)) => self.select(index),
            Some(Spot::Back) => {
                self.back();
            }
            Some(Spot::Control(control)) => self.act(control),
            None => return false,
        }
        true
    }

    /// The rows the list would like at `width`, for a dialog that is as tall as its text.
    pub fn wanted(&mut self, width: usize, now: i64, theme: &Theme) -> usize {
        let count = entries_of(self.list.as_ref()).len();
        if count == 0 {
            return message(self.list.as_ref(), width, theme).len();
        }
        if self.viewing {
            // The row back to the list, then the details.
            return 1 + self.prepare_detail(width, now, theme);
        }
        count * ENTRY_ROWS + 1
    }

    /// The rows of the selected link's details at `width`, worked out again only for another
    /// link, width or minute.
    fn prepare_detail(&mut self, width: usize, now: i64, theme: &Theme) -> usize {
        let (index, at) = (self.selected, (width, now.div_euclid(60_000)));
        let cached = self.detail.as_ref().map(|detail| (detail.index, detail.at));
        if cached != Some((index, at)) {
            let lines = match entries_of(self.list.as_ref()).get(index) {
                Some(entry) => {
                    let read = self.read.as_mut().filter(|read| read.key == entry.key);
                    if let Some(read) = read {
                        if let Answer::Read(summary) = &mut read.answer {
                            summary.lay_out(width, theme);
                        }
                        if let Answer::Read(conversation) = &mut read.conversation {
                            conversation.lay_out(width, theme, &self.shown);
                        }
                    }
                    let read = self.read.as_ref().filter(|read| read.key == entry.key);
                    detail_lines(entry, read, &self.shown, width, now, theme)
                }
                None => (Vec::new(), Vec::new()),
            };
            let (lines, targets) = lines;
            self.detail = Some(Detail {
                index,
                at,
                lines,
                targets,
            });
        }
        self.detail.as_ref().map_or(0, |detail| detail.lines.len())
    }

    /// The page a frame shows in `height` rows of `width` columns, laid out again only when
    /// its shape changed: the list with the selected link on it, or the selected link's
    /// details, scrolled no further than their last row shows. Clicks land on it until the
    /// selection or the scroll moves.
    pub fn page(
        &mut self,
        width: usize,
        height: usize,
        now: i64,
        focused: bool,
        theme: &Theme,
    ) -> &Page {
        let mut scroll = 0;
        if self.viewing {
            let total = self.prepare_detail(width, now, theme);
            let anchored = self
                .anchor
                .take()
                .filter(|(control, _)| self.pick == Some(*control));
            if let Some((control, row)) = anchored
                && let Some(detail) = self.detail.as_ref()
                && let Some((rows, _)) = detail.targets.iter().find(|(_, c)| *c == control)
            {
                self.scroll = Offset::Top(rows.start.saturating_sub(row));
            }
            // The row back to the list comes first.
            let last = total.saturating_sub(height.saturating_sub(1));
            scroll = match self.scroll {
                Offset::Top(row) => row.min(last),
                Offset::End(up) => last - up.min(last),
            };
            self.scroll = Offset::Top(scroll);
        }
        let shape = Shape {
            width,
            height,
            top: self.top,
            selected: self.selected,
            viewing: self.viewing,
            scroll,
            minute: now.div_euclid(60_000),
            pick: self.pick.filter(|_| self.viewing),
            focused,
        };
        if self.page.as_ref().map(|page| page.shape) != Some(shape) {
            #[cfg(test)]
            {
                self.pages += 1;
            }
            let list = self.list.as_ref();
            let page = if shape.viewing {
                view(list, self.detail.as_ref(), shape, theme)
            } else {
                lay_out(list, shape, now, theme)
            };
            self.top = page.shape.top;
            self.page = Some(page);
        }
        self.current = true;
        self.page.as_ref().expect("laid out above")
    }
}

/// The page of `list` for `shape`: as many links as fit from the first shown, moved so the
/// selected one shows, and the footer on the last row. A list without links says why.
fn lay_out(list: Option<&List>, mut shape: Shape, now: i64, theme: &Theme) -> Page {
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
    // The links whose rows all fit, or the one whose first rows do.
    let fit = (room / ENTRY_ROWS).max(1);
    let selected = shape.selected.min(count - 1);
    // The selected link shows, and a page that reaches the end of the list starts as early as
    // it can.
    let top = shape
        .top
        .min(selected)
        .max((selected + 1).saturating_sub(fit))
        .min(count.saturating_sub(fit));
    let mut index = top;
    while index < count && page.lines.len() < room {
        let lines = entry_lines(&entries[index], width, index == selected, now, theme);
        for line in lines.into_iter().take(room - page.lines.len()) {
            page.rows.push(Some(Spot::Link(index)));
            page.lines.push(line);
        }
        index += 1;
    }
    page.above = top > 0;
    page.below = (count - top) * ENTRY_ROWS > room;
    page.per_page = fit;
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

/// The page of the selected link's details for `shape`: the row back to the list, with where
/// the link is in it, then as many rows of the details as fit from row `shape.scroll`.
fn view(list: Option<&List>, detail: Option<&Detail>, shape: Shape, theme: &Theme) -> Page {
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
    let lines: &[Line<'static>] = match detail {
        Some(detail) if detail.index == shape.selected => &detail.lines,
        _ => &[],
    };
    let back = Style::new().fg(theme.primary).add_modifier(Modifier::BOLD);
    let back = fitted(vec![Span::styled("← List", back)], width);
    let count = entries_of(list).len();
    let place = format!("{} of {count}", shape.selected + 1);
    let place = vec![Span::styled(place, Style::new().fg(theme.muted))];
    page.lines.push(row(back, place, width));
    page.rows.push(Some(Spot::Back));
    let room = height - 1;
    let targets: &[(Range<usize>, Control)] = match detail {
        Some(detail) if detail.index == shape.selected => &detail.targets,
        _ => &[],
    };
    // The controls from the first with a row in view.
    let mut targets = targets[targets.partition_point(|(rows, _)| rows.end <= shape.scroll)..]
        .iter()
        .peekable();
    for (at, line) in lines.iter().enumerate().skip(shape.scroll).take(room) {
        while targets.next_if(|(rows, _)| rows.end <= at).is_some() {}
        let control = targets
            .peek()
            .filter(|(rows, _)| rows.contains(&at))
            .map(|(_, control)| *control);
        // The chosen control shows highlighted while the details have the keyboard.
        if control.is_some() && control == shape.pick && shape.focused {
            page.lines
                .push(Line::from(with_bg(line.spans.clone(), theme.highlight)));
        } else {
            page.lines.push(line.clone());
        }
        page.rows.push(control.map(Spot::Control));
    }
    page.above = shape.scroll > 0;
    page.below = shape.scroll + room < lines.len();
    page.per_page = room.max(1);
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

/// `text` in `style`, wrapped at `width` to at most `rows` rows, the last cut with `…`.
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

/// `text` in `style`, wrapped at `width` to every row it takes.
fn whole(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    wrapped(text, width, usize::MAX, style)
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

/// What a link's second row says before its time, a part each: the stack it heads, its state,
/// then while it is open whether T3 watches it, whether it conflicts, and its checks and
/// review, each only when the host reported it.
fn status_parts(entry: &Entry, theme: &Theme) -> Vec<Span<'static>> {
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
    parts
        .into_iter()
        .map(|(text, color)| Span::styled(text, Style::new().fg(color)))
        .collect()
}

/// `parts` with `gap` between each two.
fn joined(parts: Vec<Span<'static>>, gap: Span<'static>) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(2 * parts.len());
    for part in parts {
        if !spans.is_empty() {
            spans.push(gap.clone());
        }
        spans.push(part);
    }
    spans
}

/// `parts` on as few rows of `width` as they take, `gap` between two on a row. A part wider
/// than a row wraps on rows of its own.
fn flowed(
    parts: Vec<Span<'static>>,
    gap: &'static str,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for part in parts {
        let columns = part.content.width();
        if !spans.is_empty() && used + gap.width() + columns <= width {
            spans.push(Span::styled(gap, Style::new().fg(theme.muted)));
            spans.push(part);
            used += gap.width() + columns;
            continue;
        }
        if !spans.is_empty() {
            lines.push(Line::from(std::mem::take(&mut spans)));
        }
        if columns <= width {
            used = columns;
            spans.push(part);
        } else {
            lines.extend(whole(&part.content, width, part.style));
        }
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
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
    let diff = joined(diff_parts(snapshot, theme), Span::raw(" "));
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
    let parts = joined(status_parts(entry, theme), Span::styled(" · ", muted));
    status.extend(parts);
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

/// The lines a link adds and removes, a part each, as the desktop's `PullRequestDiffStat`
/// shows them, or nothing when the host counted none, so a missing count never reads as an
/// empty change. A count the host left out shows as nothing rather than as zero.
fn diff_parts(snapshot: Option<&Snapshot>, theme: &Theme) -> Vec<Span<'static>> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let (additions, deletions) = (snapshot.additions, snapshot.deletions);
    if additions.unwrap_or(0) == 0 && deletions.unwrap_or(0) == 0 {
        return Vec::new();
    }
    let mut parts = Vec::new();
    if let Some(additions) = additions {
        let text = format!("+{}", grouped(additions));
        parts.push(Span::styled(text, Style::new().fg(theme.success)));
    }
    if let Some(deletions) = deletions {
        let text = format!("-{}", grouped(deletions));
        parts.push(Span::styled(text, Style::new().fg(theme.error_fg)));
    }
    parts
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

/// A link's details, which show in the list's place: its number and whole title, its status
/// and the lines it changes, then what its host said of it when the details read it, then each
/// under its label its URL, whole as plain text to copy, its repository, its branches, its
/// author, and who linked it and when it last changed and synced, then what watching it does
/// and the stack it heads. Each value wraps to every row it takes, so the reader can scroll to
/// all of it, and each was bounded when it was read.
fn detail_lines(
    entry: &Entry,
    read: Option<&Read>,
    shown: &Shown,
    width: usize,
    now: i64,
    theme: &Theme,
) -> (Vec<Line<'static>>, Targets) {
    if width == 0 {
        return (Vec::new(), Vec::new());
    }
    let muted = Style::new().fg(theme.muted);
    let value = Style::new().fg(theme.fg);
    let info = Style::new().fg(theme.info);
    let snapshot = entry.snapshot();
    let heading = format!("#{} {}", entry.number, entry.title());
    let mut lines = whole(&heading, width, value.add_modifier(Modifier::BOLD));
    lines.extend(flowed(status_parts(entry, theme), " · ", width, theme));
    lines.extend(flowed(diff_parts(snapshot, theme), " ", width, theme));
    let mut targets = Vec::new();
    if let Some(read) = read {
        let at = lines.len();
        let (host, controls) = host_lines(entry, read, shown, width, now, theme);
        lines.extend(host);
        let moved = |rows: Range<usize>| rows.start + at..rows.end + at;
        targets.extend(controls.into_iter().map(|(rows, c)| (moved(rows), c)));
    }

    // Each section is a blank row, its label when it has one, then each value in its style,
    // wrapped to every row it takes.
    let mut section = |label: Option<&str>, values: &[(String, Style)]| {
        lines.push(Line::default());
        if let Some(label) = label {
            lines.push(Line::styled(clip(label, width), muted));
        }
        for (text, style) in values {
            lines.extend(whole(text, width, *style));
        }
    };
    let mut url = vec![(entry.url.clone(), info)];
    if entry.url_cut {
        let note = format!(
            "Cut short: T3 sent a URL longer than the {} characters t3term keeps.",
            grouped(URL as u64)
        );
        url.push((note, muted));
    }
    section(Some("URL"), &url);
    let place = format!("{}/{}", entry.host, entry.repository);
    section(Some("Repository"), &[(place, value)]);
    if let Some(snapshot) = snapshot {
        let branches = format!("{} → {}", snapshot.head, snapshot.base);
        section(Some("Branches"), &[(branches, value)]);
    }
    if let Some(snapshot) = snapshot
        && let Some(author) = snapshot.author.as_deref()
    {
        let bot = if snapshot.bot { " (bot)" } else { "" };
        section(Some("Author"), &[(format!("{author}{bot}"), value)]);
    }
    let linked = match time_ago(entry.linked_at.as_deref(), now) {
        Some(ago) => format!("{} · {ago}", entry.source.label()),
        None => entry.source.label().to_string(),
    };
    let mut activity = vec![(linked, value)];
    if let Some(snapshot) = snapshot {
        if let Some(ago) = time_ago(snapshot.updated.as_deref(), now) {
            activity.push((format!("Updated {ago}"), value));
        }
        if let Some(ago) = time_ago(snapshot.synced.as_deref(), now) {
            activity.push((format!("Synced {ago}"), value));
        }
    }
    section(Some("Activity"), &activity);
    if entry.open() && entry.watching {
        section(None, &[(WATCHING.to_string(), info)]);
    }
    if let Some(chain) = entry.chain {
        let size = chain.size;
        let text = if chain.native {
            format!("GitHub stack of {size}: {NATIVE_STACK}")
        } else {
            format!("{size} pull requests chained by base branch.")
        };
        section(None, &[(text, muted)]);
    }
    (lines, targets)
}

/// What the link's host said of its pull request, as the desktop's Summary tab shows it: that
/// the read is under way, why it failed, or when it was read, then a warning while the branch
/// is behind its base, the reviewers, the labels, the checks and the description. The checks
/// come before the description, which the desktop folds away and these details can't.
fn host_lines(
    entry: &Entry,
    read: &Read,
    shown: &Shown,
    width: usize,
    now: i64,
    theme: &Theme,
) -> (Vec<Line<'static>>, Targets) {
    let muted = Style::new().fg(theme.muted);
    let value = Style::new().fg(theme.fg);
    let warning = Style::new().fg(theme.warning_fg);
    let mut lines = vec![Line::default()];
    let summary = match &read.answer {
        Answer::Reading => {
            let reading = format!("Reading from {}…", entry.host);
            lines.extend(whole(&reading, width, muted));
            return (lines, Vec::new());
        }
        Answer::Failed(failure) => {
            lines.extend(whole(failure, width, warning));
            return (lines, Vec::new());
        }
        Answer::Read(summary) => summary,
    };
    let at = match age(read.asked, now).as_str() {
        "now" => "just now".to_string(),
        ago => format!("{ago} ago"),
    };
    lines.extend(whole(
        &format!("Read from {} {at}", entry.host),
        width,
        muted,
    ));
    if let Some(behind) = summary.behind {
        let by = match behind {
            Some(1) => " by 1 commit".to_string(),
            Some(count) => format!(" by {} commits", grouped(count)),
            None => String::new(),
        };
        let text = format!("This branch is out-of-date with {}{by}.", summary.base);
        lines.extend(whole(&text, width, warning));
    }
    let mut section = |label: &str, rows: Vec<Line<'static>>| {
        lines.push(Line::default());
        lines.push(Line::styled(clip(label, width), muted));
        lines.extend(rows);
    };
    let conversation = match &read.conversation {
        Answer::Read(conversation) => Some(&**conversation),
        _ => None,
    };
    let reviewers = conversation::reviewer_lines(&summary.reviewers, conversation, width, theme);
    section("Reviewers", reviewers);
    if let Some(labels) = summary.labels.as_ref() {
        let labels = match labels.is_empty() {
            true => whole("None", width, muted),
            false => whole(&labels.join(", "), width, value),
        };
        section("Labels", labels);
    }
    let mut checks: Vec<Line<'static>> = match summary.checks.is_empty() {
        true => whole("No checks reported.", width, muted),
        false => summary
            .checks
            .iter()
            .flat_map(|check| check_lines(check, width, theme))
            .collect(),
    };
    if summary.cut {
        let note = "T3 sent more reviewers, labels or checks than t3term keeps.";
        checks.extend(whole(note, width, muted));
    }
    section("Checks", checks);
    let mut body: Vec<Line<'static>> = match summary.laid_out.as_ref() {
        Some((at, laid_out)) if *at == width => laid_out.clone(),
        _ => Vec::new(),
    };
    if summary.body_cut {
        let note = format!(
            "Cut short: T3 sent a description longer than the {} bytes t3term keeps.",
            grouped(pull_request::BODY as u64)
        );
        body.extend(whole(&note, width, muted));
    }
    section("Description", body);
    // The Comments section's heading holds its order switch, so it lays its heading out.
    let comments = conversation::section(&read.conversation, shown, width, now, theme);
    lines.push(Line::default());
    let at = lines.len();
    lines.extend(comments.lines);
    let targets = comments.targets.into_iter();
    let targets = targets.map(|(rows, control)| (rows.start + at..rows.end + at, control));
    (lines, targets.collect())
}

/// Whether any of `rows` is in `view`.
fn shows(rows: &Range<usize>, view: &Range<usize>) -> bool {
    rows.start < view.end && view.start < rows.end
}

/// A check's rows: its mark and name, wrapped, with its status at the end of the first row. A
/// row too narrow for the status beside a few columns of the name has it on a row of its own.
fn check_lines(check: &CheckRow, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let (mark, status, color) = check.shown(theme);
    let value = Style::new().fg(theme.fg);
    let muted = Style::new().fg(theme.muted);
    let room = width.saturating_sub(2 + status.width() + 1);
    if room < 4 {
        let mut lines = whole(&format!("{mark} {}", check.name), width, value);
        lines.extend(whole(status, width, Style::new().fg(color)));
        return lines;
    }
    let mut pieces = wrap(&check.name, room);
    if pieces.is_empty() {
        pieces.push(String::new());
    }
    pieces
        .iter()
        .enumerate()
        .map(|(at, piece)| {
            let lead = match at {
                0 => Span::styled(mark, Style::new().fg(color)),
                _ => Span::raw(" "),
            };
            let left = vec![lead, Span::raw(" "), Span::styled(clip(piece, room), value)];
            let right = match at {
                0 => vec![Span::styled(status, muted)],
                _ => Vec::new(),
            };
            row(left, right, width)
        })
        .collect()
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

    /// Each of `links`' numbers, how many layers of its stack lie under it and the stack its
    /// bottom layer names, by number, so the order T3 sent them in doesn't change it.
    fn layout_of(links: &[Value]) -> Vec<(u64, usize, Option<Chain>)> {
        let list = list_of(&thread(links.to_vec()));
        let mut shown: Vec<_> = entries(&list)
            .iter()
            .map(|entry| (entry.number, entry.depth, entry.chain))
            .collect();
        shown.sort_by_key(|&(number, ..)| number);
        shown
    }

    /// Link `number` in no stack of two or more, as `layout_of` has it.
    fn alone(number: u64) -> (u64, usize, Option<Chain>) {
        (number, 0, None)
    }

    /// Every order of `links`.
    fn orders(links: &[Value]) -> Vec<Vec<Value>> {
        if links.len() < 2 {
            return vec![links.to_vec()];
        }
        let mut all = Vec::new();
        for (at, first) in links.iter().enumerate() {
            let mut rest = links.to_vec();
            rest.remove(at);
            for mut order in orders(&rest) {
                order.insert(0, first.clone());
                all.push(order);
            }
        }
        all
    }

    /// The numbers of `links`, in the order T3 sent them.
    fn sent(links: &[Value]) -> Vec<&Value> {
        links.iter().map(|link| &link["number"]).collect()
    }

    fn key_text(entry: &Entry) -> String {
        let key = &entry.key;
        format!("{}/{}#{}", key.host, key.repository, key.number)
    }

    /// The rows of `list`'s first page at `width` by `height`.
    fn page_rows(list: List, width: usize, height: usize) -> Vec<String> {
        let mut pulls = PullRequests::default();
        pulls.set(list);
        rows_of(&pulls.page(width, height, now(), true, &theme()).lines)
    }

    /// The links whose rows `page` shows.
    fn links_on(page: &Page) -> HashSet<usize> {
        page.rows
            .iter()
            .filter_map(|spot| match spot {
                Some(Spot::Link(index)) => Some(*index),
                _ => None,
            })
            .collect()
    }

    /// The rows of the selected link's details at the width the last frame drew them.
    fn detail_rows(pulls: &PullRequests) -> Vec<String> {
        pulls
            .detail
            .as_ref()
            .map(|detail| rows_of(&detail.lines))
            .unwrap_or_default()
    }

    #[test]
    fn a_synced_link_shows_what_the_host_reported() {
        let pull = link("krishhgg/t3term", 31, "feature", "master");
        let rows = page_rows(list_of(&thread(vec![pull.clone()])), 44, 4);
        let gap = " ".repeat(21);
        assert_eq!(rows[0], format!("● #31 Change 31{gap}+821 -51"));
        assert_eq!(rows[1], "  Open · Checks passing · Approved   46m ago");
        assert_eq!(rows[2], "  krishhgg/t3term · feature → master");
        assert_eq!(rows[3], "1 open · 1 linked · synced 1m ago");
        // Its details show in the list's place, under the row back to it.
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![pull])));
        pulls.toggle();
        let rows = rows_of(&pulls.page(44, 30, now(), true, &theme()).lines);
        assert_eq!(rows[0], format!("← List{}1 of 1", " ".repeat(32)));
        let expected = [
            "#31 Change 31",
            "Open · Checks passing · Approved",
            "+821 -51",
            "",
            "URL",
            "https://github.com/krishhgg/t3term/pull/31",
            "",
            "Repository",
            "github.com/krishhgg/t3term",
            "",
            "Branches",
            "feature → master",
            "",
            "Author",
            "krishhgg",
            "",
            "Activity",
            "Linked by the agent · 2h ago",
            "Updated 46m ago",
            "Synced 1m ago",
        ];
        assert_eq!(rows[1..], expected);
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
    fn links_that_build_on_a_cycle_stack_apart_from_it_in_any_order() {
        // #1 is on #2's head and #2 on #1's, and #3 builds on #1. The walk from #3 comes back
        // to #1, so no link stacks on another, whatever order T3 sends them in.
        let pair = [
            link("o/r", 1, "b", "c"),
            link("o/r", 2, "c", "b"),
            link("o/r", 3, "d", "b"),
        ];
        // #3 on #2 instead.
        let mut other = pair.clone();
        other[2] = link("o/r", 3, "d", "c");
        let three = [alone(1), alone(2), alone(3)];
        for links in [pair.clone(), other] {
            for order in orders(&links) {
                assert_eq!(layout_of(&order), three, "{:?}", sent(&order));
            }
        }
        // None of them reads as a layer or names a stack.
        let rows = page_rows(list_of(&thread(pair.to_vec())), 40, 10);
        for at in [0, 3, 6] {
            assert!(rows[at].starts_with("● #"), "{rows:#?}");
        }
        assert!(!rows.join("\n").contains("chained"), "{rows:#?}");

        // #5 is on its own head, and #6 builds on #5.
        let own = [link("o/r", 5, "s", "s"), link("o/r", 6, "t", "s")];
        let two = [alone(5), alone(6)];
        for order in orders(&own) {
            assert_eq!(layout_of(&order), two, "{:?}", sent(&order));
        }
        assert_eq!(layout_of(&own[..1]), [alone(5)]);

        // #4 builds on #3, which builds on the cycle. #3 and #4 still stack, from #3 up.
        let mut longer = pair.to_vec();
        longer.push(link("o/r", 4, "e", "d"));
        let derived = Chain {
            native: false,
            size: 2,
        };
        let expected = [alone(1), alone(2), (3, 0, Some(derived)), (4, 1, None)];
        for order in orders(&longer) {
            assert_eq!(layout_of(&order), expected, "{:?}", sent(&order));
        }

        // Beside a chain, a head two links share and a GitHub stack, which stack as before
        // whichever link comes first.
        let native = |number: u64| {
            let mut pull = link("o/r", number, &format!("n{number}"), "main");
            pull["stack"] = json!({"kind": "native", "id": "s1",
                "layers": [{"number": 21}, {"number": 22}]});
            pull
        };
        let mut mixed = [
            link("o/r", 1, "x", "y"),
            link("o/r", 2, "y", "x"),
            link("o/r", 3, "z", "x"),
            link("o/r", 11, "a", "main"),
            link("o/r", 12, "b", "a"),
            native(22),
            native(21),
            link("o/r", 31, "same", "main"),
            link("o/r", 32, "same", "main"),
            link("o/r", 33, "top", "same"),
        ];
        let github = Chain {
            native: true,
            size: 2,
        };
        let expected = [
            alone(1),
            alone(2),
            alone(3),
            (11, 0, Some(derived)),
            (12, 1, None),
            (21, 0, Some(github)),
            (22, 1, None),
            alone(31),
            alone(32),
            alone(33),
        ];
        for _ in 0..mixed.len() {
            mixed.rotate_left(1);
            assert_eq!(layout_of(&mixed), expected, "{:?}", sent(&mixed));
            let reversed: Vec<Value> = mixed.iter().rev().cloned().collect();
            assert_eq!(layout_of(&reversed), expected, "{:?}", sent(&reversed));
        }
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
    fn identities_are_bounded_before_they_are_trimmed() {
        // Millions of spaces round a short host or repository make it too long to read, and
        // it is never trimmed or lowercased whole.
        let pad = " ".repeat(5_000_000);
        let mut host = link("o/r", 1, "h", "b");
        host["host"] = json!(format!("{pad}github.com{pad}"));
        let mut repository = link("o/r", 2, "h2", "b");
        repository["repository"] = json!(format!("{pad}o/r{pad}"));
        let legacy = json!({"id": "t", "linkedPullRequest": {"projectId": "p",
            "repository": format!("{pad}o/r"), "number": 3,
            "url": "https://github.com/o/r/pull/3"}});
        let list = list_of(&thread(vec![host, repository, link("o/r", 4, "h4", "b")]));
        assert_eq!(numbers(&list), [(4, 0)]);
        let rows = page_rows(list, 60, 4);
        assert_eq!(rows[3], "1 open · 1 linked · synced 1m ago · 2 unreadable");
        assert_eq!(list_of(&legacy).count(), Some(0));
        // The bound is on the text as T3 sent it, padding and all.
        let over = format!("{}github.com", " ".repeat(KEY));
        assert_eq!(Key::of(&over, "o/r", 1), None);
        assert_eq!(Key::of("github.com", &over, 1), None);
        let at = format!("{}GitHub.COM", " ".repeat(KEY - "GitHub.COM".len()));
        let key = Key::of(&at, " O/R ", 1).expect("at the bound");
        assert_eq!(
            (key.host.as_str(), key.repository.as_str()),
            ("github.com", "o/r")
        );
        // Ordinary text is trimmed and lowercased as before, and empty text is no identity.
        let key = Key::of(" GitHub.COM ", " Owner/Repo ", 7).expect("ordinary");
        let expected = Key {
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 7,
        };
        assert_eq!(key, expected);
        assert_eq!(Key::of("   ", "o/r", 1), None);
        assert_eq!(Key::of("github.com", "", 1), None);
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
        // The URL was cut, and says so.
        let url = format!("https://github.com/{}…", "u".repeat(URL - 19));
        assert_eq!((entry.url.as_str(), entry.url_cut), (url.as_str(), true));

        let mut pulls = PullRequests::default();
        pulls.set(list);
        let theme = theme();
        let rows = rows_of(&pulls.page(40, 40, now(), true, &theme).lines);
        let start = "● #1 ]52;c;cHduZWQ=[2J👩\u{200d}💻e\u{301}名";
        assert!(rows[0].starts_with(start), "{rows:#?}");
        // The list and the link's details fit every width and height.
        for viewing in [false, true] {
            if viewing {
                pulls.toggle();
            }
            for width in [1, 2, 3, 7, 12, 20, 33, 52] {
                for height in [1, 2, 5, 40] {
                    let page = pulls.page(width, height, now(), true, &theme);
                    assert!(page.lines.len() <= height);
                    for line in &page.lines {
                        let text = line.to_string();
                        assert!(line.width() <= width, "{text:?} at {width}x{height}");
                        assert!(!text.contains(char::is_control), "{text:?}");
                        assert!(!text.contains("evil"), "{text:?}");
                    }
                }
            }
        }
        // Tall enough, the details show whole: every character of the URL t3term kept, wrapped
        // with nothing between, then the note that it was cut.
        let page = pulls.page(40, 2_000, now(), true, &theme);
        assert!(!page.above && !page.below);
        let rows = rows_of(&page.lines);
        assert!(rows[0].starts_with("← List") && rows[0].ends_with("1 of 1"));
        assert!(rows[1..].iter().all(|row| !row.contains("evil")));
        let from = rows.iter().position(|row| row == "URL").expect("the URL") + 1;
        let to = rows.iter().position(|row| row.starts_with("Cut short:"));
        let to = to.expect("the note");
        assert_eq!(rows[from..to].concat(), url);
        // The 2,048 characters and the … on rows of 40.
        assert_eq!(to - from, (URL + 1).div_ceil(40), "{rows:#?}");
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
        let page = pulls.page(36, 31, now(), true, &theme);
        // Thirty rows of links and the footer: ten links laid out of a thousand.
        let laid = links_on(page);
        assert_eq!(laid.len(), 10);
        assert!(page.below && !page.above);
        let footer = page.lines.last().map(Line::to_string);
        let expected = "1000 open · 1000 linked · synced 1m…";
        assert_eq!(footer.as_deref(), Some(expected));
        // Frames of the same shape lay nothing out again.
        for _ in 0..5 {
            pulls.page(36, 31, now(), true, &theme);
        }
        assert_eq!((pulls.builds, pulls.pages), (1, 1));
        // The end of the list, a page at a time and then at once.
        pulls.page_by(true);
        assert_eq!(pulls.selected, 10);
        pulls.end();
        let page = pulls.page(36, 31, now(), true, &theme);
        let laid = links_on(page);
        let last: HashSet<usize> = (990..1000).collect();
        assert_eq!(laid, last);
        assert!(page.above && !page.below);
        assert_eq!(pulls.pages, 2);
        pulls.home();
        assert_eq!((pulls.selected, pulls.top), (0, 990));
        pulls.page(36, 31, now(), true, &theme);
        assert_eq!(pulls.top, 0);
        // Wider, the footer counts the links past the first thousand.
        let page = pulls.page(80, 31, now(), true, &theme);
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
        assert_eq!((pulls.selected, pulls.viewing), (1, true));
        let rows = rows_of(&pulls.page(40, 10, now(), true, &theme).lines);
        assert_eq!(rows[0], format!("← List{}2 of 3", " ".repeat(28)));
        let url = "https://github.com/o/r/pull/2";
        assert!(rows.iter().any(|row| row == url), "{rows:#?}");
        // A newer link comes first, and the selection and the details stay on #2, where the
        // reader scrolled them.
        pulls.move_by(2);
        let four = vec![dated(1), dated(2), dated(3), dated(4)];
        pulls.set(list_of(&thread(four)));
        let after = (pulls.selected, pulls.viewing, pulls.scroll);
        assert_eq!(after, (2, true, Offset::Top(2)));
        let rows = rows_of(&pulls.page(40, 10, now(), true, &theme).lines);
        assert!(rows[0].ends_with("3 of 4"), "{rows:#?}");
        assert_eq!(rows[4], url, "{rows:#?}");
        // #2 goes. The selection stays where it was, and the list shows again.
        let without = vec![dated(1), dated(3), dated(4)];
        pulls.set(list_of(&thread(without.clone())));
        let after = (pulls.selected, pulls.viewing, pulls.scroll);
        assert_eq!(after, (2, false, Offset::Top(0)));
        assert_eq!(pulls.chosen.as_ref().map(|key| key.number), Some(1));
        // The same list again lays nothing out again.
        pulls.page(40, 30, now(), true, &theme);
        let pages = pulls.pages;
        pulls.set(list_of(&thread(without)));
        pulls.page(40, 30, now(), true, &theme);
        assert_eq!(pulls.pages, pages);
        // Only an event, another thread or another environment's support works them out again.
        assert!(!pulls.wants(Some("t"), true));
        assert!(pulls.wants(Some("t"), false));
        assert!(pulls.wants(Some("u"), true));
        assert!(pulls.wants(None, true));
        pulls.mark();
        assert!(pulls.wants(Some("t"), true));
        // Another thread starts at its top, on its list.
        pulls.toggle();
        pulls.move_by(3);
        pulls.follow();
        assert!(pulls.wants(Some("u"), true));
        let after = (pulls.selected, pulls.viewing, pulls.scroll, pulls.top);
        assert_eq!(after, (0, false, Offset::Top(0), 0));
    }

    #[test]
    fn a_links_details_scroll_to_their_last_row_and_lead_back_to_the_list() {
        let mut pulls = PullRequests::default();
        let theme = theme();
        // Every field the contract has, each long: a title of many words, a URL longer than
        // t3term keeps, long branches, a bot author, a watch and a GitHub stack of two.
        let long = |number: u64| {
            let head = format!("feature/{}{number}", "branch-".repeat(40));
            let mut pull = link("o/r", number, &head, "main");
            let query = "q".repeat(3 * URL);
            pull["url"] = json!(format!("https://github.com/o/r/pull/{number}?{query}"));
            pull["snapshot"]["title"] = json!("A long title ".repeat(60));
            pull["snapshot"]["author"] = json!({"login": "renovate[bot]", "isBot": true});
            pull["watch"] = json!({"startedAt": NOW});
            pull["stack"] = json!({"kind": "native", "id": "s1",
                "layers": [{"number": 1}, {"number": 2}]});
            pull
        };
        pulls.set(list_of(&thread(vec![long(2), long(1)])));
        assert_eq!(pulls.wanted(30, now(), &theme), 2 * ENTRY_ROWS + 1);
        // Enter on the stack's bottom layer shows its details from their top.
        pulls.toggle();
        let page = pulls.page(30, 8, now(), true, &theme);
        assert!(!page.above && page.below);
        let rows = rows_of(&page.lines);
        assert_eq!(rows[0], format!("← List{}1 of 2", " ".repeat(18)));
        assert!(rows[1].starts_with("#1 A long title"), "{rows:#?}");
        let all = detail_rows(&pulls);
        let total = all.len();
        assert_eq!(pulls.wanted(30, now(), &theme), 1 + total);
        for shown in [
            "Stack of 2 · Open · Watching",
            "Checks passing · Approved",
            "Repository",
            "Branches",
            "renovate[bot] (bot)",
            "Linked by the agent · 2h ago",
            "Updated 46m ago",
            "Synced 1m ago",
        ] {
            assert!(all.iter().any(|row| row == shown), "{shown:?} in {all:#?}");
        }
        let text = all.join(" ");
        assert!(
            text.contains("Cut short: T3 sent a URL longer than"),
            "{all:#?}"
        );
        assert!(text.contains("Watching: the agent wakes"), "{all:#?}");
        assert!(
            text.ends_with("merging a layer lands the ones below it."),
            "{all:#?}"
        );

        // ↓ scrolls a row and PageDown the seven rows a page shows.
        pulls.move_by(1);
        pulls.page(30, 8, now(), true, &theme);
        pulls.page_by(true);
        assert_eq!(pulls.scroll, Offset::Top(1 + 7));
        // End reaches the last row, and no further.
        pulls.end();
        let page = pulls.page(30, 8, now(), true, &theme);
        assert!(page.above && !page.below);
        let rows = rows_of(&page.lines);
        assert!(rows[0].starts_with("← List"), "{rows:#?}");
        assert_eq!(rows[1..], all[total - 7..]);
        assert_eq!(pulls.scroll, Offset::Top(total - 7));
        pulls.move_by(1);
        pulls.page(30, 8, now(), true, &theme);
        assert_eq!(pulls.scroll, Offset::Top(total - 7));
        // ↑ and PageUp scroll back, and Home goes to the top.
        pulls.move_by(-1);
        pulls.page_by(false);
        assert_eq!(pulls.scroll, Offset::Top(total - 7 - 1 - 7));
        pulls.home();
        assert_eq!(pulls.scroll, Offset::Top(0));
        // Back to the list, with the link still selected. Its details show from their top
        // again.
        pulls.end();
        assert!(pulls.back());
        assert!(!pulls.back());
        let after = (pulls.selected, pulls.viewing, pulls.scroll);
        assert_eq!(after, (0, false, Offset::Top(0)));
        let page = pulls.page(30, 8, now(), true, &theme);
        assert_eq!(links_on(page), HashSet::from([0, 1]));
        assert!(rows_of(&page.lines)[0].starts_with("● #1"));
        pulls.toggle();
        assert_eq!((pulls.viewing, pulls.scroll), (true, Offset::Top(0)));
        pulls.toggle();
        assert!(!pulls.viewing);
    }

    #[test]
    fn a_scroll_after_end_or_past_the_end_counts_before_the_next_frame() {
        let mut pulls = PullRequests::default();
        let theme = theme();
        let mut pull = link("o/r", 1, "h", "main");
        pull["snapshot"]["title"] = json!("A long title ".repeat(60));
        pulls.set(list_of(&thread(vec![pull])));
        pulls.toggle();
        // A page shows the row back to the list and seven rows of the details.
        pulls.page(30, 8, now(), true, &theme);
        let last = detail_rows(&pulls).len() - 7;
        assert!(last > 3 * 7, "{last}");
        // Draws a frame and says how far it scrolled the details.
        let drawn = |pulls: &mut PullRequests| {
            pulls.page(30, 8, now(), true, &theme);
            pulls.scroll
        };

        // End, then PageUp, ↑ or three turns of the wheel up before a frame: each moves from
        // the last row, not from past it.
        pulls.end();
        pulls.page_by(false);
        assert_eq!(drawn(&mut pulls), Offset::Top(last - 7));
        pulls.end();
        pulls.move_by(-1);
        assert_eq!(drawn(&mut pulls), Offset::Top(last - 1));
        pulls.end();
        for _ in 0..3 {
            pulls.move_by(-1);
        }
        assert_eq!(drawn(&mut pulls), Offset::Top(last - 3));
        // ↓ after End stays at the end, and ↑ after that moves a row.
        pulls.end();
        pulls.move_by(1);
        pulls.move_by(-1);
        assert_eq!(drawn(&mut pulls), Offset::Top(last - 1));
        // Whichever of Home and End comes last wins.
        pulls.end();
        pulls.home();
        assert_eq!(drawn(&mut pulls), Offset::Top(0));
        pulls.home();
        pulls.end();
        assert_eq!(drawn(&mut pulls), Offset::Top(last));

        // ↓ and PageDown past the end stop at the last row the frame showed, so ↑ and PageUp
        // after them count.
        pulls.home();
        drawn(&mut pulls);
        for _ in 0..last + 5 {
            pulls.move_by(1);
        }
        assert_eq!(pulls.scroll, Offset::Top(last));
        pulls.move_by(-1);
        assert_eq!(drawn(&mut pulls), Offset::Top(last - 1));
        for _ in 0..5 {
            pulls.page_by(true);
        }
        pulls.page_by(false);
        assert_eq!(drawn(&mut pulls), Offset::Top(last - 7));

        // End and the keys after it count from wherever the last row lands at the size of the
        // next frame.
        pulls.end();
        pulls.move_by(-1);
        let page = pulls.page(20, 6, now(), true, &theme);
        assert!(page.above && page.below);
        let narrower = detail_rows(&pulls).len() - 5;
        assert_eq!(pulls.scroll, Offset::Top(narrower - 1));
        // Before a frame first draws a link's details, nothing knows how many rows they take,
        // and End and ↑ count from their end all the same.
        assert!(pulls.back());
        pulls.page(30, 8, now(), true, &theme);
        pulls.toggle();
        pulls.end();
        pulls.move_by(-1);
        assert_eq!(drawn(&mut pulls), Offset::Top(last - 1));
    }

    #[test]
    fn clicks_select_a_link_open_its_details_and_lead_back() {
        let mut pulls = PullRequests::default();
        let theme = theme();
        let links = vec![link("o/r", 1, "a", "m"), link("o/r", 2, "b", "m")];
        pulls.set(list_of(&thread(links)));
        // Before a frame lays the list out, a click lands on nothing.
        assert!(!pulls.click(0));
        pulls.page(40, 20, now(), true, &theme);
        // The second link's rows are the page's fourth to sixth.
        assert!(pulls.click(4));
        assert_eq!((pulls.selected, pulls.viewing), (1, false));
        pulls.page(40, 20, now(), true, &theme);
        // The blank rows and the footer take no click.
        for offset in [6, 7, 18, 19, 20, 200] {
            assert!(!pulls.click(offset), "{offset}");
        }
        // A second click on the link shows its details.
        assert!(pulls.click(3));
        assert!(pulls.viewing);
        let rows = rows_of(&pulls.page(40, 20, now(), true, &theme).lines);
        assert_eq!(rows[1], "#2 Change 2");
        // Only the row above them takes a click, and it leads back to the list with the link
        // still selected.
        for offset in 1..30 {
            assert!(!pulls.click(offset), "{offset}");
        }
        assert!(pulls.viewing);
        assert!(pulls.click(0));
        assert_eq!((pulls.selected, pulls.viewing), (1, false));
    }

    #[test]
    fn a_click_lands_only_on_the_page_the_last_frame_drew() {
        let mut pulls = PullRequests::default();
        let theme = theme();
        let links = (1..=20)
            .map(|number| link("o/r", number, &format!("h{number}"), "main"))
            .collect();
        pulls.set(list_of(&thread(links)));
        // Three links fit above the footer.
        pulls.page(40, 10, now(), true, &theme);
        // The keys move the selection past them. Until a frame lays the list out again, a
        // click on its first row lands on nothing, rather than on the link that showed there.
        pulls.move_by(5);
        pulls.page_by(true);
        assert_eq!(pulls.selected, 8);
        for offset in 0..10 {
            assert!(!pulls.click(offset), "{offset}");
        }
        assert_eq!((pulls.selected, pulls.viewing), (8, false));
        // After the frame, the click lands on the link that row shows now.
        let page = pulls.page(40, 10, now(), true, &theme);
        assert_eq!(page.rows[0], Some(Spot::Link(6)));
        assert!(pulls.click(0));
        assert_eq!(pulls.selected, 6);
        // So with the details: until a frame draws them, no click shows the list again, and
        // after their scroll moves, none does until the next.
        pulls.page(40, 10, now(), true, &theme);
        assert!(pulls.click(0));
        assert!(pulls.viewing);
        assert!(!pulls.click(0));
        pulls.page(40, 10, now(), true, &theme);
        pulls.move_by(3);
        assert!(!pulls.click(0));
        assert!(pulls.viewing);
        pulls.page(40, 10, now(), true, &theme);
        assert!(pulls.click(0));
        assert!(!pulls.viewing);
        // A move that changes nothing keeps the page the reader sees.
        pulls.page(40, 10, now(), true, &theme);
        pulls.move_by(0);
        assert!(pulls.click(3));
        assert_eq!(pulls.selected, 7);
    }

    #[test]
    fn wanted_rows_follow_the_links_and_the_details() {
        let mut pulls = PullRequests::default();
        let theme = theme();
        let links = vec![link("o/r", 1, "a", "m"), link("o/r", 2, "b", "m")];
        pulls.set(list_of(&thread(links)));
        assert_eq!(pulls.wanted(40, now(), &theme), 2 * ENTRY_ROWS + 1);
        // The row back to the list and the twenty rows of the details.
        pulls.toggle();
        assert_eq!(pulls.wanted(40, now(), &theme), 21);
        pulls.set(list_of(&thread(vec![])));
        assert!(!pulls.viewing);
        assert_eq!(pulls.wanted(40, now(), &theme), 3);
    }

    #[test]
    fn counts_group_their_digits() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }

    /// A pull request's detail as T3 sends it, with no reviewers, labels or checks and an
    /// empty description, from a host that offers no labels.
    fn bare_detail() -> Value {
        json!({
            "provider": "github",
            "capabilities": {"diff": true, "comment": true, "actions": [], "mergeMethods": [],
                "search": true, "review": {"inlineComment": false, "reply": false,
                "resolve": false, "verdicts": []},
                "reviewers": {"request": false, "listCandidates": false}},
            "viewerPermissions": {"actions": [], "comment": false, "resolve": false,
                "verdicts": [], "requestReviewers": false},
            "projectId": "p", "projectTitle": "Demo", "workspaceRoot": "/repo",
            "repository": "krishhgg/t3term", "number": 31, "title": "Change 31", "body": " ",
            "url": "https://github.com/krishhgg/t3term/pull/31", "author": null,
            "state": "open", "isDraft": false, "mergeability": "unknown", "additions": 0,
            "deletions": 0, "changedFiles": 0, "headBranch": "feature", "baseBranch": "master",
            "createdAt": "2026-10-13T08:00:00Z", "updatedAt": "2026-10-13T11:00:00Z",
            "mergedAt": null, "closedAt": null, "reviewers": [], "labels": [], "checks": [],
            "mergeCapabilities": {"merge": false, "squash": false, "rebase": false}
        })
    }

    /// An activity as T3 sends it for a pull request nobody has said anything on.
    fn bare_activity() -> Value {
        json!({"comments": [], "commentCount": 0, "commentsTruncated": false,
            "reviewThreads": [], "commits": []})
    }

    fn read_detail(detail: &Value) -> Box<pull_request::Detail> {
        Box::new(pull_request::Detail::read(detail).unwrap())
    }

    /// The details of `link` once its host answered `answer`, `width` columns wide, from the
    /// row after the lines it changes to the URL's label.
    fn host_rows(
        link: Value,
        answer: Result<Box<pull_request::Detail>, String>,
        width: usize,
    ) -> Vec<String> {
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![link])));
        pulls.toggle();
        let ask = pulls.take_ask(true, now()).expect("a read");
        assert!(pulls.answer(ask.ticket, Part::Detail(answer)));
        let activity = pull_request::Activity::read(&bare_activity()).unwrap();
        assert!(pulls.answer(ask.ticket, Part::Activity(Ok(Box::new(activity)))));
        pulls.page(width, 500, now(), true, &theme());
        let rows = detail_rows(&pulls);
        let from = rows
            .iter()
            .position(|row| row == "+821 -51")
            .expect("the lines")
            + 1;
        let to = rows.iter().position(|row| row == "URL").expect("the URL");
        rows[from..to].to_vec()
    }

    #[test]
    fn opening_a_links_details_asks_for_its_pull_request_once() {
        let mut pulls = PullRequests::default();
        let mut pull = link("Krishhgg/T3term", 31, "feature", "master");
        pull["host"] = json!(" GitHub.com ");
        pulls.set(list_of(&thread(vec![
            pull,
            link("krishhgg/t3term", 32, "b", "c"),
        ])));
        // Nothing asks until the details open, and a disabled read asks for nothing.
        assert_eq!(pulls.take_ask(true, now()), None);
        pulls.toggle();
        assert_eq!(pulls.take_ask(false, now()), None);
        assert_eq!(pulls.take_ask(true, now()), None);
        // The read names the link as T3 sent it, trimmed, not as it compares.
        pulls.back();
        pulls.toggle();
        let ask = pulls.take_ask(true, now()).expect("a read");
        let named = ("GitHub.com", "Krishhgg/T3term", 31, 1);
        assert_eq!(
            (
                ask.host.as_str(),
                ask.repository.as_str(),
                ask.number,
                ask.ticket
            ),
            named
        );
        pulls.page(44, 30, now(), true, &theme());
        assert!(detail_rows(&pulls).contains(&"Reading from GitHub.com…".to_string()));
        // While it is under way, opening the details again asks for nothing.
        pulls.back();
        pulls.toggle();
        assert_eq!(pulls.take_ask(true, now()), None);
        // An answer to another ticket is dropped. Another thread drops the read and its
        // answer.
        assert!(!pulls.answer(2, Part::Detail(Err("other".into()))));
        pulls.follow();
        assert!(!pulls.answer(1, Part::Detail(Err("late".into()))));
        assert!(pulls.read.is_none());
    }

    #[test]
    fn a_read_stands_until_both_answers_came_and_neither_failed() {
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![link(
            "krishhgg/t3term",
            31,
            "a",
            "b",
        )])));
        let reopen = |pulls: &mut PullRequests, at: i64| {
            pulls.back();
            pulls.toggle();
            pulls.take_ask(true, at).map(|ask| ask.ticket)
        };
        pulls.toggle();
        assert_eq!(pulls.take_ask(true, now()).map(|ask| ask.ticket), Some(1));
        // The detail alone leaves the conversation reading, and the read stands.
        assert!(pulls.answer(1, Part::Detail(Ok(read_detail(&bare_detail())))));
        pulls.page(44, 60, now(), true, &theme());
        let rows = detail_rows(&pulls);
        assert!(
            rows.contains(&"Reading the conversation…".to_string()),
            "{rows:#?}"
        );
        assert_eq!(reopen(&mut pulls, now()), None);
        // A conversation that failed says so, and the next open asks again.
        assert!(pulls.answer(1, Part::Activity(Err("timed out".into()))));
        assert!(!pulls.answer(1, Part::Done));
        pulls.page(44, 60, now(), true, &theme());
        let rows = detail_rows(&pulls);
        let failed = [
            "Comments",
            "Could not load pull request activity",
            "timed out",
        ];
        let at = rows
            .iter()
            .position(|row| row == "Comments")
            .expect("the section");
        assert_eq!(rows[at..at + 3], failed);
        assert_eq!(reopen(&mut pulls, now()), Some(2));
        // Both answers stand for a minute.
        assert!(pulls.answer(2, Part::Detail(Ok(read_detail(&bare_detail())))));
        let activity = pull_request::Activity::read(&bare_activity()).unwrap();
        assert!(pulls.answer(2, Part::Activity(Ok(Box::new(activity)))));
        assert!(!pulls.answer(2, Part::Done));
        assert_eq!(reopen(&mut pulls, now() + FRESH - 1), None);
        assert_eq!(reopen(&mut pulls, now() + FRESH), Some(3));
    }

    #[test]
    fn a_link_opened_while_a_read_is_under_way_waits_for_it() {
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![
            link("krishhgg/t3term", 31, "feature", "master"),
            link("krishhgg/t3term", 32, "b", "c"),
        ])));
        let numbers = |ask: Option<Ask>| ask.map(|ask| (ask.number, ask.ticket));
        pulls.toggle();
        assert_eq!(numbers(pulls.take_ask(true, now())), Some((31, 1)));
        // #32 waits for #31's read, and shows that it reads meanwhile.
        pulls.back();
        pulls.move_by(1);
        pulls.toggle();
        assert_eq!(pulls.take_ask(true, now()), None);
        pulls.page(44, 30, now(), true, &theme());
        assert!(detail_rows(&pulls).contains(&"Reading from github.com…".to_string()));
        // Leaving its details drops it, and opening them again wants it again.
        pulls.back();
        assert!(pulls.read.is_none());
        pulls.toggle();
        assert_eq!(pulls.take_ask(true, now()), None);
        // #31's answers are dropped, and #32 is asked for once #31's read is done.
        assert!(!pulls.answer(1, Part::Detail(Err("late".into()))));
        assert_eq!(pulls.take_ask(true, now()), None);
        assert!(!pulls.answer(1, Part::Done));
        assert_eq!(numbers(pulls.take_ask(true, now())), Some((32, 2)));
        assert_eq!(pulls.take_ask(true, now()), None);

        // Another thread's link waits for the read the last thread left under way.
        pulls.follow();
        let other = link("krishhgg/t3term", 40, "d", "e");
        pulls.set(list_of(&thread(vec![other])));
        pulls.toggle();
        assert_eq!(pulls.take_ask(true, now()), None);
        // A read that can't be asked for any more is dropped.
        assert_eq!(pulls.take_ask(false, now()), None);
        assert!(pulls.read.is_none());
        pulls.back();
        pulls.toggle();
        assert_eq!(pulls.take_ask(true, now()), None);
        assert!(!pulls.answer(2, Part::Done));
        assert_eq!(numbers(pulls.take_ask(true, now())), Some((40, 3)));
    }

    #[test]
    fn a_legacy_link_is_read_on_the_host_its_url_names() {
        let legacy = json!({"repository": "krishhgg/t3term", "number": 7,
            "url": "https://GitHub.com/krishhgg/t3term/pull/7"});
        let thread = json!({"id": "t", "projectId": "p", "linkedPullRequest": legacy});
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread));
        pulls.toggle();
        let ask = pulls.take_ask(true, now()).expect("a read");
        assert_eq!((ask.host.as_str(), ask.number), ("github.com", 7));
    }

    #[test]
    fn a_hosts_answer_shows_as_the_desktops_summary_tab_does() {
        let pull = link("krishhgg/t3term", 31, "feature", "master");
        // An empty answer says so in each section, and leaves out the labels a host that
        // offers none can't have.
        let rows = host_rows(pull.clone(), Ok(read_detail(&bare_detail())), 44);
        let expected = [
            "",
            "Read from github.com just now",
            "",
            "Reviewers",
            "None",
            "",
            "Checks",
            "No checks reported.",
            "",
            "Description",
            "No description provided.",
            "",
            "Comments (0)",
            "No comments yet.",
            "",
        ];
        assert_eq!(rows, expected);

        // A full one: a bot among the reviewers, the labels, each check status with its
        // mark, a run waiting for approval, and the description as Markdown.
        let mut detail = bare_detail();
        detail["capabilities"]["labels"] = json!(true);
        detail["mergeability"] = json!("mergeable");
        detail["baseComparison"] = json!("behind");
        detail["reviewers"] = json!([
            {"login": "octocat", "name": "Mona", "isBot": false},
            {"login": "greptile-apps[bot]", "name": null, "isBot": true}
        ]);
        let runs = "https://github.com/o/r/actions/runs/12/job/3";
        let check = |name: &str, status: &str, url: Option<&str>| json!({"name": name, "status": status, "description": null, "url": url});
        detail["checks"] = json!([
            check("build", "success", None),
            check("lint", "failure", None),
            check("deploy", "cancelled", None),
            check("docs", "skipped", None),
            check("notes", "neutral", None),
            check("tests", "pending", None),
            check("first-time", "action-required", Some(runs)),
            check("sign", "action-required", None)
        ]);
        detail["body"] = json!("## Why\n\nIt **reads** the host.\u{1b}[2J");
        let rows = host_rows(pull.clone(), Ok(read_detail(&detail)), 44);
        let status = |name: &str, mark: &str, label: &str| {
            let gap = " ".repeat(44 - 2 - name.len() - label.chars().count());
            format!("{mark} {name}{gap}{label}")
        };
        let expected = [
            "".to_string(),
            "Read from github.com just now".into(),
            "This branch is out-of-date with master.".into(),
            "".into(),
            "Reviewers".into(),
            "octocat".into(),
            "greptile-apps[bot] (bot)".into(),
            "".into(),
            "Labels".into(),
            "None".into(),
            "".into(),
            "Checks".into(),
            status("build", "✓", "Passed"),
            status("lint", "✗", "Failed"),
            status("deploy", "✗", "Cancelled"),
            status("docs", "–", "Skipped"),
            status("notes", "–", "Neutral"),
            status("tests", "○", "Running"),
            status("first-time", "◉", "Awaiting approval"),
            status("sign", "◉", "Awaiting action"),
            "".into(),
            "Description".into(),
            "Why".into(),
            "".into(),
            "It reads the host.[2J".into(),
            "".into(),
            "Comments (0)".into(),
            "No comments yet.".into(),
            "".into(),
        ];
        assert_eq!(rows, expected);

        // A count of commits behind shows, and an answer that left entries or text out says
        // so. A closed pull request, or one that conflicts or may, isn't warned of.
        detail["behindBy"] = json!(1_234);
        let mut cut = detail.clone();
        cut["body"] = json!("x".repeat(pull_request::BODY + 1));
        cut["checks"] = json!(vec![check("a", "success", None); 1001]);
        let rows = host_rows(pull.clone(), Ok(read_detail(&cut)), 44);
        let behind = [
            "This branch is out-of-date with master by",
            "1,234 commits.",
        ];
        assert_eq!(rows[2..4], behind);
        let note = "T3 sent more reviewers, labels or checks than t3term keeps.";
        let joined = rows.join(" ");
        assert!(joined.contains(note), "{joined}");
        assert!(
            joined.contains("Cut short: T3 sent a description longer than"),
            "{joined}"
        );
        detail["behindBy"] = json!(1);
        let rows = host_rows(pull.clone(), Ok(read_detail(&detail)), 60);
        assert_eq!(
            rows[2],
            "This branch is out-of-date with master by 1 commit."
        );
        for (field, value) in [
            ("state", "closed"),
            ("mergeability", "conflicting"),
            ("mergeability", "unknown"),
        ] {
            let mut quiet = detail.clone();
            quiet[field] = json!(value);
            let rows = host_rows(pull.clone(), Ok(read_detail(&quiet)), 60);
            assert_eq!(rows[2], "", "{field} {value}");
        }
    }

    #[test]
    fn a_failed_read_says_why_cleaned_and_bounded() {
        let pull = link("krishhgg/t3term", 31, "feature", "master");
        let failure = format!(
            "No GitHub credential\u{1b}]52;c;x\u{7} {}",
            "y".repeat(5_000)
        );
        let rows = host_rows(pull, Err(failure), 44);
        let text = rows.join(" ");
        assert!(
            text.trim().starts_with("No GitHub credential]52;c;x y"),
            "{text}"
        );
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{7}'),
            "{text}"
        );
        assert!(text.trim_end().ends_with('…'), "{text}");
        let kept = text.chars().filter(|c| !c.is_whitespace()).count();
        assert!(kept <= FAILURE + 1, "{kept}");
    }

    #[test]
    fn a_check_too_long_for_its_row_wraps_under_its_mark() {
        let theme = theme();
        let check = CheckRow {
            name: "integration tests on macOS".into(),
            status: CheckStatus::Success,
            approval: false,
        };
        let rows = rows_of(&check_lines(&check, 22, &theme));
        assert_eq!(rows, ["✓ integration   Passed", "  tests on", "  macOS"]);
        // Too narrow for the status beside the name, each takes rows of its own.
        let rows = rows_of(&check_lines(&check, 10, &theme));
        assert_eq!(rows, ["✓", "integratio", "n tests on", "macOS", "Passed"]);
    }

    #[test]
    fn only_a_github_actions_run_awaits_approval() {
        for (url, run) in [
            ("https://github.com/o/r/actions/runs/12", true),
            ("https://github.com/o/r/actions/runs/12/job/3", true),
            (
                "https://github.com/o/r/actions/runs/x/actions/runs/9/",
                true,
            ),
            ("https://github.com/o/r/actions/runs/", false),
            ("https://github.com/o/r/actions/runs/12x", false),
            ("https://github.com/o/r/checks/12", false),
        ] {
            assert_eq!(is_workflow_run(url), run, "{url}");
        }
    }

    /// One link whose host answered with `bare_detail` and `activity`, its details open.
    fn conversing(activity: Value) -> PullRequests {
        let mut pulls = PullRequests::default();
        let pull = link("krishhgg/t3term", 31, "a", "b");
        pulls.set(list_of(&thread(vec![pull])));
        pulls.toggle();
        let ask = pulls.take_ask(true, now()).expect("a read");
        let detail = Part::Detail(Ok(read_detail(&bare_detail())));
        assert!(pulls.answer(ask.ticket, detail));
        let activity = pull_request::Activity::read(&activity).unwrap();
        assert!(pulls.answer(ask.ticket, Part::Activity(Ok(Box::new(activity)))));
        pulls
    }

    /// A conversation of `active` remarks by people, the newest last, and one by a bot.
    fn remarks(active: u32) -> Value {
        let remark = |id: String, login: &str, hour: u32, body: String| {
            json!({"id": id, "kind": "issue-comment", "author": {"login": login, "name": null},
                "body": body, "createdAt": format!("2026-10-13T{hour:02}:00:00Z"),
                "url": null, "path": null, "reviewState": null})
        };
        let mut comments: Vec<Value> = (0..active)
            .map(|n| remark(format!("c{n}"), "mona", n, format!("Remark {n}")))
            .collect();
        comments.push(remark("b".into(), "ci[bot]", 11, "Bot".into()));
        json!({"comments": comments, "commentCount": active + 1, "commentsTruncated": false,
            "reviewThreads": [], "commits": []})
    }

    /// The rows of the page at `height` that `control` takes, and the rows that show
    /// highlighted.
    fn control_rows(
        pulls: &mut PullRequests,
        height: usize,
        focused: bool,
        control: Control,
    ) -> (Vec<usize>, Vec<usize>) {
        let theme = theme();
        let page = pulls.page(40, height, now(), focused, &theme);
        let rows = (0..page.rows.len())
            .filter(|&at| page.rows[at] == Some(Spot::Control(control)))
            .collect();
        let lit = (0..page.lines.len())
            .filter(|&at| {
                let spans = &page.lines[at].spans;
                spans
                    .iter()
                    .any(|span| span.style.bg == Some(theme.highlight))
            })
            .collect();
        (rows, lit)
    }

    #[test]
    fn arrows_choose_the_controls_in_view_and_enter_does_what_they_do() {
        let mut pulls = conversing(remarks(12));
        // At the top no control shows, so ↓ scrolls a row.
        let theme = theme();
        let page = pulls.page(40, 12, now(), true, &theme);
        assert!(
            !page
                .rows
                .iter()
                .any(|s| matches!(s, Some(Spot::Control(_))))
        );
        pulls.step(true);
        assert_eq!((pulls.pick, pulls.scroll), (None, Offset::Top(1)));

        // From the end of a taller page, ↑ chooses the last control in view: the bots' group,
        // both its rows, highlighted while the details have the keyboard.
        pulls.end();
        pulls.page(40, 24, now(), true, &theme);
        pulls.step(false);
        assert_eq!(pulls.pick, Some(Control::Bots));
        let (bots, lit) = control_rows(&mut pulls, 24, true, Control::Bots);
        assert_eq!(bots.len(), 2);
        assert_eq!(lit, bots);
        let (_, lit) = control_rows(&mut pulls, 24, false, Control::Bots);
        assert!(lit.is_empty());
        assert_eq!(pulls.action().as_deref(), Some("show bot comments"));

        // Enter opens the group, which stays on its rows with its remark under it.
        pulls.enter();
        assert!(pulls.viewing);
        assert_eq!(pulls.action().as_deref(), Some("hide bot comments"));
        let (open, lit) = control_rows(&mut pulls, 24, true, Control::Bots);
        assert_eq!((open, lit), (bots.clone(), bots.clone()));
        let rows = rows_of(&pulls.page(40, 24, now(), true, &theme).lines);
        assert_eq!(rows[bots[0]], "⌄ 1 bot comment");

        // ↑ chooses Show older above it. Once every older remark shows, Show only 10 recent
        // takes its place and its row.
        pulls.step(false);
        assert_eq!(pulls.pick, Some(Control::Older));
        assert_eq!(pulls.action().as_deref(), Some("show 2 older"));
        let (older, _) = control_rows(&mut pulls, 24, true, Control::Older);
        let rows = rows_of(&pulls.page(40, 24, now(), true, &theme).lines);
        assert_eq!(rows[older[0]], "+ Show 2 older comments (2 hidden)");
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::Recent));
        assert_eq!(pulls.action().as_deref(), Some("show only 10 recent"));
        let (recent, lit) = control_rows(&mut pulls, 24, true, Control::Recent);
        assert_eq!((recent.clone(), lit), (older.clone(), older));
        let rows = rows_of(&pulls.page(40, 24, now(), true, &theme).lines);
        assert_eq!(rows[recent[0]], "Show only 10 recent comments");
        let all = detail_rows(&pulls);
        let remarks = all.iter().filter(|row| row.starts_with("  Remark")).count();
        assert_eq!(remarks, 12);

        // Home leaves none chosen, and so does showing the list.
        pulls.home();
        assert_eq!((pulls.pick, pulls.action()), (None, None));
        pulls.end();
        pulls.page(40, 24, now(), true, &theme);
        pulls.step(false);
        assert!(pulls.pick.is_some());
        pulls.back();
        assert_eq!(pulls.pick, None);
        // Opening the details again shows them as they first did.
        pulls.toggle();
        assert_eq!(pulls.shown, Shown::default());
    }

    #[test]
    fn a_chosen_control_stays_in_view_and_each_shows_on_the_way_down_and_up() {
        let mut pulls = conversing(remarks(12));
        let theme = theme();
        let mut picks = Vec::new();
        for down in [true, false] {
            for _ in 0..80 {
                pulls.page(40, 12, now(), true, &theme);
                pulls.step(down);
                if let Some(pick) = pulls.pick {
                    if picks.last() != Some(&pick) {
                        picks.push(pick);
                    }
                    // The chosen control has a row in view whenever it is chosen.
                    let (view, targets) = pulls.in_view().expect("drawn");
                    let (rows, _) = targets.iter().find(|(_, c)| *c == pick).unwrap();
                    assert!(shows(rows, &view), "{pick:?} {rows:?} {view:?}");
                }
            }
        }
        let both = [
            Control::Order,
            Control::Older,
            Control::Bots,
            Control::Older,
            Control::Order,
        ];
        assert_eq!(picks, both);
        assert_eq!(pulls.scroll, Offset::Top(0));
    }

    #[test]
    fn a_click_on_a_control_does_what_it_does_once_a_frame_drew_it() {
        let mut pulls = conversing(remarks(12));
        let theme = theme();
        pulls.end();
        let (bots, _) = control_rows(&mut pulls, 24, true, Control::Bots);
        // Either of the group's rows opens it, and it is chosen.
        assert!(pulls.click(bots[1]));
        assert_eq!(pulls.pick, Some(Control::Bots));
        assert_eq!(pulls.action().as_deref(), Some("hide bot comments"));
        // The rows moved, so until a frame draws them a click lands on nothing.
        assert!(!pulls.click(bots[0]));
        let (open, _) = control_rows(&mut pulls, 24, true, Control::Bots);
        assert_eq!(open, bots);
        assert!(pulls.click(bots[0]));
        assert_eq!(pulls.action().as_deref(), Some("show bot comments"));
        // A row between two controls takes no click.
        pulls.page(40, 24, now(), true, &theme);
        assert!(!pulls.click(bots[0] - 1));
        assert_eq!(pulls.action().as_deref(), Some("show bot comments"));
    }
}
