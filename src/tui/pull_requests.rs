//! The pull requests linked to the open thread, in the details' second tab. Each shows its
//! number, title and state, and the checks, review and line counts the host reported, with
//! the repository and its branches under them. Enter or a second click shows a link's details
//! in the list's place, its whole URL among them as plain text, and they scroll. ←, Enter or
//! a click on the row above them goes back to the list. Once the host's detail of the pull
//! request came, that row names its tabs, the Summary, the Timeline and, where the host gives a
//! diff, the Code, which →, ← and clicks switch between. t3term opens no browser and changes
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
use crate::pull_request::{self, BaseComparison, CheckStatus, Mergeability, ViewedFilesStore};
use crate::transcript::{clusters, without_controls};

mod code;
mod conversation;

use code::{Code, Reads, SIDE, Sideways};
use conversation::{Control, Conversation, Lifecycle, Reviewer, Shown, Targets};

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
/// The columns a turn of the wheel sideways scrolls the Code tab's lines that don't wrap, a
/// tab stop's.
const WHEEL_COLUMNS: usize = 4;
/// What a GitHub stack does, as the desktop's tooltip says it.
const NATIVE_STACK: &str = "merging a layer lands the ones below it.";
/// The note over a stack whose last read failed, as the desktop's `Stack data may be stale`.
const STACK_STALE: &str = "May be out of date: t3term couldn't read the stack again.";
/// The note over the stack saved on a link while the first read of the host's is on its
/// way, as the desktop's `Refreshing stack… Showing saved data.`
const STACK_SAVED: &str = "Saved with the link. Reading the host's stack…";
/// What watching a pull request does, as the desktop's tooltip says it.
const WATCHING: &str =
    "Watching: the agent wakes when checks finish, someone comments, or the branch conflicts";
/// How long a read of a link's pull request stands, in milliseconds, as the desktop keeps
/// `pullRequests.detail` for 60 seconds (packages/client-runtime/src/state/pullRequests.ts).
/// Opening the link's details again within it shows that read rather than asking again.
const FRESH: i64 = 60_000;
/// How long after a read of a link was asked its details read the detail again while they
/// show and you use t3term, in milliseconds, as the desktop's `LIVE_REFRESH_INTERVAL_MS`
/// (apps/web/src/hooks/useLiveRefresh.ts:22).
const LIVE: i64 = 5 * 60_000;
/// How long after the last key, click or wheel t3term stops reading the details again, in
/// milliseconds, as the desktop's `LIVE_REFRESH_IDLE_AFTER_MS` (useLiveRefresh.ts:31). The
/// next one reads them again at once when they are due.
pub(super) const IDLE: i64 = 6 * 60_000;
/// How soon after a read of a link was asked coming back to t3term reads it again, in
/// milliseconds, as the desktop's `LIVE_REFRESH_MIN_INTERVAL_MS` (useLiveRefresh.ts:19).
const SOONEST: i64 = 10_000;
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
    /// The host-native stack saved on the link, where it holds the link, which its details
    /// show until they read the host's.
    saved: Option<Box<StackView>>,
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
    /// That stack whole, where it holds every field the contract requires.
    saved: Option<pull_request::Stack>,
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
    let saved: Vec<_> = {
        let snapshots: HashMap<&Key, &Snapshot> = raws
            .iter()
            .filter_map(|raw| Some((&raw.entry.key, raw.entry.snapshot()?)))
            .collect();
        raws.iter().map(|raw| saved_of(raw, &snapshots)).collect()
    };
    for (raw, saved) in raws.iter_mut().zip(saved) {
        raw.entry.saved = saved;
    }
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

/// The stack saved on `raw`'s link, as the details show it before they read the host's, as
/// `savedPullRequestStack` makes it (apps/web/src/components/pullRequest/
/// pullRequestStackSnapshot.ts:4-48). Only a stack that holds the link counts. Each layer the
/// thread links has the title and the draft flag its link's snapshot holds.
fn saved_of(raw: &Raw, snapshots: &HashMap<&Key, &Snapshot>) -> Option<Box<StackView>> {
    let stack = raw.saved.as_ref()?;
    let number = raw.entry.number;
    if !stack.layers.iter().any(|layer| layer.number == number) {
        return None;
    }
    let mut key = raw.entry.key.clone();
    let view = StackView::saved(stack, |number| {
        key.number = number;
        snapshots.get(&key).copied()
    });
    Some(Box::new(view))
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
        saved: None,
    };
    let saved = link
        .get("stack")
        .and_then(|stack| pull_request::Stack::read(stack).ok());
    let raw = Raw {
        entry,
        stack: stack_of(link.get("stack")),
        saved,
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
        saved: None,
        key,
    };
    Some(Raw {
        entry,
        stack: None,
        saved: None,
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
    /// How many columns the Code tab's lines that don't wrap are scrolled sideways.
    across: usize,
    /// The minute the relative times read from.
    minute: i64,
    /// The control ↑ and ↓ chose in the details, which shows highlighted while they have the
    /// keyboard.
    pick: Option<Control>,
    focused: bool,
    /// The tab of the details that shows, while they have tabs.
    tabs: Option<Tab>,
    /// Whether the details offer the Code tab.
    code: bool,
}

/// A tab of a link's details, as the desktop's pull request panel has them once the host's
/// detail of the pull request came. The Code tab shows only where the detail's
/// `capabilities.diff` says the host gives a diff, as the desktop hides it otherwise
/// (PullRequestDetailPanel.tsx:1444-1455).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Tab {
    #[default]
    Summary,
    Timeline,
    Code,
}

impl Tab {
    /// The tabs the details offer, in order, with the Code tab where `code` says.
    fn offered(code: bool) -> &'static [Tab] {
        match code {
            true => &[Tab::Summary, Tab::Timeline, Tab::Code],
            false => &[Tab::Summary, Tab::Timeline],
        }
    }

    fn name(self) -> &'static str {
        match self {
            Tab::Summary => "Summary",
            Tab::Timeline => "Timeline",
            Tab::Code => "Code",
        }
    }

    /// Its place among the tabs, which keeps its scroll while another shows.
    fn place(self) -> usize {
        match self {
            Tab::Summary => 0,
            Tab::Timeline => 1,
            Tab::Code => 2,
        }
    }
}

/// What a click on a row of a page reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spot {
    /// One of a link's rows in the list.
    Link(usize),
    /// The row above a link's details, which leads back to the list.
    Back,
    /// A row of a control in the details' Comments section or Timeline.
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
    /// The columns of each tab's name on the row above a link's details, with the gap before
    /// it. A click on the rest of that row leads back to the list.
    tabs: Vec<(Range<usize>, Tab)>,
}

/// The selected link's details at one width and minute.
struct Detail {
    index: usize,
    /// The width and the minute these lines are of.
    at: (usize, i64),
    lines: Vec<Line<'static>>,
    /// The rows of each control among the lines, in the order they show.
    targets: Targets,
    /// The rows of the Code tab's lines that don't wrap, which scroll sideways.
    sideways: Sideways,
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
/// number as T3 sent them, and the ticket its answer comes back with. `invalidate` says a
/// person asked to read it from the host, so the read sends `pullRequests.invalidate` first.
/// `updated` is the `updatedAt` of the detail the details show, for a read that only keeps
/// them current: it reads the conversation only when the new detail's differs. `stack` says
/// to read the stack once the detail says the host keeps stacks: every read but one that only
/// keeps the details current does, and so does that one when the stack shown is stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Ask {
    pub ticket: u64,
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub invalidate: bool,
    pub updated: Option<String>,
    pub stack: bool,
}

/// What reading a slice of a link's pull request's diff names: the link's host, repository and
/// number as T3 sent them, the cursor of the slice, or None for the first, the commit whose own
/// diff it reads, or None for the whole change's, and the ticket its answer comes back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiffAsk {
    pub ticket: u64,
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub cursor: Option<String>,
    pub commit: Option<String>,
}

/// What reading the next page of a review thread's remarks names: the link's host, repository
/// and number as T3 sent them, the thread's id, the cursor of the page, and the ticket its
/// answer comes back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommentsAsk {
    pub ticket: u64,
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub thread: String,
    pub cursor: String,
}

/// What reading a file's whole text after the change names, for the unchanged lines around its
/// hunks: the link's host, repository and number as T3 sent them, the commit whose own diff the
/// Code tab shows, or None for the whole change's, how the file changed, its paths before and
/// after the change, and the ticket its answer comes back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TextAsk {
    pub ticket: u64,
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub commit: Option<String>,
    pub change: pull_request::ChangeType,
    pub old_path: String,
    pub new_path: String,
}

/// What reading the reader's marks on files viewed names: the link's host, repository and
/// number as T3 sent them, and the ticket its answer comes back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ViewedAsk {
    pub ticket: u64,
    pub host: String,
    pub repository: String,
    pub number: u64,
}

/// Part of what a host said of a pull request. A read sends its detail, its conversation and,
/// where the host keeps stacks, its stack, as each arrives, then `Done`.
pub(super) enum Part {
    /// The answer to `pullRequests.detail`, or why it failed.
    Detail(Result<Box<pull_request::Detail>, String>),
    /// The answer to `pullRequests.activity`, or why it failed.
    Activity(Result<Box<pull_request::Activity>, String>),
    /// The answer to `pullRequests.stack`, or why it failed.
    Stack(Result<Option<Box<pull_request::Stack>>, String>),
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
    /// The time the detail shown came, which `FRESH` and the age the details show count from,
    /// as the desktop's `Atom.swr` counts from its result's timestamp. Until one came, the time
    /// it was asked for, or wanted while it waits.
    at: i64,
    answer: Answer<Summary>,
    conversation: Answer<Conversation>,
    /// Set when the answers may be out of date: T3 said its reads may have changed since they
    /// were asked for, or the details opened again after `FRESH`. They are read again while
    /// the details show them, and show until the new answers come.
    again: bool,
    /// The read under way to replace the answers.
    next: Option<Next>,
    /// Set when the last read to replace the answers missed either one, so opening the details
    /// again reads again, as a failure is never reused.
    missed: bool,
    /// Set when a person asked to read the pull request from its host, so the next read sends
    /// `pullRequests.invalidate` first.
    invalidate: bool,
    /// Set while a read that sent `pullRequests.invalidate` is under way, until T3's refresh
    /// number moves. The invalidate moves it, so that number doesn't read again.
    echo: bool,
    /// When its last read was asked for, which `LIVE` counts from.
    asked: i64,
    /// Set when the read again that `again` wants only keeps the details current, so it reads
    /// the detail, and the conversation only once the detail changed. It reads both when
    /// either answer failed or the last read missed one.
    live: bool,
    /// The stack the host keeps the pull request in, as the last read of it found it. None
    /// where it is in none, or nothing read it.
    stack: Option<Box<StackView>>,
    /// Set when the last read of the stack failed. The stack shown stays and may be out of
    /// date, as the desktop keeps the stack it last read (pullRequestStackSnapshot.ts:51-73),
    /// and opening the details again reads again.
    stack_failed: bool,
    /// When the last read of the stack came, finding one or none. None until one came.
    stack_at: Option<i64>,
    /// Set when the read again that `again` wants reads the stack too, though it only keeps
    /// the details current.
    stack_due: bool,
    /// What the Code tab read of the diff, once it showed.
    code: Option<Code>,
}

/// A read under way to replace a read's answers. It holds each answer until both came, so the
/// details change once.
struct Next {
    ticket: u64,
    /// The time its detail came, or the time it was asked for until then.
    at: i64,
    answer: Option<Result<Box<Summary>, String>>,
    conversation: Option<Result<Box<Conversation>, String>>,
    stack: Option<StackRead>,
}

/// What a read of the stack found.
enum StackRead {
    /// The stack, or None where the pull request is in none, and when it came.
    Read(Option<Box<StackView>>, i64),
    Failed,
}

impl StackRead {
    fn of(answer: Result<Option<Box<pull_request::Stack>>, String>, now: i64) -> StackRead {
        match answer {
            Ok(stack) => StackRead::Read(stack.map(|stack| Box::new(StackView::of(&stack))), now),
            Err(_) => StackRead::Failed,
        }
    }
}

enum Answer<T> {
    Reading,
    Read(Box<T>),
    /// Why it failed, cleaned and bounded.
    Failed(String),
}

impl Read {
    /// A read of `entry`'s pull request, wanted at `now`.
    fn of(entry: &Entry, now: i64) -> Read {
        Read {
            key: entry.key.clone(),
            host: entry.ask_host.clone(),
            repository: entry.ask_repository.clone(),
            number: entry.number,
            ticket: None,
            at: now,
            answer: Answer::Reading,
            conversation: Answer::Reading,
            again: false,
            next: None,
            missed: false,
            invalidate: false,
            echo: false,
            asked: now,
            live: false,
            stack: None,
            stack_failed: false,
            stack_at: None,
            stack_due: false,
            code: None,
        }
    }

    /// Whether opening the link's details again should leave it be: it is under way, or both
    /// its answers came within `FRESH` of `now` and T3 hasn't said its reads changed since. A
    /// failure of either is never reused, nor are answers a read to replace them missed.
    fn stands(&self, now: i64) -> bool {
        if self.next.is_some() {
            return true;
        }
        if self.again || self.missed || self.stack_failed {
            return false;
        }
        match (&self.answer, &self.conversation) {
            (Answer::Failed(_), _) | (_, Answer::Failed(_)) => false,
            (Answer::Reading, _) | (_, Answer::Reading) => true,
            (Answer::Read(_), Answer::Read(_)) => now.saturating_sub(self.at) < FRESH,
        }
    }

    /// Whether either answer came, so the details have something to show while they read
    /// again.
    fn answered(&self) -> bool {
        matches!(self.answer, Answer::Read(_)) || matches!(self.conversation, Answer::Read(_))
    }

    /// Takes the answers of the read that replaces these. Each answer that came replaces its
    /// own, and a failure replaces only a failure, as the desktop keeps showing the last
    /// detail and conversation it read (PullRequestDetailPanel.tsx:2728).
    fn settle(&mut self, next: Next) {
        // A read that keeps the details current reads no conversation while the detail stays
        // the same, which misses nothing.
        self.missed =
            !matches!(next.answer, Some(Ok(_))) || matches!(next.conversation, Some(Err(_)));
        match next.answer {
            Some(Ok(summary)) => {
                // A pull request that changed has a diff that may have, as the desktop's
                // refresh token moves with `updatedAt` (PullRequestDetailPanel.tsx:836-847).
                let changed = match &self.answer {
                    Answer::Read(shown) => shown.updated != summary.updated,
                    _ => true,
                };
                if let Some(code) = self.code.as_mut().filter(|_| changed) {
                    code.again();
                }
                self.answer = Answer::Read(summary);
                self.at = next.at;
            }
            Some(Err(failure)) if !matches!(self.answer, Answer::Read(_)) => {
                self.answer = Answer::Failed(failure);
            }
            _ => {}
        }
        match next.conversation {
            Some(Ok(mut conversation)) => {
                if let Answer::Read(old) = &mut self.conversation {
                    conversation.carry(old);
                }
                self.conversation = Answer::Read(conversation);
            }
            Some(Err(failure)) if !matches!(self.conversation, Answer::Read(_)) => {
                self.conversation = Answer::Failed(failure);
            }
            _ => {}
        }
        // A read that keeps the details current doesn't read the stack, which keeps the one
        // shown, as the desktop's live refresh reads only the detail.
        if let Some(stack) = next.stack {
            self.take_stack(stack);
        }
    }

    /// Takes what a read of the stack found. A stack that came replaces the one shown, and so
    /// does finding the pull request in none. A failure keeps the one shown.
    fn take_stack(&mut self, read: StackRead) {
        match read {
            StackRead::Read(stack, at) => {
                self.stack = stack;
                self.stack_failed = false;
                self.stack_at = Some(at);
            }
            StackRead::Failed => self.stack_failed = true,
        }
    }

    /// Drops the description, the remarks and the Code tab's files laid out in the old colors.
    fn restyle(&mut self) {
        if let Answer::Read(summary) = &mut self.answer {
            summary.laid_out = None;
        }
        if let Answer::Read(conversation) = &mut self.conversation {
            conversation.restyle();
        }
        if let Some(code) = self.code.as_mut() {
            code.restyle();
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
    /// When the pull request opened and ended, for the Timeline.
    lifecycle: Lifecycle,
    /// The detail's `updatedAt`, which a read that keeps the details current compares.
    updated: String,
    /// Whether the host keeps stacks, as the detail's `capabilities.stacks` says.
    stacks: bool,
    /// Whether the host gives a diff, as the detail's `capabilities.diff` says.
    diff: bool,
    /// Who keeps the reader's marks on files viewed, as the detail's
    /// `capabilities.viewedFiles` says, or None where nobody does.
    viewed_files: Option<ViewedFilesStore>,
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
            lifecycle: Lifecycle::of(detail),
            updated: detail.updated_at.clone(),
            stacks: detail.capabilities.stacks,
            diff: detail.capabilities.diff,
            viewed_files: detail.capabilities.viewed_files,
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

/// What the details show of the stack the host keeps a pull request in, as the desktop's stack
/// menu lists it (apps/web/src/components/pullRequest/PullRequestStackMenu.tsx). Each text is
/// cleaned and bounded once, when the answer arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StackView {
    /// The stack's own number on the host.
    number: u64,
    /// The branch the bottom layer merges into.
    base: String,
    /// Bottom to top, as T3 sends them.
    layers: Vec<LayerRow>,
    /// Whether T3 sent more layers than t3term keeps, which leaves the top ones out.
    cut: bool,
}

/// One pull request in a stack.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LayerRow {
    number: u64,
    /// Its title, or its head branch where it has none, as the desktop's
    /// `layer.title || layer.headBranch` (PullRequestStackLayerContent.tsx).
    title: String,
    branch: String,
    state: State,
}

impl StackView {
    fn of(stack: &pull_request::Stack) -> StackView {
        StackView::with(stack, |layer| {
            let title = layer.title.as_deref().map(|title| bounded(title, TITLE));
            (title, layer.is_draft == Some(true))
        })
    }

    /// The stack saved on a link, which holds no titles. A layer whose `snapshot` T3 sent has
    /// that snapshot's title and draft flag.
    fn saved<'a>(
        stack: &pull_request::Stack,
        mut snapshot: impl FnMut(u64) -> Option<&'a Snapshot>,
    ) -> StackView {
        StackView::with(stack, |layer| match snapshot(layer.number) {
            Some(snapshot) => (Some(snapshot.title.clone()), snapshot.state == State::Draft),
            None => (None, false),
        })
    }

    /// `stack`, with each layer's title, cleaned and bounded, and whether it is a draft, as
    /// `about` says.
    fn with(
        stack: &pull_request::Stack,
        mut about: impl FnMut(&pull_request::Layer) -> (Option<String>, bool),
    ) -> StackView {
        let layers = stack.layers.iter().map(|layer| {
            let branch = bounded(&layer.head_branch, NAME);
            let (title, draft) = about(layer);
            // An open draft is a draft, as the desktop's `resolvePullRequestState` reads it.
            let state = match layer.state {
                pull_request::State::Open if draft => State::Draft,
                pull_request::State::Open => State::Open,
                pull_request::State::Merged => State::Merged,
                pull_request::State::Closed => State::Closed,
            };
            LayerRow {
                number: layer.number,
                title: title
                    .filter(|title| !title.is_empty())
                    .unwrap_or(branch.clone()),
                branch,
                state,
            }
        });
        StackView {
            number: stack.number,
            base: bounded(&stack.base, NAME),
            layers: layers.collect(),
            cut: stack.cut,
        }
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
    /// How many columns the Code tab's lines are scrolled sideways while they don't wrap. A
    /// frame of the Code tab keeps it within the widest line, so it is 0 while they wrap.
    across: usize,
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
    /// Set while the details don't show the pull requests' tab, as when the pane closes or
    /// the Thread tab shows, so a read again waits until they show.
    hidden: bool,
    /// The ticket of the last read asked for.
    tickets: u64,
    /// The ticket of the read under way, until its answer comes. One read at a time is under
    /// way, so links opened one after another don't each start a read. Another thread opening
    /// leaves it, since its answer still comes.
    running: Option<u64>,
    /// What the reader opened of the conversation, which starts over each time a link's
    /// details open.
    shown: Shown,
    /// The control in the Comments section or the Timeline that ↑ and ↓ chose, which Enter
    /// acts on.
    pick: Option<Control>,
    /// The control whose first row the next frame of the Code tab shows at the top of the
    /// details, as a file picked from the file tree shows.
    reveal: Option<Control>,
    /// The tab of a link's details that shows while they have tabs, which starts over at the
    /// Summary each time they open.
    tab: Tab,
    /// Each tab's scroll, by its place, which it shows at again when the reader goes back to
    /// it. The one that shows is `scroll`.
    kept: [Offset; 3],
    /// Whether the Code tab showed since the details opened, as the desktop's tab stays
    /// mounted once it showed (PullRequestDetailPanel.tsx:525-539).
    mounted: bool,
    /// Set when the Code tab shows for the first time since the details opened, until its
    /// diff is read, or read again where it is stale, as the desktop's tab reads it when it
    /// mounts.
    mount: bool,
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
        self.across = 0;
        self.top = 0;
        self.page = None;
        self.detail = None;
        self.read = None;
        self.asking = false;
        self.shown = Shown::default();
        self.pick = None;
        self.tab = Tab::Summary;
        self.kept = Default::default();
        self.mounted = false;
        self.mount = false;
    }

    /// Drops the page, the details and the host's answer laid out in the old colors, after the
    /// theme changed. A click lands on nothing until the next frame lays them out again.
    pub fn restyle(&mut self) {
        self.page = None;
        self.detail = None;
        if let Some(read) = self.read.as_mut() {
            read.restyle();
        }
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

    /// The tab of the selected link's details that shows, while they show and have tabs: once
    /// the host's detail of the pull request came, as the desktop shows its tabs only then.
    /// Until it does, and after it failed, the details show as the Summary does.
    pub fn tabs(&self) -> Option<Tab> {
        let read = self
            .read
            .as_ref()
            .filter(|read| self.reads_selected(read))?;
        let read = matches!(read.answer, Answer::Read(_));
        (self.viewing && read).then_some(self.tab)
    }

    /// Whether the details offer the Code tab: they have tabs, and the host gives a diff.
    pub fn offers_code(&self) -> bool {
        self.tabs().is_some()
            && self
                .read
                .as_ref()
                .is_some_and(|read| matches!(&read.answer, Answer::Read(summary) if summary.diff))
    }

    /// → in a link's details: shows the next tab, the Timeline after the Summary and the Code
    /// after the Timeline. Says whether it did.
    pub fn forward(&mut self) -> bool {
        let next = match self.tabs() {
            Some(Tab::Summary) => Tab::Timeline,
            Some(Tab::Timeline) if self.offers_code() => Tab::Code,
            _ => return false,
        };
        self.switch(next);
        true
    }

    /// Shows `tab` of the details, at the scroll it had when the reader left it, with no
    /// control chosen. Each tab keeps its own scroll while another shows.
    fn switch(&mut self, tab: Tab) {
        if self.tab == tab {
            return;
        }
        self.kept[self.tab.place()] = self.scroll;
        self.scroll = self.kept[tab.place()];
        self.tab = tab;
        if tab == Tab::Code && !self.mounted {
            self.mounted = true;
            self.mount = true;
        }
        self.pick = None;
        self.relay();
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

    /// Whether the Code tab shows lines that don't wrap and are wider than the details, as the
    /// last frame laid them out, so they scroll sideways.
    pub fn slides(&self) -> bool {
        self.tabs() == Some(Tab::Code)
            && self
                .detail
                .as_ref()
                .is_some_and(|detail| detail.sideways.widest > detail.at.0)
    }

    /// Shift+→ or `L`, or Shift+← or `H` where `right` is false: scrolls the text of the Code
    /// tab's lines that don't wrap half the details' width sideways, no further than the end
    /// of the widest shows.
    pub fn sideways(&mut self, right: bool) {
        let half = self.detail.as_ref().map_or(1, |detail| detail.at.0 / 2);
        self.slide(half.max(1), right);
    }

    /// A turn of the wheel sideways, as a trackpad sends it: scrolls the text of the Code tab's
    /// lines that don't wrap `WHEEL_COLUMNS` columns, as `sideways` does.
    pub fn wheel_sideways(&mut self, right: bool) {
        self.slide(WHEEL_COLUMNS, right);
    }

    /// Scrolls the Code tab's lines that don't wrap `columns` sideways, right or left, no
    /// further than the last frame showed they go.
    fn slide(&mut self, columns: usize, right: bool) {
        if !self.viewing || self.tabs() != Some(Tab::Code) {
            return;
        }
        let across = match right {
            true => self.across.saturating_add(columns),
            false => self.across.saturating_sub(columns),
        };
        let most = self
            .detail
            .as_ref()
            .filter(|detail| detail.index == self.selected)
            .map(|detail| detail.sideways.widest.saturating_sub(detail.at.0));
        self.across = most.map_or(across, |most| across.min(most));
    }

    /// Scrolls a link's details `delta` rows, no further than the last frame showed they go.
    fn scroll_rows(&mut self, delta: isize) {
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
    /// are in view, so the text between two controls is read on the way, and scrolls the rest
    /// of its rows in from that side. With none in view it scrolls a row, and the chosen
    /// control stays chosen while any of its rows show. One that moved out of view as it acted
    /// counts as none chosen.
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
            .and_then(|pick| targets.iter().position(|(_, control)| *control == pick))
            .filter(|&at| shows(&targets[at].0, &view));
        if at.is_none() {
            self.pick = None;
        }
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
            let hidden = match down {
                true => rows.end.saturating_sub(view.end) as isize,
                false => -(view.start.saturating_sub(rows.start) as isize),
            };
            if hidden != 0 {
                self.scroll_rows(hidden);
            }
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
    /// goes because of that, the control that takes its place is chosen, if any. The details
    /// keep their scroll, so what it shows comes in where the reader is, as in a browser, and
    /// the control may move out of view.
    fn act(&mut self, control: Control) {
        if !self
            .read
            .as_ref()
            .is_some_and(|read| self.reads_selected(read))
        {
            return;
        }
        if let Control::TimelineCommit(at) = control {
            self.open_commit(at);
            return;
        }
        if control.on_code() {
            let read = self.read.as_mut().expect("the shown read");
            let commits = match &read.conversation {
                Answer::Read(conversation) => conversation.commits(),
                _ => Vec::new(),
            };
            let code = read.code.get_or_insert_with(Code::new);
            if !code.has(control, &commits) {
                return;
            }
            let listed = code.listed(&commits);
            if code.act(control, &commits) {
                self.shown.close_off_diff();
                self.across = 0;
            }
            self.pick = match control {
                // The list closes on the row that opened it, as a menu gives its button the
                // focus back.
                Control::AllCommits | Control::Commit(_) => Some(Control::Commits),
                // The first of the commits it listed takes the place of Show more once none
                // are left.
                Control::MoreCommits if !code.has(control, &commits) => {
                    Some(Control::Commit(listed))
                }
                // A file picked from the tree is chosen where it shows, at the top of the
                // details, as the desktop scrolls the diff to it.
                Control::TreeFile(at) => {
                    self.reveal = Some(Control::File(at));
                    Some(Control::File(at))
                }
                _ => Some(control),
            }
            .filter(|control| code.has(*control, &commits));
            self.relay();
            return;
        }
        if let Control::More(index) = control {
            let read = self.read.as_mut().expect("the shown read");
            let (Some(code), Answer::Read(conversation)) = (&read.code, &mut read.conversation)
            else {
                return;
            };
            if !code.shows(control, conversation.threads(), &self.shown) {
                return;
            }
            conversation.want_more(index);
            // `Loading more comments…` takes the control's place until the page comes.
            self.pick = None;
            self.relay();
            return;
        }
        if let Control::Thread(_) | Control::OffDiff = control {
            let read = self.read.as_ref().expect("the shown read");
            let (Some(code), Answer::Read(conversation)) = (&read.code, &read.conversation) else {
                return;
            };
            let threads = conversation.threads();
            if !code.shows(control, threads, &self.shown) {
                return;
            }
            self.shown.act(control, conversation);
            self.pick = Some(control).filter(|control| code.shows(*control, threads, &self.shown));
            self.relay();
            return;
        }
        let read = self.read.as_ref().expect("the shown read");
        let Answer::Read(conversation) = &read.conversation else {
            return;
        };
        if !conversation.has(control, &self.shown) {
            return;
        }
        self.shown.act(control, conversation);
        self.pick = Some(control)
            .filter(|control| conversation.has(*control, &self.shown))
            .or_else(|| control.partner())
            .filter(|control| conversation.has(*control, &self.shown));
        self.relay();
    }

    /// Shows the Code tab with the own diff of the commit at `at` among the conversation's
    /// commits as T3 sent them, as a commit on the desktop's Timeline opens it
    /// (PullRequestDetailPanel.tsx:516-519). Another diff than the one shown is read from its
    /// first slice, with the list off the diff closed and the Code at its top. The same one
    /// shows where the reader left it.
    fn open_commit(&mut self, at: usize) {
        if !self.offers_code() {
            return;
        }
        let read = self.read.as_mut().expect("the shown read");
        let Answer::Read(conversation) = &read.conversation else {
            return;
        };
        if !conversation.has(Control::TimelineCommit(at), &self.shown) {
            return;
        }
        let oid = conversation
            .commit(at)
            .expect("a commit it has")
            .oid
            .clone();
        let code = read.code.get_or_insert_with(Code::new);
        if code.show(oid) {
            self.shown.close_off_diff();
            self.kept[Tab::Code.place()] = Offset::Top(0);
            self.across = 0;
        }
        self.switch(Tab::Code);
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
        if pick.on_code() {
            let commits = match &read.conversation {
                Answer::Read(conversation) => conversation.commits(),
                _ => Vec::new(),
            };
            return read.code.as_ref().map(|code| code.action(pick, &commits));
        }
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

    /// Shows the selected link's details in the list's place, from the top of their Summary,
    /// or the list again.
    pub fn toggle(&mut self) {
        if self.viewing {
            self.show_list();
        } else if !entries_of(self.list.as_ref()).is_empty() {
            self.viewing = true;
            self.scroll = Offset::Top(0);
            self.across = 0;
            self.tab = Tab::Summary;
            self.kept = Default::default();
            self.mounted = false;
            self.mount = false;
            self.current = false;
            self.asking = true;
            // What was opened starts over, so the rows laid out for it go too, even where the
            // link, the width and the minute are the same.
            self.shown = Shown::default();
            self.pick = None;
            self.reveal = None;
            self.relay();
        }
    }

    /// The read of the selected link's pull request that opening its details asks for. Opening
    /// them wants a read, unless `enabled` is false because the environment doesn't read pull
    /// requests or the thread names no project, or a read of the link is under way or one from
    /// the last minute stands. The details show that the read is under way until `answer`
    /// takes its answer. A read wanted while another is under way waits, and the caller calls
    /// again once that one's answer comes. A read whose answers may be out of date is read
    /// again while its details show, and they show its answers until the new ones come. None
    /// while nothing waits.
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
        let showing = self.showing_read();
        let read = self.read.as_mut()?;
        let ticket = self.tickets + 1;
        let mut updated = None;
        if read.ticket.is_none() {
            read.ticket = Some(ticket);
            read.at = now;
        } else if read.again && showing {
            read.again = false;
            // Only answers that both came and that the last read missed nothing of are kept,
            // as a failure is never reused.
            let live = std::mem::take(&mut read.live) && !read.missed;
            if live
                && let (Answer::Read(summary), Answer::Read(_)) = (&read.answer, &read.conversation)
            {
                updated = Some(summary.updated.clone());
            }
            read.next = Some(Next {
                ticket,
                at: now,
                answer: None,
                conversation: None,
                stack: None,
            });
        } else {
            return None;
        }
        self.tickets = ticket;
        self.running = Some(ticket);
        read.asked = now;
        let invalidate = std::mem::take(&mut read.invalidate);
        read.echo = invalidate;
        let stack = std::mem::take(&mut read.stack_due) || updated.is_none();
        Some(Ask {
            ticket,
            host: read.host.clone(),
            repository: read.repository.clone(),
            number: read.number,
            invalidate,
            updated,
            stack,
        })
    }

    /// Wants a read of the selected link's pull request, unless one under way or from the last
    /// minute stands. A read of the link that has an answer is read again and shows it until
    /// the new answers come, as the desktop's queries show what they hold while they read
    /// again (`Atom.swr` in packages/client-runtime/src/state/runtime.ts).
    fn want(&mut self, now: i64) {
        let Some(entry) = entries_of(self.list.as_ref()).get(self.selected) else {
            return;
        };
        if let Some(read) = self.read.as_mut().filter(|read| read.key == entry.key) {
            if read.stands(now) {
                // Its answers show, and the detail is read again unless it just came. A stack
                // that came `FRESH` or more ago is read again with it, though the detail just
                // came, as the desktop's stack query reads again when the details mount once
                // it is stale (`Atom.swr` in packages/client-runtime/src/state/runtime.ts).
                if self.stack_stale(now) {
                    self.read.as_mut().expect("the shown read").stack_due = true;
                    self.read_live(now);
                } else {
                    self.arrive(now);
                }
                return;
            }
            if read.answered() {
                read.again = true;
                read.live = false;
                self.relay();
                return;
            }
        }
        self.read = Some(Read::of(entry, now));
        self.relay();
    }

    /// Whether the details show the link whose read is held.
    fn showing_read(&self) -> bool {
        let shown = entries_of(self.list.as_ref()).get(self.selected);
        !self.hidden
            && self.viewing
            && self
                .read
                .as_ref()
                .zip(shown)
                .is_some_and(|(read, entry)| read.key == entry.key)
    }

    /// Whether a read of a link's pull request is held, which T3's refresh numbers can make
    /// out of date.
    pub fn holds_read(&self) -> bool {
        self.read.is_some()
    }

    /// Notes that T3 said its pull request reads may have changed, as a new number on
    /// `pullRequests.subscribeRefreshes` says. A read asked for before is read again: at once
    /// while its details show, once a read under way is done, and when they show again
    /// otherwise, as the desktop reads a query again on the signal while something shows it
    /// and only marks it stale when nothing does (`refreshOnSignalWhileRead` in
    /// packages/client-runtime/src/state/runtime.ts). Says whether the details show it.
    pub fn refresh(&mut self) -> bool {
        let showing = self.showing_read();
        let Some(read) = self.read.as_mut().filter(|read| read.ticket.is_some()) else {
            return false;
        };
        // The first number after a person's refresh is that refresh's own, and the read it
        // started reads what moved it.
        if std::mem::take(&mut read.echo) {
            return false;
        }
        read.again = true;
        read.live = false;
        if let Some(code) = read.code.as_mut() {
            code.again();
        }
        if showing {
            self.relay();
        }
        showing
    }

    /// `r` in a link's details: reads its pull request from its host again, as the desktop's
    /// Refresh does (PullRequestDetailPanel.tsx:857-875). The read sends
    /// `pullRequests.invalidate` first, so T3 asks the host rather than answer from its cache,
    /// and the details show the last answers until the new ones come. Only a link whose details
    /// hold a read does this, and not while a read of it waits or is under way, as the
    /// desktop's Refresh waits for the one that runs. Says whether it wants a read.
    pub fn reload(&mut self, now: i64) -> bool {
        if !self.reloads() {
            return false;
        }
        let read = self.read.as_mut().expect("a read of the shown link");
        if read.answered() {
            read.again = true;
            read.live = false;
            read.invalidate = true;
            // The desktop's Refresh reads the diff again too (PullRequestDetailPanel.tsx:832).
            if let Some(code) = read.code.as_mut() {
                code.again();
            }
        } else {
            // Nothing came to show meanwhile, so it reads as the details' first read does.
            let entry = &entries_of(self.list.as_ref())[self.selected];
            let mut read = Read::of(entry, now);
            read.invalidate = true;
            self.read = Some(read);
        }
        self.relay();
        true
    }

    /// Whether `r` would read the shown link's pull request from its host again: its details
    /// show and hold a read of it, and no read of it waits or is under way.
    pub fn reloads(&self) -> bool {
        self.viewing
            && self.read.as_ref().is_some_and(|read| {
                let done = read.ticket.is_some() && read.ticket != self.running;
                self.reads_selected(read) && done && read.next.is_none()
            })
    }

    /// When the details read the detail they show again to keep it current, as the desktop's
    /// `useLiveRefresh` does while its panel shows a pull request: `LIVE` after its last read
    /// was asked for. None unless they show a read that is done, with no read again wanted.
    pub fn live_at(&self) -> Option<i64> {
        let read = self.read.as_ref().filter(|_| self.showing_read())?;
        let done = read.ticket.is_some() && read.ticket != self.running && read.next.is_none();
        (done && !read.again).then_some(read.asked + LIVE)
    }

    /// Reads the detail the details show again once `live_at` is past at `now`, unless the
    /// last key, click or wheel at `input` came `IDLE` or more before `now`, as the desktop
    /// stops its interval for a reader who left and checks it against the time it runs. A Mac
    /// that slept past `live_at` so reads nothing on waking, and the next key reads it at
    /// once. The read reads the conversation again only when the detail's `updatedAt`
    /// changed, as the desktop does (PullRequestDetailPanel.tsx:836-856). Says whether it
    /// wants a read.
    pub fn live(&mut self, now: i64, input: i64) -> bool {
        let Some(due) = self.live_at() else {
            return false;
        };
        if now < due || now.saturating_sub(input) >= IDLE {
            return false;
        }
        self.read_live(now)
    }

    /// Reads the detail the details show again at `now`, when you arrive at them: their link's
    /// details open again, you come back to t3term after `IDLE` or more without a key, click
    /// or wheel, or its terminal gains focus. The desktop's `useLiveRefresh` reads on each of
    /// these (`onArrival`), and on arriving at a pull request only once it read it before
    /// (`shouldRefreshOnArrival`). Not while the detail they show came, or their last read
    /// was asked for, under `SOONEST` ago, so a read again that failed isn't asked again at
    /// once. Says whether it wants a read.
    pub fn arrive(&mut self, now: i64) -> bool {
        let came = self.read.as_ref().filter(|_| self.live_at().is_some());
        match came.map(|read| read.at.max(read.asked)) {
            Some(at) if now - at >= SOONEST => self.read_live(now),
            _ => false,
        }
    }

    /// Whether the details show a read that is done whose stack came `FRESH` or more before
    /// `now`.
    fn stack_stale(&self, now: i64) -> bool {
        let read = self.read.as_ref().filter(|_| self.live_at().is_some());
        let at = read.and_then(|read| read.stack_at);
        at.is_some_and(|at| now.saturating_sub(at) >= FRESH)
    }

    /// Wants the shown read again to keep it current, as `live` and `arrive` say: the detail,
    /// and the conversation only once it changed. A read that failed reads both, as a first
    /// read does, as the desktop's live refresh reads a detail it couldn't read.
    fn read_live(&mut self, now: i64) -> bool {
        let read = self.read.as_mut().expect("the shown read");
        if read.answered() {
            read.again = true;
            read.live = true;
        } else {
            // Nothing came to show, so it reads as the details' first read does.
            let entry = &entries_of(self.list.as_ref())[self.selected];
            self.read = Some(Read::of(entry, now));
        }
        self.relay();
        true
    }

    /// Notes whether the details show the pull requests' tab. Once they don't, as when the
    /// pane closes or the Thread tab shows, a read that waits to be asked for is dropped, so no
    /// host is asked for a link nobody looks at, and showing them again wants it again. A read
    /// again waits until they show.
    pub fn shown(&mut self, shown: bool) {
        self.hidden = !shown;
        if !shown && self.unqueue() {
            self.asking = true;
        }
    }

    /// Drops a read that waits to be asked for, once nothing shows it or it can't be asked.
    /// Says whether there was one.
    fn unqueue(&mut self) -> bool {
        if self.read.as_ref().is_some_and(|read| read.ticket.is_none()) {
            self.read = None;
            self.relay();
            return true;
        }
        false
    }

    /// Takes part of the answer to read `ticket`, which came at `now`: the pull request's
    /// detail or conversation, or why reading it failed. Says whether what the details show
    /// changed. An answer to an earlier read, or to a read of another thread's link, is
    /// dropped. The answers of a read that replaces others are held until it is done, then
    /// replace them as `Read::settle` says. Once the read is done, another can be asked for.
    pub fn answer(&mut self, ticket: u64, part: Part, now: i64) -> bool {
        let done = matches!(part, Part::Done);
        if done && self.running == Some(ticket) {
            self.running = None;
        }
        let Some(read) = self.read.as_mut() else {
            return false;
        };
        let ours = read.ticket == Some(ticket)
            || read.next.as_ref().is_some_and(|next| next.ticket == ticket);
        if done && ours {
            // A refresh number that comes after the read is done is another change.
            read.echo = false;
        }
        if let Some(next) = read.next.as_mut().filter(|next| next.ticket == ticket) {
            // A read that replaces the answers shows nothing until both came.
            match part {
                Part::Detail(answer) => {
                    if answer.is_ok() {
                        next.at = now;
                    }
                    let answer = answer.map(|detail| Box::new(Summary::of(&detail)));
                    next.answer = Some(answer.map_err(|failure| bounded(&failure, FAILURE)));
                    return false;
                }
                Part::Activity(answer) => {
                    let answer = answer.map(|activity| Box::new(Conversation::of(&activity)));
                    next.conversation = Some(answer.map_err(|failure| bounded(&failure, FAILURE)));
                    return false;
                }
                Part::Stack(answer) => {
                    next.stack = Some(StackRead::of(answer, now));
                    return false;
                }
                Part::Done => {
                    let next = read.next.take().expect("the read under way");
                    read.settle(next);
                }
            }
        } else if read.ticket != Some(ticket) || done {
            return false;
        } else {
            match part {
                Part::Detail(Ok(detail)) => {
                    read.at = now;
                    read.answer = Answer::Read(Box::new(Summary::of(&detail)));
                }
                Part::Detail(Err(failure)) => {
                    read.answer = Answer::Failed(bounded(&failure, FAILURE));
                }
                Part::Activity(Ok(activity)) => {
                    read.conversation = Answer::Read(Box::new(Conversation::of(&activity)));
                }
                Part::Activity(Err(failure)) => {
                    read.conversation = Answer::Failed(bounded(&failure, FAILURE));
                }
                Part::Stack(answer) => read.take_stack(StackRead::of(answer, now)),
                Part::Done => {}
            }
        }
        // The Timeline takes the pull request's opening and end from its detail, so it is made
        // once both answers came, in either order.
        if let (Answer::Read(summary), Answer::Read(conversation)) =
            (&read.answer, &mut read.conversation)
        {
            conversation.follow(&summary.lifecycle);
            self.shown.prune(conversation);
        }
        // A commit a rebase took out of the change takes the Code tab back to the whole of it.
        if let (Some(code), Answer::Read(conversation)) = (&mut read.code, &read.conversation)
            && code.follow(&conversation.commits())
        {
            self.shown.close_off_diff();
        }
        // A detail that says the host gives no diff takes the Code tab away, and the Summary
        // shows in its place, as the desktop's panel moves the reader (1444-1455).
        if self.tabs() == Some(Tab::Code) && !self.offers_code() {
            self.switch(Tab::Summary);
        }
        // The rows move, so no control stays chosen.
        self.pick = None;
        self.relay();
        true
    }

    /// The read of a slice of the diff that the Code tab wants, as `Code::ask` says: the first
    /// slice once the tab shows for the first time since the details opened, or again where
    /// it is stale, and the next once the end of the files comes near. None unless `enabled`
    /// and the tab shows. While a read that sends `pullRequests.invalidate` is under way, the
    /// diff waits for it, so T3 forgets its cached diff before the read asks for it.
    pub fn take_diff_ask(&mut self, enabled: bool, now: i64) -> Option<DiffAsk> {
        if !enabled || !self.showing_read() || self.tabs() != Some(Tab::Code) {
            return None;
        }
        let mount = std::mem::take(&mut self.mount);
        let ticket = self.tickets + 1;
        let read = self.read.as_mut().expect("the shown read");
        let code = read.code.get_or_insert_with(Code::new);
        if mount && code.stale(now) {
            code.again();
        }
        if mount && code.viewed_stale(now) {
            code.view_again();
        }
        if read.invalidate || read.echo {
            return None;
        }
        let code = read.code.as_mut().expect("made above");
        let cursor = code.ask(ticket)?;
        self.tickets = ticket;
        Some(DiffAsk {
            ticket,
            host: read.host.clone(),
            repository: read.repository.clone(),
            number: read.number,
            cursor,
            commit: code.commit().map(str::to_string),
        })
    }

    /// The read of the next page of a review thread's remarks that Enter on its `Load more
    /// comments` asked for, while the details show the read. None unless `enabled`.
    pub fn take_comments_ask(&mut self, enabled: bool) -> Option<CommentsAsk> {
        if !enabled || !self.showing_read() {
            return None;
        }
        let ticket = self.tickets + 1;
        let read = self.read.as_mut().expect("the shown read");
        let Answer::Read(conversation) = &mut read.conversation else {
            return None;
        };
        let (thread, cursor) = conversation.take_wanted(ticket)?;
        self.tickets = ticket;
        Some(CommentsAsk {
            ticket,
            host: read.host.clone(),
            repository: read.repository.clone(),
            number: read.number,
            thread,
            cursor,
        })
    }

    /// The read of the reader's marks on files viewed that the Code tab wants, as
    /// `Code::ask_viewed` says: once the tab first shows, when it shows again after the marks
    /// went stale, and with each read of the diff again. None unless `enabled`, the tab shows
    /// and the detail says the host or T3 keeps the marks. A read that sends
    /// `pullRequests.invalidate` holds it, as it holds the diff.
    pub fn take_viewed_ask(&mut self, enabled: bool) -> Option<ViewedAsk> {
        if !enabled || !self.showing_read() || self.tabs() != Some(Tab::Code) {
            return None;
        }
        let ticket = self.tickets + 1;
        let read = self.read.as_mut().expect("the shown read");
        let kept = matches!(&read.answer, Answer::Read(summary) if summary.viewed_files.is_some());
        if !kept || read.invalidate || read.echo {
            return None;
        }
        if !read.code.as_mut()?.ask_viewed(ticket) {
            return None;
        }
        self.tickets = ticket;
        Some(ViewedAsk {
            ticket,
            host: read.host.clone(),
            repository: read.repository.clone(),
            number: read.number,
        })
    }

    /// Takes the answer to read `ticket` of the marks on files viewed, which came at `now`, or
    /// why it failed. Says whether what the details show may have changed. An answer to a read
    /// the details no longer hold is dropped.
    pub fn answer_viewed(
        &mut self,
        ticket: u64,
        answer: Result<Box<pull_request::FilesViewed>, String>,
        now: i64,
    ) -> bool {
        let code = self.read.as_mut().and_then(|read| read.code.as_mut());
        if !code.is_some_and(|code| code.take_viewed(ticket, answer, now)) {
            return false;
        }
        self.relay();
        true
    }

    /// The read of a file's text that Enter on a run of unchanged lines on the Code tab asked
    /// for, while the details show the read. None unless `enabled`.
    pub fn take_text_ask(&mut self, enabled: bool) -> Option<TextAsk> {
        if !enabled || !self.showing_read() {
            return None;
        }
        let ticket = self.tickets + 1;
        let read = self.read.as_mut().expect("the shown read");
        let code = read.code.as_mut()?;
        let (change, old_path, new_path) = code.ask_text(ticket)?;
        self.tickets = ticket;
        Some(TextAsk {
            ticket,
            host: read.host.clone(),
            repository: read.repository.clone(),
            number: read.number,
            commit: code.commit().map(str::to_string),
            change,
            old_path,
            new_path,
        })
    }

    /// Takes the answer to read `ticket` of a file's text, or why it failed. Says whether what
    /// the details show changed. A run whose lines all show once it came goes, and so does
    /// its choice. An answer to a read the details no longer hold is dropped.
    pub fn answer_text(
        &mut self,
        ticket: u64,
        answer: Result<Box<pull_request::FileContents>, String>,
    ) -> bool {
        let Some(read) = self.read.as_mut() else {
            return false;
        };
        let Some(code) = read.code.as_mut() else {
            return false;
        };
        if !code.take_text(ticket, answer) {
            return false;
        }
        if let Some(pick @ Control::Stretch(..)) = self.pick
            && !code.has(pick, &[])
        {
            self.pick = None;
        }
        self.relay();
        true
    }

    /// Takes the answer to read `ticket` of a page of a review thread's remarks, or why it
    /// failed. Says whether what the details show changed. An answer to a read the details
    /// no longer hold is dropped.
    pub fn answer_comments(
        &mut self,
        ticket: u64,
        answer: Result<Box<pull_request::CommentPage>, String>,
    ) -> bool {
        let Some(read) = self.read.as_mut() else {
            return false;
        };
        let Answer::Read(conversation) = &mut read.conversation else {
            return false;
        };
        let answer = answer.as_deref().map_err(String::as_str);
        if !conversation.take_page(ticket, answer) {
            return false;
        }
        self.relay();
        true
    }

    /// Takes the answer to read `ticket` of a slice of the diff, which came at `now`, or why it
    /// failed. Says whether what the details show changed. A first slice replaces the files
    /// shown, so no control on them stays chosen. The list of commits above them keeps its
    /// own, as the desktop's menu gives its button the focus back after a pick. An answer to
    /// another read is dropped.
    pub fn answer_diff(
        &mut self,
        ticket: u64,
        answer: Result<Box<pull_request::Diff>, String>,
        now: i64,
    ) -> bool {
        let code = self.read.as_mut().and_then(|read| read.code.as_mut());
        let Some(first) = code.and_then(|code| code.take(ticket, answer, now)) else {
            return false;
        };
        if first && !self.pick.is_some_and(Control::on_commits) {
            self.pick = None;
        }
        self.relay();
        true
    }

    /// Whether the Code tab wants its next slice read, as the last frame found the end of its
    /// files within a page of the view.
    pub fn code_due(&self) -> bool {
        let code = self.read.as_ref().and_then(|read| read.code.as_ref());
        code.is_some_and(Code::due)
    }

    /// Notes that the details' rows changed, so the next frame lays them out again and a click
    /// before it lands on nothing.
    fn relay(&mut self) {
        self.detail = None;
        self.page = None;
        self.current = false;
    }

    /// ← in a link's details: shows the tab before the one that shows, the Timeline before the
    /// Code and the Summary before the Timeline, or else the list again. Says whether the
    /// details showed.
    pub fn back(&mut self) -> bool {
        let before = match self.tabs() {
            Some(Tab::Code) => Tab::Timeline,
            Some(Tab::Timeline) => Tab::Summary,
            _ => return self.show_list(),
        };
        self.switch(before);
        true
    }

    /// Shows the list again in place of a link's details, with that link still selected. Says
    /// whether the details showed.
    fn show_list(&mut self) -> bool {
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

    /// A click at `column` of row `offset` of the page the last frame drew. On a link's rows
    /// it selects the link, or shows its details when it is selected already. On the row above
    /// the details it shows the tab whose name it is on, or else the list again. Says whether
    /// it landed on one of those and changed what shows. Once the selection or the scroll
    /// moved, a click lands on nothing until a frame draws the page.
    pub fn click(&mut self, column: usize, offset: usize) -> bool {
        let Some(page) = self.page.as_ref().filter(|_| self.current) else {
            return false;
        };
        let tab = page
            .tabs
            .iter()
            .find(|(columns, _)| offset == 0 && columns.contains(&column));
        if let Some(&(_, tab)) = tab {
            let changed = self.tab != tab;
            self.switch(tab);
            return changed;
        }
        match page.rows.get(offset).copied().flatten() {
            Some(Spot::Link(index)) if index == self.selected => self.toggle(),
            Some(Spot::Link(index)) => self.select(index),
            Some(Spot::Back) => {
                self.show_list();
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

    /// The rows of the selected link's details at `width`, as the tab that shows has them,
    /// worked out again only for another link, width or minute, or once what they show changed.
    fn prepare_detail(&mut self, width: usize, now: i64, theme: &Theme) -> usize {
        let (index, at) = (self.selected, (width, now.div_euclid(60_000)));
        let cached = self.detail.as_ref().map(|detail| (detail.index, detail.at));
        if cached != Some((index, at)) {
            let tab = self.tabs().unwrap_or(Tab::Summary);
            let lines = match entries_of(self.list.as_ref()).get(index) {
                Some(entry) => {
                    let read = self.read.as_mut().filter(|read| read.key == entry.key);
                    if let Some(read) = read {
                        if let (Tab::Summary, Answer::Read(summary)) = (tab, &mut read.answer) {
                            summary.lay_out(width, theme);
                        }
                        if let (Tab::Code, Some(code)) = (tab, &mut read.code) {
                            code.prepare(width, theme);
                        }
                        if let Answer::Read(conversation) = &mut read.conversation {
                            match tab {
                                Tab::Summary => conversation.lay_out(width, theme, &self.shown),
                                Tab::Timeline => {
                                    conversation.lay_out_timeline(width, theme, &self.shown);
                                }
                                Tab::Code => {
                                    let code = read.code.as_ref();
                                    let threads = conversation.threads();
                                    let showing = code.map_or_else(Vec::new, |code| {
                                        code.threads_shown(threads, &self.shown)
                                    });
                                    conversation.lay_out_threads(&showing, width, theme);
                                }
                            }
                        }
                    }
                    let read = self.read.as_ref().filter(|read| read.key == entry.key);
                    detail_lines(entry, read, &self.shown, tab, width, now, theme)
                }
                None => (Vec::new(), Vec::new(), Sideways::default()),
            };
            let (lines, targets, sideways) = lines;
            self.detail = Some(Detail {
                index,
                at,
                lines,
                targets,
                sideways,
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
        let (mut scroll, mut across) = (0, 0);
        if self.viewing {
            let total = self.prepare_detail(width, now, theme);
            let reveal = self.reveal.take();
            let targets = self.detail.as_ref().map(|detail| &detail.targets);
            let rows = targets
                .into_iter()
                .flatten()
                .find(|(_, control)| Some(*control) == reveal);
            if let Some((rows, _)) = rows {
                self.scroll = Offset::Top(rows.start);
            }
            // The row back to the list comes first.
            let last = total.saturating_sub(height.saturating_sub(1));
            scroll = match self.scroll {
                Offset::Top(row) => row.min(last),
                Offset::End(up) => last - up.min(last),
            };
            self.scroll = Offset::Top(scroll);
            // The Code tab reads its next slice once the end of its files is within a page of
            // the view, as the desktop's sentinel loads it 240 pixels ahead.
            let room = height.saturating_sub(1);
            let near = self.tabs() == Some(Tab::Code) && scroll + 2 * room >= total;
            if near && let Some(code) = self.read.as_mut().and_then(|read| read.code.as_mut()) {
                code.near_end();
            }
            // The Code tab's lines that don't wrap scroll sideways until the end of the widest
            // shows, and the other tabs leave them where they were.
            if self.tabs() == Some(Tab::Code) {
                let widest = self.detail.as_ref().map_or(0, |d| d.sideways.widest);
                across = self.across.min(widest.saturating_sub(width));
                self.across = across;
            }
        }
        let shape = Shape {
            width,
            height,
            top: self.top,
            selected: self.selected,
            viewing: self.viewing,
            scroll,
            across,
            minute: now.div_euclid(60_000),
            pick: self.pick.filter(|_| self.viewing),
            focused,
            tabs: self.tabs(),
            code: self.offers_code(),
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
        tabs: Vec::new(),
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

/// The page of the selected link's details for `shape`: the row back to the list, with the
/// names of the tabs while they have them and where the link is in the list, then as many rows
/// of the details as fit from row `shape.scroll`. Where the row is too narrow, where the link
/// is goes first, and then the names are cut.
fn view(list: Option<&List>, detail: Option<&Detail>, shape: Shape, theme: &Theme) -> Page {
    let mut page = Page {
        shape,
        lines: Vec::new(),
        rows: Vec::new(),
        above: false,
        below: false,
        per_page: 1,
        tabs: Vec::new(),
    };
    let (width, height) = (shape.width, shape.height);
    if width == 0 || height == 0 {
        return page;
    }
    let lines: &[Line<'static>] = match detail {
        Some(detail) if detail.index == shape.selected => &detail.lines,
        _ => &[],
    };
    let back = Style::new().fg(theme.focus).add_modifier(Modifier::BOLD);
    let mut left = vec![Span::styled("← List", back)];
    if let Some(shown) = shape.tabs {
        let active = back.add_modifier(Modifier::UNDERLINED);
        let muted = Style::new().fg(theme.muted);
        for &tab in Tab::offered(shape.code) {
            let start = spans_width(&left);
            let style = if tab == shown { active } else { muted };
            left.push(Span::raw("  "));
            left.push(Span::styled(tab.name(), style));
            let end = spans_width(&left).min(width);
            // A name of which no column shows can't be clicked.
            if start + 2 < end {
                page.tabs.push((start..end, tab));
            }
        }
    }
    let back = fitted(left, width);
    let count = entries_of(list).len();
    let place = format!("{} of {count}", shape.selected + 1);
    // Where the link is shows only a gap of two columns away from the rest.
    let place = match spans_width(&back) + 2 + place.width() <= width {
        true => vec![Span::styled(place, Style::new().fg(theme.muted))],
        false => Vec::new(),
    };
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
    let runs: &[(Range<usize>, usize, Option<usize>)] = match detail {
        Some(detail) if detail.index == shape.selected => &detail.sideways.runs,
        _ => &[],
    };
    let mut runs = runs[runs.partition_point(|(rows, ..)| rows.end <= shape.scroll)..]
        .iter()
        .peekable();
    for (at, line) in lines.iter().enumerate().skip(shape.scroll).take(room) {
        while targets.next_if(|(rows, _)| rows.end <= at).is_some() {}
        let control = targets
            .peek()
            .filter(|(rows, _)| rows.contains(&at))
            .map(|(_, control)| *control);
        while runs.next_if(|(rows, ..)| rows.end <= at).is_some() {}
        let run = runs
            .peek()
            .filter(|(rows, ..)| rows.contains(&at))
            .map(|(_, lead, left)| (*lead, *left));
        // The chosen control shows highlighted across the row while the details have the
        // keyboard.
        if control.is_some() && control == shape.pick && shape.focused {
            let mut spans = line.spans.clone();
            let pad = shape.width.saturating_sub(line.width());
            if pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
            }
            page.lines.push(Line::from(with_bg(spans, theme.highlight)));
        } else if let Some((lead, left)) = run {
            page.lines.push(match left {
                Some(left) => slid_beside(line, lead, shape.across, width, left),
                None => slid(line, lead, shape.across, width),
            });
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

/// A row of a line of the Code tab that doesn't wrap, scrolled `across` columns sideways: its
/// first `lead` columns, its number and mark, then its text from `across` columns in, in at
/// most `width` columns. A wide character the left edge cuts shows as spaces where its right
/// part was, and one the right edge cuts doesn't show.
fn slid(line: &Line<'static>, lead: usize, across: usize, width: usize) -> Line<'static> {
    let hidden = lead..lead + across;
    let mut spans = Vec::with_capacity(line.spans.len());
    // The column of the row as laid out, and of the row as it shows.
    let (mut column, mut shown) = (0, 0);
    'spans: for span in &line.spans {
        let mut text = String::new();
        for piece in clusters(&span.content) {
            let columns = piece.width();
            let (start, end) = (column, column + columns);
            column = end;
            if hidden.contains(&start) && end <= hidden.end {
                continue;
            }
            // Of one the left edge cuts, the columns past it.
            let (piece, columns) = match hidden.contains(&start) {
                true => (" ".repeat(end - hidden.end), end - hidden.end),
                false => (piece.to_string(), columns),
            };
            if shown + columns > width {
                if !text.is_empty() {
                    spans.push(Span::styled(text, span.style));
                }
                break 'spans;
            }
            text.push_str(&piece);
            shown += columns;
        }
        if !text.is_empty() {
            spans.push(Span::styled(text, span.style));
        }
    }
    Line::from(spans).style(line.style)
}

/// A row of the Code tab's lines side by side, cut as `slid` cuts a row: the text of each side
/// past its numbers and mark scrolled `across` and cut at the end of its side, the left one
/// `left` columns wide, with the columns between them kept.
fn slid_beside(
    line: &Line<'static>,
    lead: usize,
    across: usize,
    width: usize,
    left: usize,
) -> Line<'static> {
    let spans = &line.spans;
    let side = |spans: &[Span<'static>]| Line::from(spans.to_vec());
    let mut out = slid(&side(&spans[..SIDE.min(spans.len())]), lead, across, left).spans;
    let shown = spans_width(&out);
    if shown < left {
        out.push(Span::raw(" ".repeat(left - shown)));
    }
    let between = spans.get(SIDE + 1).cloned();
    let room = width.saturating_sub(left + between.as_ref().map_or(0, Span::width));
    out.extend(between);
    let right = side(spans.get(SIDE + 2..).unwrap_or_default());
    out.extend(slid(&right, lead, across, room).spans);
    Line::from(out).style(line.style)
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
        Synced::Read(snapshot) => snapshot.state.shown(theme),
    }
}

impl State {
    /// A state's mark, its name and their color.
    fn shown(self, theme: &Theme) -> (&'static str, &'static str, Color) {
        match self {
            State::Open => ("●", "Open", theme.success),
            State::Draft => ("◌", "Draft", theme.muted),
            State::Merged => ("●", "Merged", theme.violet),
            State::Closed => ("●", "Closed", theme.error_fg),
        }
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
/// and the lines it changes. On the Summary, what its host said of it when the details read
/// it follows, then each under its label its URL, whole as plain text to copy, its repository,
/// its branches, its author, and who linked it and when it last changed and synced, then what
/// watching it does and the stack it heads. On the Timeline, the Timeline follows, and on the
/// Code, the diff's files. Each value wraps to every row it takes, so the reader can scroll to
/// all of it, and each was bounded when it was read.
fn detail_lines(
    entry: &Entry,
    read: Option<&Read>,
    shown: &Shown,
    tab: Tab,
    width: usize,
    now: i64,
    theme: &Theme,
) -> (Vec<Line<'static>>, Targets, Sideways) {
    if width == 0 {
        return (Vec::new(), Vec::new(), Sideways::default());
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
    if let (Tab::Timeline, Some(read)) = (tab, read) {
        lines.push(Line::default());
        let at = lines.len();
        // A commit opens its own diff on the Code tab where the host gives a diff.
        let opens = matches!(&read.answer, Answer::Read(summary) if summary.diff);
        let timeline =
            conversation::timeline_section(&read.conversation, shown, opens, width, now, theme);
        lines.extend(timeline.lines);
        let moved = |rows: Range<usize>| rows.start + at..rows.end + at;
        targets.extend(
            timeline
                .targets
                .into_iter()
                .map(|(rows, c)| (moved(rows), c)),
        );
        return (lines, targets, Sideways::default());
    }
    if let (Tab::Code, Some(read)) = (tab, read) {
        lines.push(Line::default());
        let at = lines.len();
        // The review threads and the commits come with the conversation, and none show until
        // it came.
        let (threads, commits) = match &read.conversation {
            Answer::Read(conversation) => (conversation.threads(), conversation.commits()),
            _ => (&[][..], Vec::new()),
        };
        let viewed = match &read.answer {
            Answer::Read(summary) => summary.viewed_files,
            _ => None,
        };
        let reads = Reads {
            threads,
            commits: &commits,
            viewed,
        };
        // Until the tab asks for its first slice, it shows as it does while that read runs.
        let (code, controls, mut sideways) = match &read.code {
            Some(code) => code.lay_out(reads, shown, width, now, theme),
            None => Code::new().lay_out(reads, shown, width, now, theme),
        };
        lines.extend(code);
        let moved = |rows: Range<usize>| rows.start + at..rows.end + at;
        targets.extend(controls.into_iter().map(|(rows, c)| (moved(rows), c)));
        for (rows, ..) in &mut sideways.runs {
            *rows = moved(rows.clone());
        }
        return (lines, targets, sideways);
    }
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
    (lines, targets, Sideways::default())
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
        // A failure shows until a read again is under way, which has no summary to show over,
        // so it reads as a first read does.
        Answer::Failed(failure) if read.next.is_none() => {
            lines.extend(whole(failure, width, warning));
            return (lines, Vec::new());
        }
        Answer::Reading | Answer::Failed(_) => {
            let reading = format!("Reading from {}…", entry.host);
            lines.extend(whole(&reading, width, muted));
            return (lines, Vec::new());
        }
        Answer::Read(summary) => summary,
    };
    let at = match age(read.at, now).as_str() {
        "now" => "just now".to_string(),
        ago => format!("{ago} ago"),
    };
    let when = match read.next.is_some() {
        true => format!("Refreshing from {}…", entry.host),
        false => format!("Read from {} {at}", entry.host),
    };
    lines.extend(whole(&when, width, muted));
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
    // The desktop shows the stack in the details' header, beside the merge controls. Until a
    // read of it came, and after a read failed with none to keep, it shows the one saved on
    // the link where the host keeps stacks (pullRequestStackSnapshot.ts:51-73).
    let saved = summary.stacks && (read.stack_at.is_none() || read.stack_failed);
    let stack = match (read.stack.as_deref(), entry.saved.as_deref()) {
        (Some(stack), _) => Some((stack, read.stack_failed.then_some(STACK_STALE))),
        (None, Some(stack)) if saved => {
            let note = if read.stack_failed {
                STACK_STALE
            } else {
                STACK_SAVED
            };
            Some((stack, Some(note)))
        }
        _ => None,
    };
    if let Some((stack, note)) = stack {
        let (label, rows) = stack_lines(stack, entry.number, note, width, theme);
        section(&label, rows);
    }
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

/// The stack's label, which names it and where pull request `number` sits in it, as the
/// desktop's stack button does (PullRequestStackMenu.tsx:130-146), and its rows: `note`, which
/// says where the stack may be out of date, then its layers from the top down, as the
/// desktop's menu lists them (PullRequestStackLayers.tsx:17-38), then the branch the bottom
/// one merges into.
fn stack_lines(
    stack: &StackView,
    number: u64,
    note: Option<&str>,
    width: usize,
    theme: &Theme,
) -> (String, Vec<Line<'static>>) {
    let muted = Style::new().fg(theme.muted);
    let size = stack.layers.len();
    let label = match stack.layers.iter().position(|layer| layer.number == number) {
        Some(at) => format!("Stack #{} · layer {} of {size}", stack.number, at + 1),
        None => format!("Stack #{}", stack.number),
    };
    let mut lines = Vec::new();
    if let Some(note) = note {
        lines.extend(whole(note, width, muted));
    }
    if stack.cut {
        let note = "T3 sent more layers than t3term keeps, so the top ones are left out.";
        lines.extend(whole(note, width, muted));
    }
    for layer in stack.layers.iter().rev() {
        lines.extend(layer_lines(layer, layer.number == number, width, theme));
    }
    lines.extend(whole(&format!("↳ {}", stack.base), width, muted));
    (label, lines)
}

/// A layer of a stack, as the desktop's `PullRequestStackLayerContent` shows it: its state's
/// mark and its title, with `✓` at the right on the pull request the details show, then its
/// number, its branch and its state.
fn layer_lines(layer: &LayerRow, current: bool, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let (mark, state, color) = layer.state.shown(theme);
    let title = match current {
        true => Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
        false => Style::new().fg(theme.fg),
    };
    let muted = Style::new().fg(theme.muted);
    let about = format!("#{} · {} · {state}", layer.number, layer.branch);
    let check = if current { "✓" } else { "" };
    // The mark and a space at the left, and a space and the check at the right.
    let room = width.saturating_sub(4);
    if room < 4 {
        let mut lines = whole(&format!("{mark} {} {check}", layer.title), width, title);
        lines.extend(whole(&about, width, muted));
        return lines;
    }
    let mut pieces = wrap(&layer.title, room);
    if pieces.is_empty() {
        pieces.push(String::new());
    }
    let mut lines: Vec<Line<'static>> = pieces
        .iter()
        .enumerate()
        .map(|(at, piece)| {
            let lead = match at {
                0 => Span::styled(mark, Style::new().fg(color)),
                _ => Span::raw(" "),
            };
            let left = vec![lead, Span::raw(" "), Span::styled(clip(piece, room), title)];
            let right = match at {
                0 => vec![Span::styled(check, Style::new().fg(theme.primary))],
                _ => Vec::new(),
            };
            row(left, right, width)
        })
        .collect();
    let room = width - 2;
    lines.extend(wrap(&about, room).iter().map(|piece| {
        let left = vec![Span::raw("  "), Span::styled(clip(piece, room), muted)];
        row(left, Vec::new(), width)
    }));
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
        assert!(!pulls.click(0, 0));
        pulls.page(40, 20, now(), true, &theme);
        // The second link's rows are the page's fourth to sixth.
        assert!(pulls.click(0, 4));
        assert_eq!((pulls.selected, pulls.viewing), (1, false));
        pulls.page(40, 20, now(), true, &theme);
        // The blank rows and the footer take no click.
        for offset in [6, 7, 18, 19, 20, 200] {
            assert!(!pulls.click(0, offset), "{offset}");
        }
        // A second click on the link shows its details.
        assert!(pulls.click(0, 3));
        assert!(pulls.viewing);
        let rows = rows_of(&pulls.page(40, 20, now(), true, &theme).lines);
        assert_eq!(rows[1], "#2 Change 2");
        // Only the row above them takes a click, and it leads back to the list with the link
        // still selected.
        for offset in 1..30 {
            assert!(!pulls.click(0, offset), "{offset}");
        }
        assert!(pulls.viewing);
        assert!(pulls.click(0, 0));
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
            assert!(!pulls.click(0, offset), "{offset}");
        }
        assert_eq!((pulls.selected, pulls.viewing), (8, false));
        // After the frame, the click lands on the link that row shows now.
        let page = pulls.page(40, 10, now(), true, &theme);
        assert_eq!(page.rows[0], Some(Spot::Link(6)));
        assert!(pulls.click(0, 0));
        assert_eq!(pulls.selected, 6);
        // So with the details: until a frame draws them, no click shows the list again, and
        // after their scroll moves, none does until the next.
        pulls.page(40, 10, now(), true, &theme);
        assert!(pulls.click(0, 0));
        assert!(pulls.viewing);
        assert!(!pulls.click(0, 0));
        pulls.page(40, 10, now(), true, &theme);
        pulls.move_by(3);
        assert!(!pulls.click(0, 0));
        assert!(pulls.viewing);
        pulls.page(40, 10, now(), true, &theme);
        assert!(pulls.click(0, 0));
        assert!(!pulls.viewing);
        // A move that changes nothing keeps the page the reader sees.
        pulls.page(40, 10, now(), true, &theme);
        pulls.move_by(0);
        assert!(pulls.click(0, 3));
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
        assert!(pulls.answer(ask.ticket, Part::Detail(answer), now()));
        let activity = pull_request::Activity::read(&bare_activity()).unwrap();
        let activity = Part::Activity(Ok(Box::new(activity)));
        assert!(pulls.answer(ask.ticket, activity, now()));
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
        assert!(!pulls.answer(2, Part::Detail(Err("other".into())), now()));
        pulls.follow();
        assert!(!pulls.answer(1, Part::Detail(Err("late".into())), now()));
        assert!(pulls.read.is_none());
    }

    /// A stack as T3 sends it for link 31: a merged layer under it and a draft above it whose
    /// title is empty.
    fn stack_sent() -> Value {
        json!({"id": "S_1", "number": 7, "url": "https://github.com/krishhgg/t3term/stacks/7",
        "base": "master", "layers": [
            {"number": 30, "title": "Lay the base", "headBranch": "base", "state": "merged"},
            {"number": 31, "title": "Change 31", "isDraft": false, "headBranch": "feature",
                "state": "open"},
            {"number": 32, "title": "", "isDraft": true, "headBranch": "feature-32",
                "state": "open"}
        ]})
    }

    fn stack_part(stack: &Value) -> Part {
        let stack = pull_request::Stack::read(stack).unwrap();
        Part::Stack(Ok(Some(Box::new(stack))))
    }

    /// Link 31's details once its host answered, and `stack` came, if any.
    fn read_with_stack(stack: Option<Part>) -> PullRequests {
        let mut pulls = PullRequests::default();
        let links = vec![link("krishhgg/t3term", 31, "feature", "master")];
        pulls.set(list_of(&thread(links)));
        pulls.toggle();
        let ask = pulls.take_ask(true, now()).expect("a read");
        let detail = Part::Detail(Ok(read_detail(&bare_detail())));
        let activity = pull_request::Activity::read(&bare_activity()).unwrap();
        let activity = Part::Activity(Ok(Box::new(activity)));
        for part in [detail]
            .into_iter()
            .chain(stack)
            .chain([activity, Part::Done])
        {
            pulls.answer(ask.ticket, part, now());
        }
        pulls
    }

    /// The details' stack section at `width`, from its label to the blank row before
    /// Reviewers.
    fn stack_rows(pulls: &mut PullRequests, width: usize) -> Vec<String> {
        pulls.page(width, 500, now(), true, &theme());
        let rows = detail_rows(pulls);
        let Some(from) = rows.iter().position(|row| row.starts_with("Stack #")) else {
            return Vec::new();
        };
        let to = rows[from..].iter().position(String::is_empty).unwrap() + from;
        rows[from..to].to_vec()
    }

    #[test]
    fn a_stack_lists_its_layers_from_the_top_and_checks_the_one_shown() {
        let mut pulls = read_with_stack(Some(stack_part(&stack_sent())));
        let rows = stack_rows(&mut pulls, 44);
        let check = format!("● Change 31{}✓", " ".repeat(32));
        assert_eq!(
            rows,
            [
                "Stack #7 · layer 2 of 3",
                // A layer without a title shows its branch, as the desktop's does.
                "◌ feature-32",
                "  #32 · feature-32 · Draft",
                check.as_str(),
                "  #31 · feature · Open",
                "● Lay the base",
                "  #30 · base · Merged",
                "↳ master",
            ]
        );
        // The section comes before the reviewers, as the desktop's header comes before its
        // tabs, and after the time of the read.
        let all = detail_rows(&pulls);
        let label = all.iter().position(|row| row == "Stack #7 · layer 2 of 3");
        let read = all
            .iter()
            .position(|row| row.starts_with("Read from github.com"));
        assert!(read < label, "{all:#?}");

        // Each mark is its state's color, and the pull request shown is bold and checked.
        let theme = theme();
        let lines = &pulls.detail.as_ref().unwrap().lines;
        let at = |text: &str| {
            let row = all.iter().position(|row| row.starts_with(text)).unwrap();
            &lines[row].spans
        };
        let color = |spans: &[Span], content: &str| {
            let span = spans.iter().find(|span| span.content == content).unwrap();
            (span.style.fg, span.style.add_modifier)
        };
        let plain = Modifier::empty();
        assert_eq!(color(at("◌ feature-32"), "◌"), (Some(theme.muted), plain));
        assert_eq!(color(at("● Lay"), "●"), (Some(theme.violet), plain));
        assert_eq!(color(at("● Change"), "●"), (Some(theme.success), plain));
        let bold = Modifier::BOLD;
        assert_eq!(color(at("● Change"), "Change 31"), (Some(theme.fg), bold));
        assert_eq!(color(at("● Change"), "✓"), (Some(theme.primary), plain));
        assert_eq!(color(at("● Lay"), "Lay the base"), (Some(theme.fg), plain));
        let about = at("  #30");
        assert_eq!(color(about, "#30 · base · Merged").0, Some(theme.muted));

        // Without a stack the section isn't there.
        let mut alone = read_with_stack(Some(Part::Stack(Ok(None))));
        assert_eq!(stack_rows(&mut alone, 44), Vec::<String>::new());
        let mut unasked = read_with_stack(None);
        assert_eq!(stack_rows(&mut unasked, 44), Vec::<String>::new());
    }

    #[test]
    fn a_stack_is_cleaned_and_fits_any_width() {
        let mut sent = stack_sent();
        sent["number"] = json!(70);
        sent["base"] = json!("ma\u{1b}[31min");
        sent["layers"][0]["title"] = json!("Lay\u{7}  the\nbase");
        sent["layers"][2]["title"] = json!("   ");
        sent["layers"][2]["headBranch"] = json!("x".repeat(2000));
        // A stack that doesn't name the pull request shown has no place for it.
        sent["layers"][1]["number"] = json!(41);
        let view = StackView::of(&pull_request::Stack::read(&sent).unwrap());
        assert_eq!(view.base, "ma[31min");
        assert_eq!(view.layers[0].title, "Lay  the base");
        assert_eq!(view.layers[2].title, view.layers[2].branch);
        let cut = format!("{}…", "x".repeat(NAME));
        assert_eq!(view.layers[2].branch, cut);
        let mut pulls = read_with_stack(Some(stack_part(&sent)));
        assert_eq!(stack_rows(&mut pulls, 44)[0], "Stack #70");
        assert!(
            !stack_rows(&mut pulls, 44)
                .iter()
                .any(|row| row.contains('✓'))
        );

        let mut pulls = read_with_stack(Some(stack_part(&stack_sent())));
        for width in 0..=70 {
            pulls.page(width, 500, now(), true, &theme());
            let lines = &pulls.detail.as_ref().unwrap().lines;
            for line in lines {
                assert!(line.width() <= width, "{width}: {line:?}");
            }
            // The label is whole from here, and the check shows.
            if width >= 23 {
                let rows = stack_rows(&mut pulls, width);
                assert!(rows.iter().any(|row| row.ends_with('✓')), "{width}");
            }
        }
        let rows = stack_rows(&mut pulls, 23);
        assert_eq!(
            rows,
            [
                "Stack #7 · layer 2 of 3",
                "◌ feature-32",
                // A row that doesn't fit wraps under the title, not under the mark.
                "  #32 · feature-32 ·",
                "  Draft",
                "● Change 31           ✓",
                "  #31 · feature · Open",
                "● Lay the base",
                "  #30 · base · Merged",
                "↳ master",
            ]
        );
    }

    #[test]
    fn a_stack_read_again_replaces_the_one_shown_and_a_failure_keeps_it() {
        let mut pulls = read_with_stack(Some(stack_part(&stack_sent())));
        let full = stack_rows(&mut pulls, 44);
        let read_again = |pulls: &mut PullRequests, stack: Option<Part>| {
            assert!(pulls.refresh());
            let ask = pulls.take_ask(true, now()).expect("a read again");
            assert_eq!(ask.updated, None);
            let detail = Part::Detail(Ok(read_detail(&bare_detail())));
            let activity = pull_request::Activity::read(&bare_activity()).unwrap();
            let activity = Part::Activity(Ok(Box::new(activity)));
            for part in [detail].into_iter().chain(stack).chain([activity]) {
                // A read again holds its answers until it is done.
                assert!(!pulls.answer(ask.ticket, part, now()));
            }
            assert!(pulls.answer(ask.ticket, Part::Done, now()));
        };

        // A stack that couldn't be read again stays, with a note.
        read_again(&mut pulls, Some(Part::Stack(Err("timed out".into()))));
        let rows = stack_rows(&mut pulls, 44);
        assert_eq!(rows[0], full[0]);
        assert_eq!(
            rows[1..3],
            [
                "May be out of date: t3term couldn't read the",
                "stack again."
            ]
        );
        assert_eq!(rows[3..], full[1..]);
        // A failure isn't reused: opening the details again reads again.
        pulls.back();
        pulls.toggle();
        let ask = pulls.take_ask(true, now()).expect("a read again");
        assert_eq!(ask.updated, None);
        let detail = Part::Detail(Ok(read_detail(&bare_detail())));
        let activity = pull_request::Activity::read(&bare_activity()).unwrap();
        let activity = Part::Activity(Ok(Box::new(activity)));
        let mut moved = stack_sent();
        moved["layers"][1]["state"] = json!("merged");
        for part in [detail, stack_part(&moved), activity, Part::Done] {
            pulls.answer(ask.ticket, part, now());
        }
        let rows = stack_rows(&mut pulls, 44);
        assert_eq!(rows.len(), full.len());
        assert_eq!(rows[4], "  #31 · feature · Merged");

        // A read that keeps the details current reads no stack and keeps the one shown.
        let later = now() + LIVE;
        assert!(pulls.live(later, later));
        let ask = pulls.take_ask(true, later).expect("a live read");
        assert!(ask.updated.is_some());
        let detail = Part::Detail(Ok(read_detail(&bare_detail())));
        pulls.answer(ask.ticket, detail, later);
        pulls.answer(ask.ticket, Part::Done, later);
        assert_eq!(stack_rows(&mut pulls, 44)[4], "  #31 · feature · Merged");

        // A read again that finds the pull request in no stack takes the stack away.
        read_again(&mut pulls, Some(Part::Stack(Ok(None))));
        assert_eq!(stack_rows(&mut pulls, 44), Vec::<String>::new());
    }

    /// The stack T3 saved on link 31 when it last synced it, which holds no titles.
    fn stack_saved() -> Value {
        json!({"kind": "native", "id": "S_1", "number": 7,
        "url": "https://github.com/krishhgg/t3term/stacks/7", "base": "master", "layers": [
            {"number": 30, "headBranch": "base", "state": "merged"},
            {"number": 31, "headBranch": "feature", "state": "open"},
            {"number": 32, "headBranch": "feature-32", "state": "open"}
        ]})
    }

    #[test]
    fn a_link_keeps_the_stack_saved_on_it_with_its_layers_titled_by_their_links() {
        let mut stacked = Vec::new();
        for (number, head) in [(30, "base"), (31, "feature"), (32, "feature-32")] {
            let mut one = link("krishhgg/t3term", number, head, "master");
            one["stack"] = stack_saved();
            stacked.push(one);
        }
        // The host's state for a layer wins over the link's, and a link's draft flag counts
        // only on an open layer.
        stacked[0]["snapshot"]["isDraft"] = json!(true);
        stacked[2]["snapshot"]["isDraft"] = json!(true);
        stacked[2]["snapshot"]["title"] = json!("Draw\u{7} it");
        // A link to the same number in another repository titles nothing.
        let mut other = link("someone/else", 33, "x", "y");
        other["snapshot"]["title"] = json!("Elsewhere");
        let mut saved = stack_saved();
        saved["layers"][2]["number"] = json!(33);
        saved["layers"]
            .as_array_mut()
            .unwrap()
            .push(json!({"number": 34, "headBranch": "top", "state": "closed"}));
        let mut top = link("krishhgg/t3term", 33, "feature-33", "feature-32");
        top["stack"] = saved;
        let list = list_of(&thread(stacked.into_iter().chain([other, top]).collect()));
        let saved = |number: u64| {
            let entry = entries(&list)
                .iter()
                .find(|entry| entry.number == number && entry.repository == "krishhgg/t3term")
                .unwrap();
            entry.saved.as_deref().cloned()
        };
        let row = |number: u64, title: &str, branch: &str, state: State| LayerRow {
            number,
            title: title.to_string(),
            branch: branch.to_string(),
            state,
        };
        assert_eq!(
            saved(31),
            Some(StackView {
                number: 7,
                base: "master".into(),
                layers: vec![
                    row(30, "Change 30", "base", State::Merged),
                    row(31, "Change 31", "feature", State::Open),
                    row(32, "Draw it", "feature-32", State::Draft),
                ],
                cut: false,
            })
        );
        assert_eq!(saved(30), saved(31));
        // Link 33's saved stack holds it, and #34 isn't linked, so its branch stands in.
        let view = saved(33).unwrap();
        assert_eq!(
            view.layers[2],
            row(33, "Change 33", "feature-32", State::Open)
        );
        assert_eq!(view.layers[3], row(34, "top", "top", State::Closed));

        // A saved stack that doesn't hold the link, or lacks a field T3 requires, isn't kept,
        // and the links still stack by the stack's id.
        let mut alone = link("krishhgg/t3term", 40, "a", "master");
        alone["stack"] = stack_saved();
        let mut broken = link("krishhgg/t3term", 31, "feature", "master");
        broken["stack"] = stack_saved();
        broken["stack"]["base"] = json!(" ");
        let mut base = link("krishhgg/t3term", 30, "base", "master");
        base["stack"] = stack_saved();
        assert_eq!(entries(&list_of(&thread(vec![alone])))[0].saved, None);
        let list = list_of(&thread(vec![broken, base]));
        let entry = |number: u64| {
            entries(&list)
                .iter()
                .find(|entry| entry.number == number)
                .unwrap()
        };
        assert_eq!(entry(31).saved, None);
        assert!(entry(30).saved.is_some());
        assert_eq!(entry(31).depth, 1);
    }

    #[test]
    fn the_details_show_the_saved_stack_until_the_host_s_comes() {
        let mut one = link("krishhgg/t3term", 31, "feature", "master");
        one["stack"] = stack_saved();
        let mut stacks = bare_detail();
        stacks["capabilities"]["stacks"] = json!(true);
        let open = |detail: &Value| {
            let mut pulls = PullRequests::default();
            pulls.set(list_of(&thread(vec![one.clone()])));
            pulls.toggle();
            let ticket = pulls.take_ask(true, now()).expect("a read").ticket;
            pulls.answer(ticket, Part::Detail(Ok(read_detail(detail))), now());
            (pulls, ticket)
        };
        let check = format!("● Change 31{}✓", " ".repeat(32));
        let layers = [
            "● feature-32",
            "  #32 · feature-32 · Open",
            check.as_str(),
            "  #31 · feature · Open",
            "● base",
            "  #30 · base · Merged",
            "↳ master",
        ];

        // Once the detail came and before the stack did, the stack saved on the link shows.
        let (mut pulls, ticket) = open(&stacks);
        let rows = stack_rows(&mut pulls, 44);
        assert_eq!(rows[0], "Stack #7 · layer 2 of 3");
        assert_eq!(
            rows[1..3],
            ["Saved with the link. Reading the host's", "stack…"]
        );
        assert_eq!(rows[3..], layers);
        // The host's stack replaces it.
        pulls.answer(ticket, stack_part(&stack_sent()), now());
        let mut read = read_with_stack(Some(stack_part(&stack_sent())));
        assert_eq!(stack_rows(&mut pulls, 44), stack_rows(&mut read, 44));

        // A pull request the host finds in no stack shows none.
        let (mut pulls, ticket) = open(&stacks);
        pulls.answer(ticket, Part::Stack(Ok(None)), now());
        assert_eq!(stack_rows(&mut pulls, 44), Vec::<String>::new());
        // When the next read fails, the saved stack shows again, as out of date.
        let failed = |pulls: &mut PullRequests| {
            assert!(pulls.refresh());
            let ask = pulls.take_ask(true, now()).expect("a read again");
            let parts = [
                Part::Detail(Ok(read_detail(&stacks))),
                Part::Stack(Err("timed out".into())),
                Part::Done,
            ];
            for part in parts {
                pulls.answer(ask.ticket, part, now());
            }
        };
        pulls.answer(ticket, Part::Done, now());
        failed(&mut pulls);
        let rows = stack_rows(&mut pulls, 44);
        assert_eq!(
            rows[1..3],
            [
                "May be out of date: t3term couldn't read the",
                "stack again."
            ]
        );
        assert_eq!(rows[3..], layers);

        // So does a first read that failed.
        let (mut pulls, ticket) = open(&stacks);
        pulls.answer(ticket, Part::Stack(Err("timed out".into())), now());
        assert_eq!(stack_rows(&mut pulls, 44)[3..], layers);

        // A host without stacks shows none, saved or not.
        let (mut pulls, _) = open(&bare_detail());
        assert_eq!(stack_rows(&mut pulls, 44), Vec::<String>::new());
    }

    /// The desktop's stack query reads again when the details mount once it is stale, while
    /// its live refresh and focus read the detail alone.
    #[test]
    fn opening_the_details_again_reads_a_stack_that_came_a_minute_ago() {
        let mut pulls = read_with_stack(Some(stack_part(&stack_sent())));
        let reopen = |pulls: &mut PullRequests, at: i64| {
            pulls.back();
            pulls.toggle();
            pulls.take_ask(true, at)
        };
        let detail_only = |pulls: &mut PullRequests, ask: &Ask, at: i64| {
            let detail = Part::Detail(Ok(read_detail(&bare_detail())));
            pulls.answer(ask.ticket, detail, at);
            pulls.answer(ask.ticket, Part::Done, at);
        };
        // Within the minute, opening the details again reads the detail alone.
        let at = now() + SOONEST;
        let ask = reopen(&mut pulls, at).expect("a read on arrival");
        assert!(ask.updated.is_some() && !ask.stack, "{ask:?}");
        detail_only(&mut pulls, &ask, at);
        // So does coming back to t3term, as the stack comes to a minute old.
        let at = now() + FRESH - 1;
        assert!(pulls.arrive(at));
        let ask = pulls.take_ask(true, at).expect("a read on focus");
        assert!(ask.updated.is_some() && !ask.stack, "{ask:?}");
        detail_only(&mut pulls, &ask, at);
        // Opening them once it is a minute old reads the stack too, though the detail just
        // came. Opening them again while that read is under way asks for nothing more.
        let at = at + 1;
        let ask = reopen(&mut pulls, at).expect("a read with the stack");
        assert!(ask.updated.is_some() && ask.stack, "{ask:?}");
        assert_eq!(reopen(&mut pulls, at), None);
        let mut moved = stack_sent();
        moved["layers"][1]["state"] = json!("merged");
        let detail = Part::Detail(Ok(read_detail(&bare_detail())));
        for part in [detail, stack_part(&moved), Part::Done] {
            pulls.answer(ask.ticket, part, at);
        }
        assert_eq!(stack_rows(&mut pulls, 44)[4], "  #31 · feature · Merged");
        // Then the stack is fresh and the detail just came, so nothing is read.
        assert_eq!(reopen(&mut pulls, at + 1), None);
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
        assert!(pulls.answer(1, Part::Detail(Ok(read_detail(&bare_detail()))), now()));
        pulls.page(44, 60, now(), true, &theme());
        let rows = detail_rows(&pulls);
        assert!(
            rows.contains(&"Reading the conversation…".to_string()),
            "{rows:#?}"
        );
        assert_eq!(reopen(&mut pulls, now()), None);
        // A conversation that failed says so, and the next open asks again.
        assert!(pulls.answer(1, Part::Activity(Err("timed out".into())), now()));
        assert!(!pulls.answer(1, Part::Done, now()));
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
        // The detail that came shows while the read goes again, and the new answers show once
        // both came.
        pulls.page(44, 60, now(), true, &theme());
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"Refreshing from github.com…".to_string()));
        assert!(rows.contains(&"timed out".to_string()));
        let detail = Part::Detail(Ok(read_detail(&bare_detail())));
        assert!(!pulls.answer(2, detail, now()));
        let activity = pull_request::Activity::read(&bare_activity()).unwrap();
        assert!(!pulls.answer(2, Part::Activity(Ok(Box::new(activity))), now()));
        assert!(pulls.answer(2, Part::Done, now()));
        pulls.page(44, 60, now(), true, &theme());
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"Read from github.com just now".to_string()));
        assert!(!rows.contains(&"timed out".to_string()));
        // Both answers stand for a minute, and then show while the read goes again.
        assert_eq!(reopen(&mut pulls, now() + SOONEST - 1), None);
        assert_eq!(reopen(&mut pulls, now() + FRESH), Some(3));
        pulls.page(44, 60, now() + FRESH, true, &theme());
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"Refreshing from github.com…".to_string()));
        assert!(
            !rows.iter().any(|row| row.starts_with("Reading")),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_refresh_reads_again_while_the_details_show_the_read() {
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![link(
            "krishhgg/t3term",
            31,
            "a",
            "b",
        )])));
        let activity = || {
            let activity = pull_request::Activity::read(&bare_activity()).unwrap();
            Part::Activity(Ok(Box::new(activity)))
        };
        let ticket = |pulls: &mut PullRequests| pulls.take_ask(true, now()).map(|a| a.ticket);
        let rows = |pulls: &mut PullRequests| {
            pulls.page(44, 60, now(), true, &theme());
            detail_rows(pulls)
        };
        // Nothing is read yet, so there is nothing to refresh.
        assert!(!pulls.refresh());
        pulls.toggle();
        assert!(!pulls.refresh());

        // A refresh during the first read reads again once it is done.
        assert_eq!(ticket(&mut pulls), Some(1));
        assert!(pulls.refresh());
        assert_eq!(ticket(&mut pulls), None);
        assert!(pulls.answer(1, Part::Detail(Ok(read_detail(&bare_detail()))), now()));
        assert!(pulls.answer(1, activity(), now()));
        assert!(!pulls.answer(1, Part::Done, now()));
        assert_eq!(ticket(&mut pulls), Some(2));
        assert!(rows(&mut pulls).contains(&"Refreshing from github.com…".to_string()));

        // A refresh that misses an answer keeps the last answers, and the details show when
        // they were read. Opening them again reads again at once.
        assert!(!pulls.answer(2, Part::Detail(Err("timed out".into())), now()));
        assert!(!pulls.answer(2, activity(), now()));
        assert!(pulls.answer(2, Part::Done, now()));
        let shown = rows(&mut pulls);
        assert!(shown.contains(&"Read from github.com just now".to_string()));
        assert!(!shown.contains(&"timed out".to_string()), "{shown:#?}");
        assert_eq!(ticket(&mut pulls), None);
        pulls.back();
        pulls.toggle();
        assert_eq!(ticket(&mut pulls), Some(3));
        assert!(!pulls.answer(3, Part::Detail(Ok(read_detail(&bare_detail()))), now()));
        assert!(!pulls.answer(3, activity(), now()));
        assert!(pulls.answer(3, Part::Done, now()));
        pulls.back();
        pulls.toggle();
        assert_eq!(ticket(&mut pulls), None);

        // While the list shows, a refresh only marks the read, and opening the details again
        // reads again within the minute.
        pulls.back();
        assert!(!pulls.refresh());
        assert_eq!(ticket(&mut pulls), None);
        pulls.toggle();
        assert_eq!(ticket(&mut pulls), Some(4));
        assert!(rows(&mut pulls).contains(&"Refreshing from github.com…".to_string()));
        assert!(!pulls.answer(4, Part::Detail(Ok(read_detail(&bare_detail()))), now()));
        assert!(!pulls.answer(4, activity(), now()));
        assert!(pulls.answer(4, Part::Done, now()));

        // So does one while the tab doesn't show, as when the pane closes, and showing it
        // reads again.
        pulls.shown(false);
        assert!(!pulls.refresh());
        assert_eq!(ticket(&mut pulls), None);
        pulls.shown(true);
        let ask = pulls.take_ask(true, now() - 45_000).map(|ask| ask.ticket);
        assert_eq!(ask, Some(5));

        // The new answers stand for a minute from when the pull request came, though the host
        // took 45 seconds, so opening the details again reads only the detail.
        assert!(!pulls.answer(5, Part::Detail(Ok(read_detail(&bare_detail()))), now()));
        assert!(!pulls.answer(5, activity(), now()));
        assert!(pulls.answer(5, Part::Done, now()));
        pulls.back();
        pulls.toggle();
        let ask = pulls.take_ask(true, now() + 20_000);
        assert_eq!(
            ask.map(|ask| (ask.ticket, ask.updated.is_some())),
            Some((6, true))
        );
    }

    #[test]
    fn r_reads_a_link_from_its_host_again_once_and_only_when_none_is_under_way() {
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![
            link("krishhgg/t3term", 31, "a", "b"),
            link("krishhgg/t3term", 32, "c", "d"),
        ])));
        let detail = || Part::Detail(Ok(read_detail(&bare_detail())));
        let activity = || {
            let activity = pull_request::Activity::read(&bare_activity()).unwrap();
            Part::Activity(Ok(Box::new(activity)))
        };
        let answered = |pulls: &mut PullRequests, ticket| {
            pulls.answer(ticket, detail(), now());
            pulls.answer(ticket, activity(), now());
            pulls.answer(ticket, Part::Done, now());
        };
        let asked = |pulls: &mut PullRequests| {
            let ask = pulls.take_ask(true, now());
            ask.map(|ask| (ask.number, ask.ticket, ask.invalidate))
        };
        let rows = |pulls: &mut PullRequests| {
            pulls.page(44, 60, now(), true, &theme());
            detail_rows(pulls)
        };

        // The list has nothing to read again, and neither does a read under way.
        assert!(!pulls.reloads() && !pulls.reload(now()));
        pulls.toggle();
        assert_eq!(asked(&mut pulls), Some((31, 1, false)));
        assert!(!pulls.reloads() && !pulls.reload(now()));
        answered(&mut pulls, 1);

        // Once it is done, `r` reads again from the host, and the details show the last
        // answers until the new ones come.
        assert!(pulls.reloads() && pulls.reload(now()));
        assert_eq!(asked(&mut pulls), Some((31, 2, true)));
        assert!(!pulls.reloads() && !pulls.reload(now()));
        let shown = rows(&mut pulls);
        assert!(shown.contains(&"Refreshing from github.com…".to_string()));
        assert!(shown.contains(&"Reviewers".to_string()), "{shown:#?}");

        // The number its invalidate moved doesn't read again, and the next one does, once
        // the read is done.
        assert!(!pulls.refresh());
        assert!(pulls.refresh());
        answered(&mut pulls, 2);
        assert_eq!(asked(&mut pulls), Some((31, 3, false)));
        answered(&mut pulls, 3);

        // A read from the host that heard no number forgets it once done, so the next number
        // reads again.
        assert!(pulls.reload(now()));
        assert_eq!(asked(&mut pulls), Some((31, 4, true)));
        answered(&mut pulls, 4);
        assert!(pulls.refresh());
        assert_eq!(asked(&mut pulls), Some((31, 5, false)));
        answered(&mut pulls, 5);

        // With nothing to show meanwhile, as after a failure, it reads as a first read does.
        pulls.back();
        pulls.move_by(1);
        pulls.toggle();
        assert_eq!(asked(&mut pulls), Some((32, 6, false)));
        pulls.answer(
            6,
            Part::Detail(Err("Pull request #32 was not found.".into())),
            now(),
        );
        pulls.answer(6, Part::Activity(Err("Not found".into())), now());
        pulls.answer(6, Part::Done, now());
        assert!(pulls.reload(now()));
        assert!(rows(&mut pulls).contains(&"Reading from github.com…".to_string()));
        assert_eq!(asked(&mut pulls), Some((32, 7, true)));

        // Nor does a link whose read waits for another's.
        pulls.back();
        pulls.move_by(-1);
        pulls.toggle();
        assert_eq!(asked(&mut pulls), None);
        assert!(!pulls.reloads() && !pulls.reload(now()));
        pulls.answer(7, Part::Done, now());
        assert_eq!(asked(&mut pulls), Some((31, 8, false)));
    }

    #[test]
    fn the_details_read_their_detail_again_after_five_minutes_while_in_use() {
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![link(
            "krishhgg/t3term",
            31,
            "a",
            "b",
        )])));
        let detail = |updated: &str| {
            let mut detail = bare_detail();
            detail["updatedAt"] = json!(updated);
            Part::Detail(Ok(read_detail(&detail)))
        };
        let remark = |body: &str| {
            let mut activity = bare_activity();
            activity["comments"] = json!([{"id": body, "kind": "issue-comment",
                "author": {"login": "mona", "name": null}, "body": body,
                "createdAt": "2026-10-13T09:00:00Z", "url": null, "path": null,
                "reviewState": null}]);
            activity["commentCount"] = json!(1);
            let activity = pull_request::Activity::read(&activity).unwrap();
            Part::Activity(Ok(Box::new(activity)))
        };
        let asked = |pulls: &mut PullRequests, at: i64| {
            let ask = pulls.take_ask(true, at);
            ask.map(|ask| (ask.ticket, ask.updated))
        };
        let rows = |pulls: &mut PullRequests, at: i64| {
            pulls.page(44, 60, at, true, &theme());
            detail_rows(pulls)
        };
        let t0 = now();

        // Nothing is due until a read is done.
        assert_eq!(pulls.live_at(), None);
        pulls.toggle();
        assert_eq!(asked(&mut pulls, t0), Some((1, None)));
        assert_eq!(pulls.live_at(), None);
        pulls.answer(1, detail("2026-10-13T11:00:00Z"), t0);
        pulls.answer(1, remark("First."), t0);
        pulls.answer(1, Part::Done, t0);
        assert_eq!(pulls.live_at(), Some(t0 + LIVE));

        // Five minutes after it was asked for, with a key a minute ago, the detail is read
        // again with the `updatedAt` it showed. Waking from a sleep twenty minutes past that
        // key reads nothing.
        assert!(!pulls.live(t0 + LIVE - 1, t0 + LIVE - 60_000));
        assert!(!pulls.live(t0 + LIVE + 19 * 60_000, t0 + LIVE - 60_000));
        assert!(pulls.live(t0 + LIVE, t0 + LIVE - 60_000));
        let t1 = t0 + LIVE;
        let updated = Some("2026-10-13T11:00:00Z".to_string());
        assert_eq!(asked(&mut pulls, t1), Some((2, updated)));
        assert_eq!(pulls.live_at(), None);
        assert!(rows(&mut pulls, t1).contains(&"Refreshing from github.com…".to_string()));

        // A detail that stayed the same keeps the conversation, and the read stands.
        assert!(!pulls.answer(2, detail("2026-10-13T11:00:00Z"), t1));
        assert!(pulls.answer(2, Part::Done, t1));
        let shown = rows(&mut pulls, t1);
        assert!(shown.contains(&"Read from github.com just now".to_string()));
        assert!(shown.iter().any(|row| row.trim() == "First."), "{shown:#?}");
        pulls.back();
        pulls.toggle();
        assert_eq!(asked(&mut pulls, t1 + 1), None);

        // Left alone for six minutes by then, the details wait, and the next key reads them.
        let t2 = t1 + LIVE;
        assert_eq!(pulls.live_at(), Some(t2));
        assert!(!pulls.live(t2, t2 - IDLE));
        assert!(!pulls.live(t2 + 60_000, t2 - IDLE));
        assert!(pulls.live(t2 + 60_000, t2 + 60_000));
        assert_eq!(asked(&mut pulls, t2).map(|ask| ask.0), Some(3));

        // A detail that changed comes with the conversation the read asked for after it.
        assert!(!pulls.answer(3, detail("2026-10-13T12:00:00Z"), t2));
        assert!(!pulls.answer(3, remark("Second."), t2));
        assert!(pulls.answer(3, Part::Done, t2));
        let shown = rows(&mut pulls, t2);
        assert!(
            shown.iter().any(|row| row.trim() == "Second."),
            "{shown:#?}"
        );

        // A failed read waits five minutes more rather than ask again at once.
        let t3 = t2 + LIVE;
        assert!(pulls.live(t3, t3));
        assert_eq!(asked(&mut pulls, t3).map(|ask| ask.0), Some(4));
        assert!(!pulls.answer(4, Part::Detail(Err("timed out".into())), t3));
        assert!(pulls.answer(4, Part::Done, t3));
        assert!(!rows(&mut pulls, t3).contains(&"timed out".to_string()));
        assert_eq!(pulls.live_at(), Some(t3 + LIVE));
        assert!(!pulls.live(t3 + 1, t3 + 1));

        // A refresh number reads the conversation too, though a live read was due.
        let t4 = t3 + LIVE;
        assert!(pulls.live(t4, t4));
        assert!(pulls.refresh());
        assert_eq!(asked(&mut pulls, t4), Some((5, None)));
        pulls.answer(5, Part::Done, t4);

        // Nothing is due while the list shows or the tab doesn't.
        pulls.back();
        assert_eq!(pulls.live_at(), None);
        pulls.toggle();
        pulls.shown(false);
        assert_eq!(pulls.live_at(), None);
        pulls.shown(true);
        assert_eq!(pulls.live_at(), Some(t4 + LIVE));
    }

    #[test]
    fn a_read_that_keeps_details_current_reads_a_conversation_that_failed_again() {
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![link(
            "krishhgg/t3term",
            31,
            "a",
            "b",
        )])));
        let detail = |updated: &str| {
            let mut detail = bare_detail();
            detail["updatedAt"] = json!(updated);
            Part::Detail(Ok(read_detail(&detail)))
        };
        let remark = |body: &str| {
            let mut activity = bare_activity();
            activity["comments"] = json!([{"id": body, "kind": "issue-comment",
                "author": {"login": "mona", "name": null}, "body": body,
                "createdAt": "2026-10-13T09:00:00Z", "url": null, "path": null,
                "reviewState": null}]);
            activity["commentCount"] = json!(1);
            let activity = pull_request::Activity::read(&activity).unwrap();
            Part::Activity(Ok(Box::new(activity)))
        };
        let timed_out = || Part::Activity(Err("timed out".into()));
        let live = |pulls: &mut PullRequests, at: i64| {
            assert!(pulls.live(at, at));
            let ask = pulls.take_ask(true, at).expect("a read again");
            (ask.ticket, ask.updated)
        };
        let t0 = now();
        pulls.toggle();
        assert_eq!(pulls.take_ask(true, t0).map(|ask| ask.ticket), Some(1));
        pulls.answer(1, detail("2026-10-13T11:00:00Z"), t0);
        pulls.answer(1, timed_out(), t0);
        pulls.answer(1, Part::Done, t0);

        // A first read whose conversation failed reads it again with the detail.
        let t1 = t0 + LIVE;
        assert_eq!(live(&mut pulls, t1), (2, None));
        pulls.answer(2, detail("2026-10-13T11:00:00Z"), t1);
        pulls.answer(2, remark("First."), t1);
        pulls.answer(2, Part::Done, t1);

        // A detail that changed and a conversation that then failed keep the last conversation
        // beside the new detail.
        let t2 = t1 + LIVE;
        let shown = Some("2026-10-13T11:00:00Z".to_string());
        assert_eq!(live(&mut pulls, t2), (3, shown));
        pulls.answer(3, detail("2026-10-13T12:00:00Z"), t2);
        pulls.answer(3, timed_out(), t2);
        pulls.answer(3, Part::Done, t2);
        pulls.page(44, 60, t2, true, &theme());
        let rows = detail_rows(&pulls);
        assert!(rows.iter().any(|row| row.trim() == "First."), "{rows:#?}");

        // The next read reads both, though the detail shown is the newest, and once both came
        // the one after reads the detail alone again.
        let t3 = t2 + LIVE;
        assert_eq!(live(&mut pulls, t3), (4, None));
        pulls.answer(4, detail("2026-10-13T12:00:00Z"), t3);
        pulls.answer(4, remark("Second."), t3);
        pulls.answer(4, Part::Done, t3);
        let t4 = t3 + LIVE;
        let shown = Some("2026-10-13T12:00:00Z".to_string());
        assert_eq!(live(&mut pulls, t4), (5, shown));
    }

    #[test]
    fn coming_back_to_t3term_reads_the_details_again_unless_read_ten_seconds_ago() {
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![link(
            "krishhgg/t3term",
            31,
            "a",
            "b",
        )])));
        let t0 = now();
        assert!(!pulls.arrive(t0));
        pulls.toggle();
        assert_eq!(pulls.take_ask(true, t0).map(|ask| ask.ticket), Some(1));
        assert!(!pulls.arrive(t0 + LIVE));
        let mut detail = bare_detail();
        detail["updatedAt"] = json!("2026-10-13T11:00:00Z");
        pulls.answer(1, Part::Detail(Ok(read_detail(&detail))), t0);
        let activity = pull_request::Activity::read(&bare_activity()).unwrap();
        pulls.answer(1, Part::Activity(Ok(Box::new(activity))), t0);
        pulls.answer(1, Part::Done, t0);

        // Well before the five minutes, coming back reads the detail again, though not within
        // ten seconds of the last read.
        assert!(!pulls.arrive(t0 + SOONEST - 1));
        assert!(pulls.arrive(t0 + SOONEST));
        let ask = pulls.take_ask(true, t0 + SOONEST).expect("a read again");
        let updated = Some("2026-10-13T11:00:00Z".to_string());
        assert_eq!((ask.ticket, ask.updated), (2, updated));
        assert!(!pulls.arrive(t0 + LIVE));
        pulls.answer(2, Part::Detail(Err("timed out".into())), t0 + SOONEST);
        pulls.answer(2, Part::Done, t0 + SOONEST);
        assert_eq!(pulls.live_at(), Some(t0 + SOONEST + LIVE));

        // A read again that failed counts from when it was asked for, so arriving within ten
        // seconds of it asks nothing more, though the detail shown came before.
        assert!(!pulls.arrive(t0 + 2 * SOONEST - 1));
        assert!(pulls.arrive(t0 + 2 * SOONEST));

        // Not on the list.
        pulls.back();
        assert!(!pulls.arrive(t0 + LIVE));
    }

    #[test]
    fn details_whose_read_failed_read_it_again_after_five_minutes_while_in_use() {
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![link(
            "krishhgg/t3term",
            31,
            "a",
            "b",
        )])));
        let t0 = now();
        pulls.toggle();
        assert_eq!(pulls.take_ask(true, t0).map(|ask| ask.ticket), Some(1));
        pulls.answer(1, Part::Detail(Err("timed out".into())), t0);
        pulls.answer(1, Part::Activity(Err("timed out".into())), t0);
        pulls.answer(1, Part::Done, t0);
        assert_eq!(pulls.live_at(), Some(t0 + LIVE));

        // It reads both again as a first read does, since there is no detail to compare.
        let t1 = t0 + LIVE;
        assert!(pulls.live(t1, t1));
        let ask = pulls.take_ask(true, t1).expect("a read again");
        assert_eq!((ask.ticket, ask.invalidate, ask.updated), (2, false, None));
        pulls.page(44, 60, t1, true, &theme());
        assert!(detail_rows(&pulls).contains(&"Reading from github.com…".to_string()));
    }

    #[test]
    fn a_read_again_with_no_detail_to_show_says_it_reads() {
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![link(
            "krishhgg/t3term",
            31,
            "a",
            "b",
        )])));
        let activity = || {
            let activity = pull_request::Activity::read(&bare_activity()).unwrap();
            Part::Activity(Ok(Box::new(activity)))
        };
        let ticket = |pulls: &mut PullRequests| pulls.take_ask(true, now()).map(|a| a.ticket);
        let rows = |pulls: &mut PullRequests| {
            pulls.page(44, 60, now(), true, &theme());
            detail_rows(pulls)
        };
        let failure = "no credential".to_string();
        let reading = "Reading from github.com…".to_string();
        // The detail fails and the conversation comes, so the details say why.
        pulls.toggle();
        assert_eq!(ticket(&mut pulls), Some(1));
        assert!(pulls.answer(1, Part::Detail(Err(failure.clone())), now()));
        assert!(pulls.answer(1, activity(), now()));
        assert!(!pulls.answer(1, Part::Done, now()));
        assert!(rows(&mut pulls).contains(&failure));

        // Opening them again reads again, and with no detail to show over, they say so in
        // place of the old failure until both answers came.
        pulls.back();
        pulls.toggle();
        assert_eq!(ticket(&mut pulls), Some(2));
        let shown = rows(&mut pulls);
        assert!(shown.contains(&reading), "{shown:#?}");
        assert!(!shown.contains(&failure), "{shown:#?}");
        assert!(!pulls.answer(2, Part::Detail(Err(failure.clone())), now()));
        assert!(rows(&mut pulls).contains(&reading));
        assert!(!pulls.answer(2, activity(), now()));
        assert!(pulls.answer(2, Part::Done, now()));
        let shown = rows(&mut pulls);
        assert!(shown.contains(&failure), "{shown:#?}");
        assert!(!shown.contains(&reading), "{shown:#?}");

        // So does a refresh while the details show them.
        assert!(pulls.refresh());
        assert_eq!(ticket(&mut pulls), Some(3));
        let shown = rows(&mut pulls);
        assert!(shown.contains(&reading), "{shown:#?}");
        assert!(!shown.contains(&failure), "{shown:#?}");
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
        assert!(!pulls.answer(1, Part::Detail(Err("late".into())), now()));
        assert_eq!(pulls.take_ask(true, now()), None);
        assert!(!pulls.answer(1, Part::Done, now()));
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
        assert!(!pulls.answer(2, Part::Done, now()));
        assert_eq!(numbers(pulls.take_ask(true, now())), Some((40, 3)));
    }

    #[test]
    fn a_read_that_waits_goes_while_the_tab_doesnt_show() {
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![
            link("krishhgg/t3term", 31, "feature", "master"),
            link("krishhgg/t3term", 32, "b", "c"),
        ])));
        let numbers = |ask: Option<Ask>| ask.map(|ask| (ask.number, ask.ticket));
        pulls.toggle();
        assert_eq!(numbers(pulls.take_ask(true, now())), Some((31, 1)));
        pulls.back();
        pulls.move_by(1);
        pulls.toggle();
        assert_eq!(pulls.take_ask(true, now()), None);
        // The pane closes, or the Thread tab shows, while #32 waits, so it goes and #31's
        // answer asks for nothing.
        pulls.shown(false);
        assert!(pulls.read.is_none());
        assert!(!pulls.answer(1, Part::Detail(Err("late".into())), now()));
        assert!(!pulls.answer(1, Part::Done, now()));
        // Shown again, #32's details want it again.
        pulls.shown(true);
        assert_eq!(numbers(pulls.take_ask(true, now())), Some((32, 2)));
        // A read under way stays, and so does an answer.
        pulls.shown(false);
        assert!(pulls.answer(2, Part::Detail(Ok(read_detail(&bare_detail()))), now()));
        pulls.shown(false);
        pulls.shown(true);
        assert_eq!(pulls.take_ask(true, now()), None);
        assert!(matches!(
            pulls.read.as_ref().map(|read| &read.answer),
            Some(Answer::Read(_))
        ));
    }

    #[test]
    fn an_answer_stands_for_a_minute_from_when_it_came() {
        // The host took 45 seconds, and the details say when the answer came.
        let answered = || {
            let mut pulls = PullRequests::default();
            pulls.set(list_of(&thread(vec![link(
                "krishhgg/t3term",
                31,
                "a",
                "b",
            )])));
            pulls.toggle();
            let asked = now() - 45_000;
            assert_eq!(pulls.take_ask(true, asked).map(|ask| ask.ticket), Some(1));
            assert!(pulls.answer(1, Part::Detail(Ok(read_detail(&bare_detail()))), now()));
            let activity = pull_request::Activity::read(&bare_activity()).unwrap();
            assert!(pulls.answer(1, Part::Activity(Ok(Box::new(activity))), now()));
            assert!(!pulls.answer(1, Part::Done, now()));
            pulls.page(44, 30, now(), true, &theme());
            let rows = detail_rows(&pulls);
            assert!(rows.contains(&"Read from github.com just now".to_string()));
            pulls
        };
        // Which read opening the details again at `at` asks for, and whether it reads only the
        // detail.
        let reopen = |at: i64| {
            let mut pulls = answered();
            pulls.back();
            pulls.toggle();
            let ask = pulls.take_ask(true, at);
            ask.map(|ask| (ask.ticket, ask.updated.is_some()))
        };
        // Within ten seconds of the answer, nothing is read. Within the minute the answers
        // stand, and only the detail is read again. After it, both are.
        assert_eq!(reopen(now() + SOONEST - 1), None);
        assert_eq!(reopen(now() + SOONEST), Some((2, true)));
        assert_eq!(reopen(now() + FRESH - 1), Some((2, true)));
        assert_eq!(reopen(now() + FRESH), Some((2, false)));
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
        conversing_on(&bare_detail(), activity)
    }

    /// One link whose host answered with `detail` and `activity`, its details open.
    fn conversing_on(detail: &Value, activity: Value) -> PullRequests {
        let mut pulls = PullRequests::default();
        let pull = link("krishhgg/t3term", 31, "a", "b");
        pulls.set(list_of(&thread(vec![pull])));
        pulls.toggle();
        let ask = pulls.take_ask(true, now()).expect("a read");
        let detail = Part::Detail(Ok(read_detail(detail)));
        assert!(pulls.answer(ask.ticket, detail, now()));
        let activity = pull_request::Activity::read(&activity).unwrap();
        let activity = Part::Activity(Ok(Box::new(activity)));
        assert!(pulls.answer(ask.ticket, activity, now()));
        pulls
    }

    /// A conversation of `active` remarks by people, the newest last, and one by a bot. Each
    /// came between 11 and 12 hours before `NOW`.
    fn remarks(active: u32) -> Value {
        let remark = |id: String, login: &str, minute: u32, body: String| {
            json!({"id": id, "kind": "issue-comment", "author": {"login": login, "name": null},
                "body": body, "createdAt": format!("2026-10-13T00:{minute:02}:00Z"),
                "url": null, "path": null, "reviewState": null})
        };
        let mut comments: Vec<Value> = (0..active)
            .map(|n| remark(format!("c{n}"), "mona", n, format!("Remark {n}")))
            .collect();
        comments.push(remark("b".into(), "ci[bot]", 59, "Bot".into()));
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
        let page = pulls.page(40, 24, now(), true, &theme);
        for &at in &bots {
            assert_eq!(page.lines[at].width(), 40, "{:?}", page.lines[at]);
        }
        let (_, lit) = control_rows(&mut pulls, 24, false, Control::Bots);
        assert!(lit.is_empty());
        assert_eq!(pulls.action().as_deref(), Some("show bot comments"));

        // Enter opens the group, whose remark comes in under it.
        pulls.enter();
        assert!(pulls.viewing);
        assert_eq!(pulls.action().as_deref(), Some("hide bot comments"));
        let (open, lit) = control_rows(&mut pulls, 24, true, Control::Bots);
        assert_eq!((open, lit), (bots.clone(), bots.clone()));
        let rows = rows_of(&pulls.page(40, 24, now(), true, &theme).lines);
        assert_eq!(rows[bots[0]], "⌄ 1 bot comment");

        // ↑ chooses Show older above it. The older remarks come in where it was, and Show only
        // 10 recent takes its place under them.
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
        // Two remarks of a header and a row of text, each after a blank row.
        assert_eq!(recent, [older[0] + 6]);
        assert_eq!(lit, recent);
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
                    // The chosen control has a row in view whenever it is chosen, and all of
                    // them once chosen, where they fit.
                    let (view, targets) = pulls.in_view().expect("drawn");
                    let (rows, _) = targets.iter().find(|(_, c)| *c == pick).unwrap();
                    assert!(shows(rows, &view), "{pick:?} {rows:?} {view:?}");
                    if picks.last() != Some(&pick) {
                        picks.push(pick);
                        let whole = view.start <= rows.start && rows.end <= view.end;
                        assert!(whole, "{pick:?} {rows:?} {view:?}");
                    }
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
        assert!(pulls.click(0, bots[1]));
        assert_eq!(pulls.pick, Some(Control::Bots));
        assert_eq!(pulls.action().as_deref(), Some("hide bot comments"));
        // The rows moved, so until a frame draws them a click lands on nothing.
        assert!(!pulls.click(0, bots[0]));
        let (open, _) = control_rows(&mut pulls, 24, true, Control::Bots);
        assert_eq!(open, bots);
        assert!(pulls.click(0, bots[0]));
        assert_eq!(pulls.action().as_deref(), Some("show bot comments"));
        // A row between two controls takes no click.
        pulls.page(40, 24, now(), true, &theme);
        assert!(!pulls.click(0, bots[0] - 1));
        assert_eq!(pulls.action().as_deref(), Some("show bot comments"));
    }

    #[test]
    fn details_opened_again_in_the_same_minute_start_over() {
        let mut pulls = conversing(remarks(12));
        let theme = theme();
        pulls.page(40, 24, now(), true, &theme);
        pulls.act(Control::Bots);
        pulls.act(Control::Older);
        pulls.page(40, 24, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(rows.iter().any(|row| row == "⌄ 1 bot comment"), "{rows:?}");

        // Back to the list and in again, at the same width and minute, with the read standing.
        pulls.toggle();
        pulls.toggle();
        assert_eq!(pulls.take_ask(true, now()), None);
        pulls.page(40, 24, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(rows.iter().any(|row| row == "› 1 bot comment"), "{rows:?}");
        let remarks = rows
            .iter()
            .filter(|row| row.starts_with("  Remark"))
            .count();
        assert_eq!(remarks, 10);
        pulls.end();
        pulls.page(40, 24, now(), true, &theme);
        pulls.step(false);
        assert_eq!(pulls.action().as_deref(), Some("show bot comments"));
    }

    #[test]
    fn a_control_that_moved_out_of_view_as_it_acted_counts_as_none_chosen() {
        let mut pulls = conversing(remarks(25));
        let theme = theme();
        pulls.page(40, 12, now(), true, &theme);
        // ↓ chooses Show older two rows into the view.
        let targets = &pulls.detail.as_ref().unwrap().targets;
        let (rows, _) = targets.iter().find(|(_, c)| *c == Control::Older).unwrap();
        pulls.scroll = Offset::Top(rows.start - 2);
        pulls.page(40, 12, now(), true, &theme);
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Older));
        let (older, _) = control_rows(&mut pulls, 12, true, Control::Older);
        // Enter brings 10 older remarks in where it was, and it moves down out of view. It
        // stays chosen, as a button keeps the focus in a browser, and Enter acts on it again.
        pulls.enter();
        let (moved, lit) = control_rows(&mut pulls, 12, true, Control::Older);
        assert!(moved.is_empty() && lit.is_empty(), "{moved:?}");
        let rows = rows_of(&pulls.page(40, 12, now(), true, &theme).lines);
        assert_eq!(older, [3]);
        assert_eq!(rows[3], format!("mona{}11h ago", " ".repeat(29)));
        assert_eq!(pulls.action().as_deref(), Some("show 5 older"));
        // ↓ reads on from there, a row at a time, with none chosen.
        let scroll = pulls.scroll;
        pulls.step(true);
        assert_eq!(pulls.pick, None);
        assert_ne!(pulls.scroll, scroll);
    }

    #[test]
    fn a_links_details_have_a_summary_and_a_timeline_each_at_its_own_scroll() {
        let theme = theme();
        let head = |pulls: &mut PullRequests, width| {
            let page = pulls.page(width, 6, now(), true, &theme);
            (rows_of(&page.lines[..1]).remove(0), page.tabs.clone())
        };
        // Until the host's detail came, the details have no tabs and → does nothing.
        let mut pulls = PullRequests::default();
        pulls.set(list_of(&thread(vec![link(
            "krishhgg/t3term",
            31,
            "a",
            "b",
        )])));
        pulls.toggle();
        let ask = pulls.take_ask(true, now()).expect("a read");
        assert_eq!(pulls.tabs(), None);
        assert!(!pulls.forward());
        let (row, tabs) = head(&mut pulls, 40);
        assert_eq!(row, format!("← List{}1 of 1", " ".repeat(28)));
        assert!(tabs.is_empty());
        let detail = Part::Detail(Ok(read_detail(&bare_detail())));
        assert!(pulls.answer(ask.ticket, detail, now()));
        assert_eq!(pulls.tabs(), Some(Tab::Summary));
        let (row, tabs) = head(&mut pulls, 40);
        assert_eq!(row, "← List  Summary  Timeline  Code   1 of 1");
        let three = [
            (6..15, Tab::Summary),
            (15..25, Tab::Timeline),
            (25..31, Tab::Code),
        ];
        assert_eq!(tabs, three);
        let page = pulls.page(40, 6, now(), true, &theme);
        let underlined = |name: &str| {
            let span = page.lines[0].spans.iter().find(|span| span.content == name);
            span.unwrap()
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED)
        };
        assert!(underlined("Summary") && !underlined("Timeline") && !underlined("Code"));
        // The Timeline waits for the conversation.
        assert!(pulls.forward());
        assert_eq!(pulls.tabs(), Some(Tab::Timeline));
        head(&mut pulls, 40);
        let rows = detail_rows(&pulls);
        assert_eq!(rows[rows.len() - 2..], ["", "Reading the conversation…"]);
        let activity = pull_request::Activity::read(&remarks(3)).unwrap();
        let activity = Part::Activity(Ok(Box::new(activity)));
        assert!(pulls.answer(ask.ticket, activity, now()));
        head(&mut pulls, 40);
        let rows = detail_rows(&pulls);
        // The fixture's remarks came before it opened.
        let timeline = [
            "",
            "4 comments · 0 commits      Newest first",
            "",
            "● Pull request opened",
            "  4h ago",
            "",
            "› 4 comments",
            "  2 authors · 11h ago",
        ];
        assert_eq!(rows[rows.len() - 8..], timeline);

        // Each tab keeps its own scroll, and a switch chooses no control.
        assert!(pulls.back());
        head(&mut pulls, 40);
        pulls.move_by(3);
        assert!(pulls.forward());
        assert_eq!(pulls.scroll, Offset::Top(0));
        head(&mut pulls, 40);
        pulls.move_by(2);
        head(&mut pulls, 40);
        assert!(pulls.back());
        assert_eq!(pulls.scroll, Offset::Top(3));
        head(&mut pulls, 40);
        assert!(pulls.forward());
        assert_eq!(pulls.scroll, Offset::Top(2));
        // The Code comes last, and ← from it shows the Timeline where it was.
        assert!(pulls.forward());
        assert_eq!(
            (pulls.tabs(), pulls.scroll),
            (Some(Tab::Code), Offset::Top(0))
        );
        assert!(!pulls.forward());
        assert!(pulls.back());
        assert_eq!(
            (pulls.tabs(), pulls.scroll),
            (Some(Tab::Timeline), Offset::Top(2))
        );

        // A click on a tab's name shows it, and one on the rest of the row leads straight back
        // to the list.
        head(&mut pulls, 40);
        assert!(!pulls.click(20, 0), "the tab that shows");
        assert!(pulls.click(8, 0));
        assert_eq!(pulls.tabs(), Some(Tab::Summary));
        assert!(!pulls.click(20, 0), "before a frame drew the tab");
        head(&mut pulls, 40);
        assert!(pulls.click(20, 0));
        assert_eq!(pulls.tabs(), Some(Tab::Timeline));
        head(&mut pulls, 40);
        assert!(pulls.click(28, 0));
        assert_eq!(pulls.tabs(), Some(Tab::Code));
        head(&mut pulls, 40);
        assert!(pulls.click(36, 0));
        assert!(!pulls.viewing());

        // They open again on the Summary, from the top of each tab.
        pulls.toggle();
        assert_eq!(pulls.tabs(), Some(Tab::Summary));
        assert_eq!(
            (pulls.scroll, pulls.kept),
            (Offset::Top(0), [Offset::Top(0); 3])
        );
        assert!(pulls.forward());
        assert_eq!(pulls.scroll, Offset::Top(0));

        // ↓ chooses the Timeline's controls, and Enter does what they do.
        pulls.page(40, 24, now(), true, &theme);
        pulls.step(true);
        assert_eq!(pulls.action().as_deref(), Some("oldest first"));
        pulls.enter();
        assert_eq!(pulls.action().as_deref(), Some("newest first"));
        pulls.page(40, 24, now(), true, &theme);
        pulls.step(true);
        assert_eq!(pulls.action().as_deref(), Some("show comments"));
        pulls.enter();
        pulls.page(40, 24, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"⌄ 4 comments".to_string()), "{rows:?}");
        assert!(
            rows.contains(&"  ci[bot] commented".to_string()),
            "{rows:?}"
        );
        // ← shows the Summary, with no control chosen, then the list.
        assert!(pulls.back());
        assert_eq!((pulls.tabs(), pulls.pick), (Some(Tab::Summary), None));
        assert!(pulls.back());
        assert!(!pulls.viewing());
        assert!(!pulls.back());
    }

    #[test]
    fn a_read_again_forgets_the_open_runs_it_no_longer_has() {
        let mut pulls = conversing(remarks(3));
        let theme = theme();
        assert!(!pulls.answer(1, Part::Done, now()));
        // The Timeline's one run opens.
        assert!(pulls.forward());
        pulls.page(40, 24, now(), true, &theme);
        pulls.step(true);
        assert_eq!(pulls.action().as_deref(), Some("oldest first"));
        pulls.page(40, 24, now(), true, &theme);
        pulls.step(true);
        assert_eq!(pulls.action().as_deref(), Some("show comments"));
        pulls.enter();
        assert_ne!(pulls.shown, Shown::default());

        // A newer remark joins it, so it is another run, and the key of the one that went is
        // forgotten.
        assert!(pulls.refresh());
        assert_eq!(pulls.take_ask(true, now()).map(|ask| ask.ticket), Some(2));
        let mut activity = remarks(3);
        let newer = json!({"id": "d", "kind": "issue-comment",
            "author": {"login": "mona", "name": null}, "body": "Later",
            "createdAt": "2026-10-13T01:00:00Z", "url": null, "path": null,
            "reviewState": null});
        activity["comments"].as_array_mut().unwrap().push(newer);
        activity["commentCount"] = json!(5);
        let activity = pull_request::Activity::read(&activity).unwrap();
        let detail = Part::Detail(Ok(read_detail(&bare_detail())));
        assert!(!pulls.answer(2, detail, now()));
        assert!(!pulls.answer(2, Part::Activity(Ok(Box::new(activity))), now()));
        assert!(pulls.answer(2, Part::Done, now()));
        assert_eq!(pulls.shown, Shown::default());
    }

    #[test]
    fn the_row_above_a_links_details_drops_where_it_is_then_cuts_the_tab_names() {
        let theme = theme();
        let both = vec![(6..15, Tab::Summary), (15..25, Tab::Timeline)];
        let three = || {
            let mut tabs = both.clone();
            tabs.push((25..31, Tab::Code));
            tabs
        };
        let cut = vec![(6..15, Tab::Summary), (15..20, Tab::Timeline)];
        let short = vec![(6..9, Tab::Summary)];
        // A host that gives a diff has the Code tab last.
        let mut pulls = conversing(remarks(1));
        let cases = [
            (39, "← List  Summary  Timeline  Code  1 of 1", three()),
            (38, "← List  Summary  Timeline  Code", three()),
            (28, "← List  Summary  Timeline  …", {
                let mut tabs = both.clone();
                tabs.push((25..28, Tab::Code));
                tabs
            }),
            (27, "← List  Summary  Timeline", both.clone()),
            (20, "← List  Summary  Ti…", cut.clone()),
            (9, "← List  …", short.clone()),
            (8, "← List", vec![]),
        ];
        for (width, row, tabs) in cases {
            let page = pulls.page(width, 6, now(), true, &theme);
            assert_eq!(rows_of(&page.lines[..1]), [row], "{width}");
            assert_eq!(page.tabs, tabs, "{width}");
        }
        // One that gives none has the Timeline last.
        let mut detail = bare_detail();
        detail["capabilities"]["diff"] = json!(false);
        let mut pulls = conversing_on(&detail, remarks(1));
        let cases = [
            (33, "← List  Summary  Timeline  1 of 1", both.clone()),
            (32, "← List  Summary  Timeline", both),
            (20, "← List  Summary  Ti…", cut),
            (9, "← List  …", short),
            (8, "← List", vec![]),
        ];
        for (width, row, tabs) in cases {
            let page = pulls.page(width, 6, now(), true, &theme);
            assert_eq!(rows_of(&page.lines[..1]), [row], "{width}");
            assert_eq!(page.tabs, tabs, "{width}");
        }
    }

    /// A slice of a diff that adds `files` files, `f{from}.rs` on, of a line each, and whose
    /// next slice is `next`.
    fn added(from: usize, files: usize, next: Option<&str>) -> Box<pull_request::Diff> {
        let patch: String = (from..from + files)
            .map(|n| {
                format!(
                    "diff --git a/f{n}.rs b/f{n}.rs\nnew file mode 100644\n--- /dev/null\n\
                     +++ b/f{n}.rs\n@@ -0,0 +1 @@\n+line {n}\n"
                )
            })
            .collect();
        Box::new(pull_request::Diff {
            patch,
            truncated: false,
            next_cursor: next.map(str::to_string),
            omitted: Vec::new(),
            omitted_cut: false,
        })
    }

    /// The read of the diff of `bare_detail`'s pull request with `ticket`, for the slice at
    /// `cursor`.
    fn diff_ask(ticket: u64, cursor: Option<&str>) -> DiffAsk {
        DiffAsk {
            ticket,
            host: "github.com".into(),
            repository: "krishhgg/t3term".into(),
            number: 31,
            cursor: cursor.map(str::to_string),
            commit: None,
        }
    }

    /// `conversing`'s details on the Code tab, their read done.
    fn on_code() -> PullRequests {
        let mut pulls = conversing(remarks(1));
        assert!(!pulls.answer(1, Part::Done, now()));
        assert!(pulls.forward() && pulls.forward());
        assert_eq!(pulls.tabs(), Some(Tab::Code));
        pulls
    }

    #[test]
    fn the_code_reads_its_first_slice_once_it_shows_and_the_next_near_the_end() {
        let theme = theme();
        // Nothing asks for the diff while the Summary or the Timeline shows.
        let mut pulls = conversing(remarks(1));
        assert_eq!(pulls.take_diff_ask(true, now()), None);
        assert!(pulls.forward());
        assert_eq!(pulls.take_diff_ask(true, now()), None);
        assert!(pulls.forward());
        // The Code asks for the first slice once it shows, unless reads are off, and once.
        assert_eq!(pulls.take_diff_ask(false, now()), None);
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        assert_eq!(pulls.take_diff_ask(true, now()), None);
        pulls.page(40, 6, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert_eq!(rows[rows.len() - 2..], ["", "Loading pull request diff…"]);
        assert!(!pulls.code_due());

        // An answer to another read is dropped. The first slice shows its files, and the end
        // of them coming within two pages of the view reads the next.
        assert!(!pulls.answer_diff(3, Ok(added(0, 20, Some("c2"))), now()));
        assert!(pulls.answer_diff(2, Ok(added(0, 20, Some("c2"))), now()));
        pulls.page(40, 6, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert_eq!(rows[rows.len() - 2..], ["", "Loading more files…"]);
        assert!(rows.contains(&"20 files+".to_string()), "{rows:#?}");
        assert!(!pulls.code_due());
        assert_eq!(pulls.take_diff_ask(true, now()), None);
        // With 5 rows in view, the end within 10 rows of the view's top reads the next.
        let lines = rows.len();
        pulls.move_by(lines as isize - 11);
        pulls.page(40, 6, now(), true, &theme);
        assert!(!pulls.code_due());
        pulls.move_by(1);
        pulls.page(40, 6, now(), true, &theme);
        assert!(pulls.code_due());
        assert_eq!(
            pulls.take_diff_ask(true, now()),
            Some(diff_ask(3, Some("c2")))
        );
        assert!(!pulls.code_due());
        assert_eq!(pulls.take_diff_ask(true, now()), None);

        // The last slice adds its files to the others, and no more are read.
        assert!(pulls.answer_diff(3, Ok(added(20, 2, None)), now()));
        pulls.page(40, 6, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"22 files".to_string()), "{rows:#?}");
        assert!(rows[rows.len() - 1].contains("f21.rs"), "{rows:#?}");
        pulls.move_by(100);
        pulls.page(40, 6, now(), true, &theme);
        assert!(!pulls.code_due());
        assert_eq!(pulls.take_diff_ask(true, now()), None);

        // Leaving the Code and coming back within a minute reads nothing again.
        assert!(pulls.back() && pulls.forward());
        assert_eq!(pulls.take_diff_ask(true, now() + FRESH), None);
    }

    #[test]
    fn the_code_reads_its_diff_again_where_it_may_have_changed() {
        let theme = theme();
        let mut pulls = on_code();
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        assert!(pulls.answer_diff(2, Ok(added(0, 2, None)), now()));
        // The first file unfolds, and stays so through the reads again.
        pulls.page(40, 30, now(), true, &theme);
        pulls.step(true);
        assert_eq!(pulls.action().as_deref(), Some("show whitespace changes"));
        pulls.step(true);
        assert_eq!(pulls.action().as_deref(), Some("expand all files"));
        pulls.step(true);
        assert_eq!(pulls.action().as_deref(), Some("show split diff view"));
        pulls.step(true);
        assert_eq!(pulls.action().as_deref(), Some("disable line wrapping"));
        pulls.step(true);
        assert_eq!(pulls.action().as_deref(), Some("show file tree"));
        pulls.step(true);
        assert_eq!(pulls.action().as_deref(), Some("expand file"));
        pulls.enter();
        let unfolded = |pulls: &mut PullRequests| {
            pulls.page(40, 30, now(), true, &theme);
            let rows = detail_rows(pulls);
            rows.contains(&"⌄ A f0.rs +1 -0".to_string()) && rows.contains(&"1 + line 0".into())
        };
        assert!(unfolded(&mut pulls));
        // The file unfolded is laid out once, and not again when the minute changes.
        let builds = |pulls: &PullRequests| {
            let code = pulls.read.as_ref().and_then(|read| read.code.as_ref());
            code.map(|code| code.builds)
        };
        assert_eq!(builds(&pulls), Some(1));
        pulls.page(40, 30, now() + 60_000, true, &theme);
        assert_eq!(builds(&pulls), Some(1));
        let answer = |pulls: &mut PullRequests, ticket, detail: &Value, conversation| {
            let detail = Part::Detail(Ok(read_detail(detail)));
            assert!(!pulls.answer(ticket, detail, now()));
            if conversation {
                let activity = pull_request::Activity::read(&remarks(1)).unwrap();
                assert!(!pulls.answer(ticket, Part::Activity(Ok(Box::new(activity))), now()));
            }
            pulls.answer(ticket, Part::Done, now());
        };

        // A refresh number reads the diff again along with the detail. The files that come
        // replace the others, so the file chosen goes.
        assert!(pulls.refresh());
        assert_eq!(pulls.take_ask(true, now()).map(|ask| ask.ticket), Some(3));
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(4, None)));
        assert_eq!(pulls.pick, Some(Control::File(0)));
        assert!(pulls.answer_diff(4, Ok(added(5, 1, None)), now()));
        assert_eq!(pulls.pick, None);
        pulls.page(40, 30, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"› A f5.rs +1 -0".to_string()), "{rows:#?}");
        assert!(!rows.iter().any(|row| row.contains("f0.rs")), "{rows:#?}");
        // Nor does an answer to an earlier read, or a second to the same.
        assert!(!pulls.answer_diff(2, Ok(added(0, 2, None)), now()));
        assert!(!pulls.answer_diff(4, Ok(added(0, 2, None)), now()));
        // A detail that stayed the same reads nothing more.
        answer(&mut pulls, 3, &bare_detail(), true);
        assert_eq!(pulls.take_diff_ask(true, now()), None);

        // `r` reads it again once the read that sent `pullRequests.invalidate` is done, so
        // T3 forgot the diff it held.
        assert!(pulls.reload(now()));
        assert_eq!(pulls.take_diff_ask(true, now()), None);
        let ask = pulls.take_ask(true, now()).expect("the read again");
        assert!(ask.invalidate);
        assert_eq!(pulls.take_diff_ask(true, now()), None);
        let detail = Part::Detail(Ok(read_detail(&bare_detail())));
        assert!(!pulls.answer(ask.ticket, detail, now()));
        assert_eq!(pulls.take_diff_ask(true, now()), None);
        let activity = pull_request::Activity::read(&remarks(1)).unwrap();
        pulls.answer(ask.ticket, Part::Activity(Ok(Box::new(activity))), now());
        pulls.answer(ask.ticket, Part::Done, now());
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(6, None)));
        assert!(pulls.answer_diff(6, Ok(added(0, 2, None)), now()));
        assert!(unfolded(&mut pulls));

        // A read that keeps the details current reads it again where `updatedAt` moved.
        let at = now() + SOONEST;
        assert!(pulls.arrive(at));
        let ask = pulls.take_ask(true, at).expect("a live read");
        answer(&mut pulls, ask.ticket, &bare_detail(), false);
        assert_eq!(pulls.take_diff_ask(true, at), None);
        let at = at + SOONEST;
        assert!(pulls.arrive(at));
        let ask = pulls.take_ask(true, at).expect("a live read");
        let mut moved = bare_detail();
        moved["updatedAt"] = json!("2026-10-13T12:00:00Z");
        answer(&mut pulls, ask.ticket, &moved, true);
        let ticket = ask.ticket + 1;
        assert_eq!(pulls.take_diff_ask(true, at), Some(diff_ask(ticket, None)));
        assert!(pulls.answer_diff(ticket, Ok(added(0, 2, None)), at));

        // Opening the details again reads the diff again once the Code shows, where it came a
        // minute or more before, as the desktop's query reads a stale diff when it mounts.
        let reopen = |pulls: &mut PullRequests, at: i64| {
            pulls.back();
            pulls.back();
            pulls.back();
            pulls.toggle();
            let ask = pulls.take_ask(true, at).expect("a read on arrival");
            answer(pulls, ask.ticket, &moved, false);
            assert_eq!(pulls.take_diff_ask(true, at), None);
            assert!(pulls.forward() && pulls.forward());
            pulls.take_diff_ask(true, at)
        };
        assert_eq!(reopen(&mut pulls, at + FRESH - 1), None);
        let again = reopen(&mut pulls, at + FRESH);
        assert_eq!(again, Some(diff_ask(pulls.tickets, None)));
    }

    #[test]
    fn enter_or_a_click_on_the_row_above_the_files_unfolds_or_folds_them_all() {
        let theme = theme();
        let mut pulls = on_code();
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        assert!(pulls.answer_diff(2, Ok(added(0, 2, None)), now()));
        let lines = |pulls: &mut PullRequests| {
            pulls.page(40, 30, now(), true, &theme);
            let rows = detail_rows(pulls);
            let shown = |text: &str| rows.contains(&text.to_string());
            [shown("1 + line 0"), shown("1 + line 1")]
        };
        assert_eq!(lines(&mut pulls), [false, false]);
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Whitespace));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Folds));
        assert_eq!(pulls.action().as_deref(), Some("expand all files"));
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::Folds));
        assert_eq!(pulls.action().as_deref(), Some("collapse all files"));
        assert_eq!(lines(&mut pulls), [true, true]);
        assert_eq!(pulls.take_diff_ask(true, now()), None, "nothing to read");

        // A click on the row does the same.
        let page = pulls.page(40, 30, now(), true, &theme);
        let spot = Some(Spot::Control(Control::Folds));
        let row = page.rows.iter().position(|row| *row == spot).unwrap();
        assert!(pulls.click(4, row));
        assert_eq!(lines(&mut pulls), [false, false]);
        assert!(pulls.click(4, row));
        assert_eq!(lines(&mut pulls), [true, true]);
    }

    #[test]
    fn enter_or_a_click_on_the_whitespace_row_shows_or_hides_those_changes() {
        let theme = theme();
        let mut pulls = on_code();
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        let patch = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1 +1 @@\n-a  b\n+a b\n";
        let diff = Box::new(pull_request::Diff {
            patch: patch.into(),
            truncated: false,
            next_cursor: None,
            omitted: Vec::new(),
            omitted_cut: false,
        });
        assert!(pulls.answer_diff(2, Ok(diff), now()));
        let lines = |pulls: &mut PullRequests| {
            pulls.page(40, 30, now(), true, &theme);
            let rows = detail_rows(pulls);
            let line = |row: &&String| row.starts_with("1 ") && row.ends_with('b');
            rows.iter().filter(line).cloned().collect::<Vec<_>>()
        };
        pulls.page(40, 30, now(), true, &theme);
        pulls.step(true);
        pulls.step(true);
        pulls.enter();
        assert_eq!(lines(&mut pulls), ["1   a b"]);
        pulls.step(false);
        assert_eq!(pulls.pick, Some(Control::Whitespace));
        assert_eq!(pulls.action().as_deref(), Some("show whitespace changes"));
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::Whitespace));
        assert_eq!(pulls.action().as_deref(), Some("hide whitespace changes"));
        assert_eq!(lines(&mut pulls), ["1 - a  b", "1 + a b"]);
        assert_eq!(pulls.take_diff_ask(true, now()), None, "nothing to read");

        // A click on the row does the same.
        let page = pulls.page(40, 30, now(), true, &theme);
        let spot = Some(Spot::Control(Control::Whitespace));
        let row = page.rows.iter().position(|row| *row == spot).unwrap();
        assert!(pulls.click(4, row));
        assert_eq!(lines(&mut pulls), ["1   a b"]);
        assert!(pulls.click(4, row));
        assert_eq!(lines(&mut pulls), ["1 - a  b", "1 + a b"]);
    }

    #[test]
    fn lines_that_dont_wrap_scroll_sideways_and_keep_their_numbers() {
        let theme = theme();
        let mut activity = remarks(1);
        activity["commits"] = json!([{"oid": "0000001abc", "messageHeadline": "Commit 1",
            "committedDate": "2026-10-13T00:01:00Z"}]);
        let mut pulls = conversing(activity);
        assert!(!pulls.answer(1, Part::Done, now()));
        assert!(pulls.forward() && pulls.forward());
        // 60 letters, which no 20 columns of repeat.
        let long: String = "abcdefghijklmnopqrstuvwxyz".repeat(3)[..60].to_string();
        let diff = || {
            let patch = format!(
                "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1 +1 @@\n-a\n+{long}\n"
            );
            Box::new(pull_request::Diff {
                patch,
                truncated: false,
                next_cursor: None,
                omitted: Vec::new(),
                omitted_cut: false,
            })
        };
        let ask = pulls.take_diff_ask(true, now()).expect("the first slice");
        assert!(pulls.answer_diff(ask.ticket, Ok(diff()), now()));
        // The rows of the file's lines on a page 40 columns wide.
        let lines = |pulls: &mut PullRequests| {
            let page = pulls.page(40, 30, now(), true, &theme);
            let rows = rows_of(&page.lines);
            let header = rows.iter().position(|row| row.starts_with("⌄ M f.rs"));
            let header = header.expect("the file unfolded");
            rows[header + 1..]
                .iter()
                .take_while(|row| !row.starts_with('⋯'))
                .cloned()
                .collect::<Vec<_>>()
        };
        pulls.pick = Some(Control::Folds);
        pulls.enter();
        // While the lines wrap, nothing scrolls sideways.
        assert_eq!(
            lines(&mut pulls),
            [
                "1 - a",
                "1 + abcdefghijklmnopqrstuvwxyzabcdefghij",
                "    klmnopqrstuvwxyzabcdefgh"
            ]
        );
        assert!(!pulls.slides());
        pulls.sideways(true);
        pulls.wheel_sideways(true);
        assert_eq!(pulls.across, 0);

        // Once they don't, the text past the numbers and marks scrolls half the width at a
        // time, until the end of the widest line shows.
        pulls.pick = Some(Control::Wrap);
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::Wrap));
        assert_eq!(
            lines(&mut pulls),
            ["1 - a", "1 + abcdefghijklmnopqrstuvwxyzabcdefghij"]
        );
        assert!(pulls.slides());
        assert_eq!(pulls.take_diff_ask(true, now()), None, "nothing to read");
        pulls.sideways(true);
        assert_eq!(pulls.across, 20);
        assert_eq!(
            lines(&mut pulls),
            ["1 -", "1 + uvwxyzabcdefghijklmnopqrstuvwxyzabcd"]
        );
        pulls.sideways(true);
        assert_eq!(pulls.across, 24);
        assert_eq!(
            lines(&mut pulls),
            ["1 -", "1 + yzabcdefghijklmnopqrstuvwxyzabcdefgh"]
        );
        pulls.sideways(false);
        assert_eq!(pulls.across, 4);
        // The wheel turns 4 columns at a time.
        pulls.wheel_sideways(false);
        assert_eq!(pulls.across, 0);
        pulls.wheel_sideways(false);
        assert_eq!(pulls.across, 0);
        pulls.wheel_sideways(true);
        assert_eq!(pulls.across, 4);
        assert_eq!(
            lines(&mut pulls),
            ["1 -", "1 + efghijklmnopqrstuvwxyzabcdefghijklmn"]
        );
        // Where the last frame showed another link's details, their lines don't hold it back,
        // and the next frame of this link's does.
        pulls.detail.as_mut().expect("the details").index = 1;
        pulls.sideways(true);
        pulls.sideways(true);
        assert_eq!(pulls.across, 44);
        pulls.page(40, 30, now(), true, &theme);
        assert_eq!(pulls.across, 24);
        // On a page one column wide, a step is a column.
        pulls.page(1, 30, now(), true, &theme);
        pulls.sideways(false);
        assert_eq!(pulls.across, 23);
        pulls.page(40, 30, now(), true, &theme);
        pulls.wheel_sideways(false);
        pulls.wheel_sideways(false);
        pulls.wheel_sideways(false);
        pulls.wheel_sideways(false);
        pulls.wheel_sideways(false);
        assert_eq!(pulls.across, 3);
        pulls.sideways(false);
        pulls.wheel_sideways(true);
        assert_eq!(pulls.across, 4);

        // The other tabs neither scroll it nor lose it.
        assert!(pulls.back() && pulls.back());
        assert!(!pulls.slides());
        pulls.sideways(true);
        pulls.wheel_sideways(true);
        assert_eq!(pulls.across, 4);
        assert!(pulls.forward() && pulls.forward());
        assert_eq!(pulls.across, 4);

        // A wider page shows more of the line, and scrolls back as far as it must.
        pulls.page(60, 30, now(), true, &theme);
        assert_eq!(pulls.across, 4);
        pulls.page(70, 30, now(), true, &theme);
        assert_eq!(pulls.across, 0);
        assert!(!pulls.slides());
        // The line's row is 64 columns wide, so it scrolls on a page of 63 and not of 64.
        pulls.page(63, 30, now(), true, &theme);
        assert!(pulls.slides());
        pulls.page(64, 30, now(), true, &theme);
        assert!(!pulls.slides());
        pulls.wheel_sideways(true);
        pulls.page(40, 30, now(), true, &theme);
        assert_eq!(pulls.across, 0, "nothing was wider than the details");
        pulls.wheel_sideways(true);
        assert_eq!(pulls.across, 4);

        // Wrapping the lines again takes them back.
        pulls.pick = Some(Control::Wrap);
        pulls.enter();
        lines(&mut pulls);
        assert_eq!(pulls.across, 0);
        pulls.enter();
        lines(&mut pulls);
        pulls.wheel_sideways(true);
        assert_eq!(pulls.across, 4);

        // So does another diff picked from the commits, and the details shown again.
        pulls.pick = Some(Control::Commits);
        pulls.enter();
        pulls.page(40, 30, now(), true, &theme);
        pulls.pick = Some(Control::Commit(0));
        pulls.enter();
        assert_eq!(pulls.across, 0);
        let ask = pulls
            .take_diff_ask(true, now())
            .expect("the commit's slice");
        assert!(pulls.answer_diff(ask.ticket, Ok(diff()), now()));
        pulls.pick = Some(Control::Folds);
        pulls.enter();
        lines(&mut pulls);
        pulls.sideways(true);
        assert_eq!(pulls.across, 20);
        // The same commit opened from the Timeline keeps it, as it keeps the Code's place.
        assert!(pulls.back());
        pulls.pick = Some(Control::TimelineCommit(0));
        pulls.enter();
        assert_eq!(pulls.tabs(), Some(Tab::Code));
        assert_eq!(pulls.across, 20);
        // Opened while the Code shows the whole change, the commit's diff starts at the left.
        pulls.pick = Some(Control::Commits);
        pulls.enter();
        pulls.page(40, 30, now(), true, &theme);
        pulls.pick = Some(Control::AllCommits);
        pulls.enter();
        let ask = pulls
            .take_diff_ask(true, now())
            .expect("the whole change's slice");
        assert!(pulls.answer_diff(ask.ticket, Ok(diff()), now()));
        pulls.pick = Some(Control::Folds);
        pulls.enter();
        lines(&mut pulls);
        pulls.sideways(true);
        assert_eq!(pulls.across, 20);
        assert!(pulls.back());
        pulls.pick = Some(Control::TimelineCommit(0));
        pulls.enter();
        assert_eq!(pulls.tabs(), Some(Tab::Code));
        assert_eq!(pulls.across, 0);
        let ask = pulls
            .take_diff_ask(true, now())
            .expect("the commit's slice");
        assert!(pulls.answer_diff(ask.ticket, Ok(diff()), now()));
        pulls.pick = Some(Control::Folds);
        pulls.enter();
        lines(&mut pulls);
        pulls.sideways(true);
        assert_eq!(pulls.across, 20);
        pulls.toggle();
        pulls.toggle();
        assert_eq!(pulls.across, 0);
    }

    #[test]
    fn a_row_slid_sideways_keeps_its_lead_and_styles_and_cuts_wide_characters() {
        let red = Style::new().fg(Color::Red);
        let line = Line::from(vec![Span::raw("1 + "), Span::styled("ab日本cd", red)])
            .style(Style::new().bg(Color::Blue));
        // The lead takes columns 0 to 4, then a, b, 日 from 6 to 8, 本 from 8 to 10, c and d.
        let row = |across, width| slid(&line, 4, across, width).to_string();
        assert_eq!(row(0, 12), "1 + ab日本cd");
        assert_eq!(row(0, 40), "1 + ab日本cd");
        // One the right edge cuts doesn't show.
        assert_eq!(row(0, 9), "1 + ab日");
        assert_eq!(row(0, 3), "1 +");
        assert_eq!(row(2, 12), "1 + 日本cd");
        // One the left edge cuts shows as spaces where its right part was.
        assert_eq!(row(3, 12), "1 +  本cd");
        assert_eq!(row(4, 12), "1 + 本cd");
        assert_eq!(row(5, 12), "1 +  cd");
        assert_eq!(row(5, 5), "1 +  ");
        assert_eq!(row(8, 12), "1 + ");
        assert_eq!(row(30, 12), "1 + ");
        // Each span keeps its style, and the row its own.
        let slid = slid(&line, 4, 3, 12);
        assert_eq!(slid.style, Style::new().bg(Color::Blue));
        assert_eq!(slid.spans, [Span::raw("1 + "), Span::styled(" 本cd", red)]);
        // A span the edge hides whole leaves no empty span.
        assert_eq!(super::slid(&line, 4, 10, 12).spans, [Span::raw("1 + ")]);
    }

    #[test]
    fn lines_side_by_side_scroll_sideways_together() {
        let theme = theme();
        let mut pulls = conversing(remarks(1));
        assert!(!pulls.answer(1, Part::Done, now()));
        assert!(pulls.forward() && pulls.forward());
        // 50 letters removed and 60 added, which no 20 columns of repeat.
        let letters = "abcdefghijklmnopqrstuvwxyz".repeat(3);
        let (lower, upper) = (&letters[..50], letters[..60].to_uppercase());
        let patch = format!(
            "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1 +1 @@\n-{lower}\n+{upper}\n"
        );
        let diff = Box::new(pull_request::Diff {
            patch,
            truncated: false,
            next_cursor: None,
            omitted: Vec::new(),
            omitted_cut: false,
        });
        let ask = pulls.take_diff_ask(true, now()).expect("the first slice");
        assert!(pulls.answer_diff(ask.ticket, Ok(diff), now()));
        let lines = |pulls: &mut PullRequests| {
            let page = pulls.page(40, 30, now(), true, &theme);
            let rows = rows_of(&page.lines);
            let header = rows.iter().position(|row| row.starts_with("⌄ M f.rs"));
            let header = header.expect("the file unfolded");
            rows[header + 1..]
                .iter()
                .take_while(|row| !row.starts_with('⋯'))
                .cloned()
                .collect::<Vec<_>>()
        };
        pulls.pick = Some(Control::Folds);
        pulls.enter();
        pulls.pick = Some(Control::Layout);
        pulls.enter();
        pulls.pick = Some(Control::Wrap);
        pulls.enter();
        assert_eq!(
            lines(&mut pulls),
            ["1 - abcdefghijklmno│ 1 + ABCDEFGHIJKLMNO"]
        );
        assert!(pulls.slides());
        // Both sides' text scrolls by the same columns, as the desktop's sides scroll together,
        // and each keeps its number and mark.
        pulls.sideways(true);
        assert_eq!(pulls.across, 20);
        assert_eq!(
            lines(&mut pulls),
            ["1 - uvwxyzabcdefghi│ 1 + UVWXYZABCDEFGHI"]
        );
        // The text stops once the end of the side whose text runs on furthest shows, here the
        // right side's, so the left one's ends in blanks.
        pulls.sideways(true);
        pulls.sideways(true);
        assert_eq!(pulls.across, 45);
        assert_eq!(
            lines(&mut pulls),
            ["1 - tuvwx          │ 1 + TUVWXYZABCDEFGH"]
        );
        // One under the other, a row has 36 columns of text, so the text scrolls back as far as
        // it must.
        pulls.pick = Some(Control::Layout);
        pulls.enter();
        assert_eq!(
            lines(&mut pulls),
            [
                "1 - yzabcdefghijklmnopqrstuvwx",
                "1 + YZABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGH"
            ]
        );
        assert_eq!(pulls.across, 24);
    }

    #[test]
    fn a_row_side_by_side_slides_each_side_within_its_columns() {
        let (red, green) = (Style::new().fg(Color::Red), Style::new().fg(Color::Green));
        let muted = Style::new().fg(Color::Gray);
        // As `beside` lays out a row unwrapped: the left side's number, mark and text, the
        // padding to its 14 columns, the bar and space between, then the right side's.
        let row = |left: [&str; 3], right: &[&str]| {
            let mut spans = vec![
                Span::raw(left[0].to_string()),
                Span::raw(left[1].to_string()),
                Span::styled(left[2].to_string(), red),
            ];
            let used: usize = left.iter().map(|text| text.width()).sum();
            spans.push(Span::raw(" ".repeat(14_usize.saturating_sub(used))));
            spans.push(Span::styled("│ ", muted));
            let styles = [Style::new(), Style::new(), green];
            for (text, style) in right.iter().zip(styles) {
                spans.push(Span::styled(text.to_string(), style));
            }
            Line::from(spans).style(Style::new().bg(Color::Blue))
        };
        let both = row(["1 ", "- ", "ab日本cdefgh"], &["1 ", "+ ", "xyz日w"]);
        let beside = |line: &Line<'static>, across, width| {
            slid_beside(line, 4, across, width, 14).to_string()
        };
        assert_eq!(beside(&both, 0, 40), "1 - ab日本cdef│ 1 + xyz日w");
        // Each side's text scrolls by the same columns, its number and mark kept, and stops at
        // the end of its side.
        assert_eq!(beside(&both, 2, 40), "1 - 日本cdefgh│ 1 + z日w");
        // A wide character the left edge cuts shows as a space, and one the side's right edge
        // cuts doesn't show.
        assert_eq!(beside(&both, 3, 40), "1 -  本cdefgh │ 1 + 日w");
        assert_eq!(beside(&both, 4, 40), "1 - 本cdefgh  │ 1 +  w");
        assert_eq!(beside(&both, 1, 40), "1 - b日本cdefg│ 1 + yz日w");
        assert_eq!(beside(&both, 0, 22), "1 - ab日本cdef│ 1 + xy");
        assert_eq!(beside(&both, 30, 40), "1 -           │ 1 + ");
        // The bar keeps its style, each side its own, and the row its own.
        let slid = slid_beside(&both, 4, 2, 40, 14);
        assert_eq!(slid.style, Style::new().bg(Color::Blue));
        let style_of = |text: &str| {
            let span = slid.spans.iter().find(|span| span.content == text);
            span.map(|span| span.style)
        };
        assert_eq!(style_of("│ "), Some(muted));
        assert_eq!(style_of("日本cdefgh"), Some(red));
        assert_eq!(style_of("z日w"), Some(green));
        // A side with no line stays blank.
        let left = row(["", "", ""], &["1 ", "+ ", "xyz日w"]);
        assert_eq!(beside(&left, 1, 40), "              │ 1 + yz日w");
        let right = row(["1 ", "- ", "ab日本cdefgh"], &[]);
        assert_eq!(beside(&right, 1, 40), "1 - b日本cdefg│ ");
    }

    #[test]
    fn enter_or_a_click_unfolds_a_file_and_a_slice_that_failed_reads_again() {
        let theme = theme();
        let mut pulls = on_code();
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        // A first slice that failed says why, with nothing to choose, as the desktop does.
        let failed = "Couldn't read this diff: timed out";
        assert!(pulls.answer_diff(2, Err(failed.into()), now()));
        pulls.page(40, 30, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert_eq!(rows[rows.len() - 2..], ["", failed]);
        pulls.step(true);
        assert_eq!((pulls.pick, pulls.action()), (None, None));
        assert_eq!(pulls.take_diff_ask(true, now()), None);
        // `r` reads it again.
        assert!(pulls.reload(now()));
        let ask = pulls.take_ask(true, now()).expect("the read again");
        pulls.answer(ask.ticket, Part::Done, now());
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(4, None)));
        assert!(pulls.answer_diff(4, Ok(added(0, 2, Some("c2"))), now()));

        // ↓ chooses a file, and Enter unfolds and folds it.
        let page = pulls.page(40, 30, now(), true, &theme);
        let at = |page: &Page, control| {
            let spot = Some(Spot::Control(control));
            page.rows
                .iter()
                .position(|row| *row == spot)
                .expect("the row")
        };
        let second = at(page, Control::File(1));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Whitespace));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Folds));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Layout));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Wrap));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Tree));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::File(0)));
        assert_eq!(pulls.action().as_deref(), Some("expand file"));
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::File(0)));
        assert_eq!(pulls.action().as_deref(), Some("collapse file"));
        pulls.page(40, 30, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"1 + line 0".to_string()), "{rows:#?}");
        pulls.enter();
        pulls.page(40, 30, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(!rows.contains(&"1 + line 0".to_string()), "{rows:#?}");
        // A click on a file's row unfolds it too.
        assert!(pulls.click(4, second));
        let page = pulls.page(40, 30, now(), true, &theme);
        assert_eq!(rows_of(&page.lines[second + 1..second + 2]), ["1 + line 1"]);

        // A later slice that failed offers Retry, which reads it again.
        assert!(pulls.code_due());
        assert_eq!(
            pulls.take_diff_ask(true, now()),
            Some(diff_ask(5, Some("c2")))
        );
        assert!(pulls.answer_diff(5, Err("timed out".into()), now()));
        let page = pulls.page(40, 30, now(), true, &theme);
        let retry = at(page, Control::Retry);
        let rows = detail_rows(&pulls);
        let failure = [
            "",
            "t3term couldn't read the rest of this",
            "diff.",
            "timed out",
            "↻ Retry",
        ];
        assert_eq!(rows[rows.len() - 5..], failure);
        assert!(!pulls.code_due());
        assert_eq!(pulls.take_diff_ask(true, now()), None);
        // The click chose the second file, and ↓ from it chooses Retry.
        assert_eq!(pulls.pick, Some(Control::File(1)));
        pulls.step(true);
        assert_eq!(pulls.action().as_deref(), Some("retry"));
        pulls.enter();
        assert_eq!(pulls.pick, None);
        assert_eq!(
            pulls.take_diff_ask(true, now()),
            Some(diff_ask(6, Some("c2")))
        );
        assert!(pulls.answer_diff(6, Err("timed out".into()), now()));
        pulls.page(40, 30, now(), true, &theme);
        assert!(pulls.click(2, retry));
        assert_eq!(
            pulls.take_diff_ask(true, now()),
            Some(diff_ask(7, Some("c2")))
        );
        assert!(pulls.answer_diff(7, Ok(added(2, 1, None)), now()));
        pulls.page(40, 30, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"3 files".to_string()), "{rows:#?}");
        assert!(!rows.iter().any(|row| row.contains("Retry")), "{rows:#?}");
    }

    #[test]
    fn a_file_picked_from_the_tree_unfolds_and_shows_at_the_top() {
        let theme = theme();
        let mut pulls = on_code();
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        assert!(pulls.answer_diff(2, Ok(added(0, 30, None)), now()));
        pulls.page(40, 20, now(), true, &theme);
        for _ in 0..5 {
            pulls.step(true);
        }
        assert_eq!(pulls.pick, Some(Control::Tree));
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::Tree));
        assert_eq!(pulls.action().as_deref(), Some("hide file tree"));
        pulls.page(40, 20, now(), true, &theme);
        // ↓ goes on to the tree's files, each at the root.
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::TreeFile(0)));
        assert_eq!(pulls.action().as_deref(), Some("go to file"));

        // Enter unfolds the file and chooses its header, at the top of the details. The files
        // show in the order of their paths, so the 21st is f27.rs.
        pulls.pick = Some(Control::TreeFile(20));
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::File(20)));
        assert_eq!(pulls.action().as_deref(), Some("collapse file"));
        let page = pulls.page(40, 20, now(), true, &theme);
        assert_eq!(
            rows_of(&page.lines[1..3]),
            ["⌄ A f27.rs +1 -0", "1 + line 27"]
        );
        // The next frame keeps the scroll.
        let top = pulls.scroll;
        pulls.page(40, 20, now(), true, &theme);
        assert_eq!(pulls.scroll, top);

        // A click on a file's row does the same.
        pulls.home();
        let page = pulls.page(40, 20, now(), true, &theme);
        let spot = Some(Spot::Control(Control::TreeFile(3)));
        let row = page.rows.iter().position(|row| *row == spot).unwrap();
        assert!(pulls.click(4, row));
        assert_eq!(pulls.pick, Some(Control::File(3)));
        let page = pulls.page(40, 20, now(), true, &theme);
        assert_eq!(
            rows_of(&page.lines[1..3]),
            ["⌄ A f11.rs +1 -0", "1 + line 11"]
        );

        // A file near the end shows as high as the last page lets it.
        pulls.home();
        pulls.pick = Some(Control::TreeFile(29));
        pulls.enter();
        let page = pulls.page(40, 20, now(), true, &theme);
        let rows = rows_of(&page.lines);
        assert_eq!(rows[rows.len() - 2..], ["⌄ A f9.rs +1 -0", "1 + line 9"]);

        // A frame of another tab drops the reveal, so the Code shows again where it was.
        pulls.home();
        pulls.pick = Some(Control::TreeFile(10));
        pulls.enter();
        assert!(pulls.back());
        pulls.page(40, 20, now(), true, &theme);
        assert!(pulls.forward());
        pulls.page(40, 20, now(), true, &theme);
        assert_eq!(pulls.scroll, Offset::Top(0));

        // So do the details shown again before a frame, which start over from the top.
        pulls.pick = Some(Control::TreeFile(10));
        pulls.enter();
        pulls.toggle();
        pulls.toggle();
        assert!(pulls.forward());
        assert!(pulls.forward());
        pulls.page(40, 20, now(), true, &theme);
        assert_eq!(pulls.tabs(), Some(Tab::Code));
        assert_eq!(pulls.scroll, Offset::Top(0));
    }

    #[test]
    fn the_code_shows_review_threads_that_enter_opens_and_closes() {
        let theme = theme();
        let remark = |id: &str, body: &str| {
            json!({"id": id, "author": {"login": "mona", "name": null}, "body": body,
                "createdAt": "2026-10-13T00:30:00Z", "url": null})
        };
        let mut activity = remarks(1);
        activity["reviewThreads"] = json!([
            {"id": "t0", "path": "f0.rs", "line": 1, "side": "right", "isResolved": false,
                "isOutdated": true, "commentCount": 3,
                "comments": [remark("t0c", "Rename **this**.")]},
            {"id": "t1", "path": "f1.rs", "line": 1, "side": "right", "isResolved": false,
                "isOutdated": false, "comments": [remark("t1c", "Later file.")]},
        ]);
        let mut pulls = conversing(activity);
        assert!(!pulls.answer(1, Part::Done, now()));
        assert!(pulls.forward() && pulls.forward());
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        assert!(pulls.answer_diff(2, Ok(added(0, 1, Some("c2"))), now()));
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        let label = "› 1 conversation not on the diff read so far";
        assert!(rows.contains(&label.to_string()), "{rows:#?}");

        // The list opens, and its thread shows its remark, laid out for the frame.
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Whitespace));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Folds));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Layout));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Wrap));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Tree));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::OffDiff));
        assert_eq!(
            pulls.action().as_deref(),
            Some("show conversations off the diff")
        );
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::OffDiff));
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        let at = rows
            .iter()
            .position(|row| row == "f1.rs")
            .expect("the list");
        assert_eq!(rows[at + 1..at + 3], ["Line 1", "│ ⌄ Open · 1 comment"]);
        assert_eq!(rows[at + 4], "│   Later file.");
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Thread(1)));
        assert_eq!(pulls.action().as_deref(), Some("hide conversation"));
        pulls.enter();
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert_eq!(rows[at + 2..at + 4], ["│ › Open · 1 comment", ""]);

        // Unfolding the file shows the thread under its line, outdated, with the remarks T3
        // didn't send counted.
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::File(0)));
        pulls.enter();
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        let at = rows
            .iter()
            .position(|row| row == "1 + line 0")
            .expect("the line");
        assert_eq!(
            rows[at + 1..at + 5],
            [
                "│ ⌄ Open · 3 comments · outdated",
                "│ mona                                               11h ago",
                "│   Rename this.",
                "│ 2 more comments on the host.",
            ]
        );

        // A thread whose file folds is no longer one Enter can open or close.
        pulls.enter();
        let before = pulls.shown.clone();
        pulls.pick = Some(Control::Thread(0));
        pulls.enter();
        assert_eq!(pulls.shown, before);

        // The thread's file comes in the next slice, which the end in view asked for, so the
        // list goes.
        assert!(pulls.code_due());
        assert_eq!(
            pulls.take_diff_ask(true, now()),
            Some(diff_ask(3, Some("c2")))
        );
        assert!(pulls.answer_diff(3, Ok(added(1, 1, None)), now()));
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(
            !rows.iter().any(|row| row.contains("not on the")),
            "{rows:#?}"
        );
        assert!(rows.contains(&"› A f1.rs +1 -0".to_string()), "{rows:#?}");
    }

    #[test]
    fn the_code_tab_reads_the_commit_picked_from_its_list() {
        let theme = theme();
        let commit = |n: u32| {
            json!({"oid": format!("{n:07}abc"), "messageHeadline": format!("Commit {n}"),
                "committedDate": format!("2026-10-13T00:{n:02}:00Z")})
        };
        let mut activity = remarks(1);
        activity["commits"] = (0..21).map(commit).collect();
        activity["reviewThreads"] = json!([
            {"id": "t0", "path": "f0.rs", "line": 1, "side": "right", "isResolved": false,
                "isOutdated": false, "comments": [{"id": "r0",
                "author": {"login": "mona", "name": null}, "body": "On line 1.",
                "createdAt": "2026-10-13T00:30:00Z", "url": null}]},
        ]);
        let scoped = |ticket: u64, commit: Option<&str>| DiffAsk {
            commit: commit.map(str::to_string),
            ..diff_ask(ticket, None)
        };
        let mut pulls = conversing(activity.clone());
        assert!(!pulls.answer(1, Part::Done, now()));
        assert!(pulls.forward() && pulls.forward());
        assert_eq!(pulls.take_diff_ask(true, now()), Some(scoped(2, None)));
        assert!(pulls.answer_diff(2, Ok(added(0, 1, None)), now()));
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        let at = rows
            .iter()
            .position(|row| row == "› All commits")
            .expect("the row");
        assert_eq!(rows[at + 1], "1 file");

        // The row that picks the diff comes first, and opens the list.
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Commits));
        assert_eq!(pulls.action().as_deref(), Some("choose commit"));
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::Commits));
        assert_eq!(pulls.action().as_deref(), Some("hide commits"));
        pulls.page(60, 40, now(), true, &theme);
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::AllCommits));
        assert_eq!(pulls.action().as_deref(), Some("show all commits"));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Commit(0)));
        assert_eq!(pulls.action().as_deref(), Some("show commit 0000020"));

        // Show more stays chosen while more are left, and the first commit it listed takes
        // its place once none are.
        pulls.pick = Some(Control::MoreCommits);
        assert_eq!(pulls.action().as_deref(), Some("show more commits"));
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::MoreCommits));
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::Commit(20)));
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::Commits));
        assert_eq!(
            pulls.take_diff_ask(true, now()),
            Some(scoped(3, Some("0000000abc")))
        );
        // An entry of the list closed picks nothing.
        pulls.pick = Some(Control::Commit(1));
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::Commit(1)));
        assert_eq!(pulls.take_diff_ask(true, now()), None);
        pulls.pick = Some(Control::Commits);

        // A commit's diff places no thread, and picking another closes the list of those
        // off the diff.
        // The diff that comes keeps the row that picked it chosen, where it chooses nothing
        // on the files it replaced.
        assert!(pulls.answer_diff(3, Ok(added(0, 1, None)), now()));
        assert_eq!(pulls.pick, Some(Control::Commits));
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert_eq!(
            rows[at..at + 9],
            [
                "› Commit 0 · 0000000",
                "1 file",
                "¶ Show whitespace changes",
                "⇊ Expand all files",
                "◫ Split diff view",
                "↵ Disable line wrapping",
                "⊢ Show file tree",
                "",
                "› 1 conversation not on the current diff"
            ]
        );
        pulls.pick = Some(Control::OffDiff);
        pulls.enter();
        assert!(pulls.shown.off_diff_open());
        pulls.pick = Some(Control::Commits);
        pulls.enter();
        pulls.pick = Some(Control::Commit(0));
        pulls.enter();
        assert!(!pulls.shown.off_diff_open());
        assert_eq!(
            pulls.take_diff_ask(true, now()),
            Some(scoped(4, Some("0000020abc")))
        );
        pulls.pick = Some(Control::File(0));
        assert!(pulls.answer_diff(4, Ok(added(0, 1, None)), now()));
        assert_eq!(pulls.pick, None);
        pulls.pick = Some(Control::OffDiff);
        pulls.enter();
        assert!(pulls.shown.off_diff_open());

        // A read of the conversation that no longer lists the commit, as after a force-push,
        // takes the tab back to the whole change, the list of threads off it closed.
        activity["commits"] = (0..20).map(commit).collect();
        assert!(pulls.reload(now()));
        let ask = pulls.take_ask(true, now()).expect("the read again");
        let detail = Part::Detail(Ok(read_detail(&bare_detail())));
        pulls.answer(ask.ticket, detail, now());
        let again = pull_request::Activity::read(&activity).unwrap();
        pulls.answer(ask.ticket, Part::Activity(Ok(Box::new(again))), now());
        assert!(pulls.shown.off_diff_open());
        pulls.answer(ask.ticket, Part::Done, now());
        assert!(!pulls.shown.off_diff_open());
        let ask = pulls.take_diff_ask(true, now()).expect("the whole change");
        assert_eq!(ask.commit, None);
        assert_eq!(ask.cursor, None);
    }

    #[test]
    fn a_thread_reads_more_comments_a_page_at_a_time() {
        let theme = theme();
        let remark = |id: &str| {
            json!({"id": id, "author": {"login": "mona", "name": null},
                "body": format!("Remark {id}."), "createdAt": "2026-10-13T00:30:00Z",
                "url": null})
        };
        let mut activity = remarks(1);
        activity["reviewThreads"] = json!([
            {"id": "t0", "path": "f0.rs", "line": 1, "side": "right", "isResolved": false,
                "isOutdated": false, "commentCount": 5, "nextCommentsCursor": "p2",
                "comments": [remark("a")]},
        ]);
        let page = |ids: &[&str], next: Option<&str>| {
            let page = json!({"comments": ids.iter().map(|id| remark(id)).collect::<Vec<_>>(),
                "nextCursor": next});
            Box::new(pull_request::CommentPage::read(&page).unwrap())
        };
        let comments_ask = |ticket: u64, cursor: &str| CommentsAsk {
            ticket,
            host: "github.com".into(),
            repository: "krishhgg/t3term".into(),
            number: 31,
            thread: "t0".into(),
            cursor: cursor.into(),
        };
        let count = |rows: &[String], text: &str| rows.iter().filter(|row| *row == text).count();
        let mut pulls = conversing(activity.clone());
        assert!(!pulls.answer(1, Part::Done, now()));
        assert!(pulls.forward() && pulls.forward());
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        assert!(pulls.answer_diff(2, Ok(added(0, 1, None)), now()));
        pulls.page(60, 40, now(), true, &theme);
        for _ in 0..6 {
            pulls.step(true);
        }
        assert_eq!(pulls.pick, Some(Control::File(0)));
        pulls.enter();
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert_eq!(count(&rows, "│ Load more comments"), 1, "{rows:#?}");
        // Nothing reads until the reader asks, nor from a button that no longer shows, as
        // a click on a closed thread's last frame would.
        assert_eq!(pulls.take_comments_ask(true), None);
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Thread(0)));
        pulls.enter();
        pulls.pick = Some(Control::More(0));
        pulls.enter();
        assert_eq!(pulls.take_comments_ask(true), None);
        pulls.pick = Some(Control::Thread(0));
        pulls.enter();
        pulls.page(60, 40, now(), true, &theme);
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::More(0)));
        assert_eq!(pulls.action().as_deref(), Some("load more comments"));

        // Enter asks for the page at the thread's cursor, once, and the row says it is coming.
        pulls.enter();
        assert_eq!(pulls.pick, None);
        assert_eq!(pulls.take_comments_ask(false), None);
        assert_eq!(pulls.take_comments_ask(true), Some(comments_ask(3, "p2")));
        assert_eq!(pulls.take_comments_ask(true), None);
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert_eq!(count(&rows, "│ Loading more comments…"), 1, "{rows:#?}");
        assert_eq!(count(&rows, "│ Load more comments"), 0);
        // While it comes, Enter on where the button was asks for nothing more.
        pulls.pick = Some(Control::More(0));
        pulls.enter();
        assert_eq!(pulls.take_comments_ask(true), None);

        // An answer to another read changes nothing. A failure says why, and the button
        // reads the same page again.
        assert!(!pulls.answer_comments(99, Ok(page(&["b"], None))));
        let failure = Err("GitHub took too long to answer.".to_string());
        assert!(pulls.answer_comments(3, failure));
        assert!(!pulls.answer_comments(3, Ok(page(&["b"], None))));
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        let at = rows
            .iter()
            .position(|row| row == "│ t3term couldn't read more comments.")
            .expect("the failure");
        assert_eq!(
            rows[at + 1..at + 3],
            ["│ GitHub took too long to answer.", "│ Load more comments"]
        );
        pulls.pick = Some(Control::More(0));
        pulls.enter();
        assert_eq!(pulls.take_comments_ask(true), Some(comments_ask(4, "p2")));

        // The page's remarks follow the thread's, each once, and its cursor reads the next.
        assert!(pulls.answer_comments(4, Ok(page(&["a", "b", "c"], Some("p3")))));
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        for body in ["│   Remark a.", "│   Remark b.", "│   Remark c."] {
            assert_eq!(count(&rows, body), 1, "{body}: {rows:#?}");
        }
        assert_eq!(count(&rows, "│ Load more comments"), 1);

        // A read again of the conversation keeps the pages, and the next is the page's.
        assert!(pulls.reload(now()));
        let ask = pulls.take_ask(true, now()).expect("the read again");
        let detail = Part::Detail(Ok(read_detail(&bare_detail())));
        pulls.answer(ask.ticket, detail, now());
        let again = pull_request::Activity::read(&activity).unwrap();
        pulls.answer(ask.ticket, Part::Activity(Ok(Box::new(again))), now());
        pulls.answer(ask.ticket, Part::Done, now());
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert_eq!(count(&rows, "│   Remark c."), 1, "{rows:#?}");
        pulls.pick = Some(Control::More(0));
        pulls.enter();
        let ask = pulls.take_comments_ask(true).expect("the next page");
        assert_eq!(ask.cursor, "p3");

        // The last page takes the button away. The host counts one more than came, which
        // t3term can't read.
        assert!(pulls.answer_comments(ask.ticket, Ok(page(&["d"], None))));
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert_eq!(count(&rows, "│ Load more comments"), 0);
        assert_eq!(count(&rows, "│   Remark d."), 1);
        assert_eq!(
            count(&rows, "│ 1 more comment on the host."),
            1,
            "{rows:#?}"
        );
    }

    /// `bare_detail` with the reader's marks on files viewed kept by `store`.
    fn viewed_detail(store: &str) -> Value {
        let mut detail = bare_detail();
        detail["capabilities"]["viewedFiles"] = json!(store);
        detail
    }

    /// The read of the marks on files viewed of `bare_detail`'s pull request with `ticket`.
    fn viewed_ask(ticket: u64) -> ViewedAsk {
        ViewedAsk {
            ticket,
            host: "github.com".into(),
            repository: "krishhgg/t3term".into(),
            number: 31,
        }
    }

    /// Marks that say the files at `paths` are viewed.
    fn viewed(paths: &[&str]) -> Result<Box<pull_request::FilesViewed>, String> {
        let files = paths
            .iter()
            .map(|path| pull_request::FileViewed {
                path: path.to_string(),
                state: pull_request::ViewedState::Viewed,
            })
            .collect();
        Ok(Box::new(pull_request::FilesViewed {
            files,
            truncated: false,
            cut: false,
        }))
    }

    #[test]
    fn the_code_reads_the_viewed_marks_where_the_detail_says_who_keeps_them() {
        let theme = theme();
        // A host that keeps none reads none.
        let mut pulls = on_code();
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        assert_eq!(pulls.take_viewed_ask(true), None);

        // Nothing reads them while the Summary or the Timeline shows.
        let detail = viewed_detail("host");
        let mut pulls = conversing_on(&detail, remarks(1));
        assert!(!pulls.answer(1, Part::Done, now()));
        assert_eq!(pulls.take_viewed_ask(true), None);
        assert!(pulls.forward());
        assert_eq!(pulls.take_viewed_ask(true), None);
        assert!(pulls.forward());
        // The Code reads them once it shows, unless reads are off, and once.
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        assert_eq!(pulls.take_viewed_ask(false), None);
        assert_eq!(pulls.take_viewed_ask(true), Some(viewed_ask(3)));
        assert_eq!(pulls.take_viewed_ask(true), None);
        assert!(pulls.answer_diff(2, Ok(added(0, 3, None)), now()));
        pulls.page(40, 30, now(), true, &theme);
        assert!(detail_rows(&pulls).contains(&"3 files".into()));
        // An answer to another read is dropped, and the marks show once theirs came.
        assert!(!pulls.answer_viewed(9, viewed(&["f0.rs"]), now()));
        assert!(pulls.answer_viewed(3, viewed(&["f0.rs", "f2.rs"]), now()));
        assert!(!pulls.answer_viewed(3, viewed(&["f0.rs"]), now()));
        pulls.page(40, 30, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"3 files · 2 / 3 viewed".into()), "{rows:#?}");
        assert!(
            rows.contains(&"› A f0.rs +1 -0 · ✓ Viewed".into()),
            "{rows:#?}"
        );
        assert!(rows.contains(&"› A f1.rs +1 -0".into()), "{rows:#?}");

        let finish = |pulls: &mut PullRequests, ticket, detail: &Value| {
            let detail = Part::Detail(Ok(read_detail(detail)));
            assert!(!pulls.answer(ticket, detail, now()));
            let activity = pull_request::Activity::read(&remarks(1)).unwrap();
            pulls.answer(ticket, Part::Activity(Ok(Box::new(activity))), now());
            pulls.answer(ticket, Part::Done, now());
        };
        // A refresh number reads them again with the diff, once the Code shows.
        assert!(pulls.back());
        assert!(pulls.refresh());
        let ask = pulls.take_ask(true, now()).expect("the read again");
        let ticket = ask.ticket;
        assert_eq!(pulls.take_diff_ask(true, now()), None);
        assert_eq!(pulls.take_viewed_ask(true), None);
        assert!(pulls.forward());
        assert_eq!(
            pulls.take_diff_ask(true, now()),
            Some(diff_ask(ticket + 1, None))
        );
        assert_eq!(pulls.take_viewed_ask(true), Some(viewed_ask(ticket + 2)));
        finish(&mut pulls, ticket, &detail);
        assert!(pulls.answer_diff(ticket + 1, Ok(added(0, 3, None)), now()));
        assert!(pulls.answer_viewed(ticket + 2, viewed(&["f1.rs"]), now()));
        pulls.page(40, 30, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"3 files · 1 / 3 viewed".into()), "{rows:#?}");
        assert!(
            rows.contains(&"› A f1.rs +1 -0 · ✓ Viewed".into()),
            "{rows:#?}"
        );

        // `r` reads them again once the read that sent `pullRequests.invalidate` is done, so
        // T3 forgot the marks it held.
        assert!(pulls.reload(now()));
        assert_eq!(pulls.take_viewed_ask(true), None);
        let ask = pulls.take_ask(true, now()).expect("the read again");
        assert!(ask.invalidate);
        assert_eq!(pulls.take_viewed_ask(true), None);
        finish(&mut pulls, ask.ticket, &detail);
        let ticket = ask.ticket;
        assert_eq!(
            pulls.take_diff_ask(true, now()),
            Some(diff_ask(ticket + 1, None))
        );
        assert_eq!(pulls.take_viewed_ask(true), Some(viewed_ask(ticket + 2)));
        assert!(pulls.answer_diff(ticket + 1, Ok(added(0, 3, None)), now()));
        assert!(pulls.answer_viewed(ticket + 2, viewed(&[]), now()));

        // Opening the details again reads them again once the Code shows, where they came 15
        // seconds or more before, though the diff is still fresh.
        let reopen = |pulls: &mut PullRequests, at: i64| {
            pulls.back();
            pulls.back();
            pulls.back();
            pulls.toggle();
            // A read that keeps the details current finds the same detail.
            if let Some(ask) = pulls.take_ask(true, at) {
                finish(pulls, ask.ticket, &detail);
            }
            assert!(pulls.forward() && pulls.forward());
            assert_eq!(pulls.take_diff_ask(true, at), None);
            pulls.take_viewed_ask(true)
        };
        assert_eq!(reopen(&mut pulls, now() + 14_999), None);
        let again = reopen(&mut pulls, now() + 15_000);
        assert_eq!(again, Some(viewed_ask(pulls.tickets)));

        // A detail that no longer says anybody keeps them hides them and reads none.
        assert!(pulls.answer_viewed(pulls.tickets, viewed(&["f0.rs"]), now()));
        assert!(pulls.reload(now()));
        let ask = pulls.take_ask(true, now()).expect("the read again");
        finish(&mut pulls, ask.ticket, &bare_detail());
        assert!(pulls.take_diff_ask(true, now()).is_some());
        assert_eq!(pulls.take_viewed_ask(true), None);
        pulls.page(40, 30, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"3 files".into()), "{rows:#?}");
        assert!(!rows.iter().any(|row| row.contains("iewed")), "{rows:#?}");
    }

    /// A diff of one file changed in place, whose hunks at lines 3, 254 and 256 leave runs of
    /// 2, 250 and 1 unchanged lines before them.
    fn stretched() -> Box<pull_request::Diff> {
        let patch = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n\
                     @@ -3 +3 @@\n-o3\n+l3\n@@ -254 +254 @@\n-o254\n+l254\n\
                     @@ -256 +256 @@\n-o256\n+l256\n";
        Box::new(pull_request::Diff {
            patch: patch.into(),
            truncated: false,
            next_cursor: None,
            omitted: Vec::new(),
            omitted_cut: false,
        })
    }

    #[test]
    fn a_run_of_unchanged_lines_reads_its_files_text_once() {
        let theme = theme();
        let diff = stretched;
        let text = |count: u64| {
            let new = (1..=count).map(|n| format!("l{n}\n")).collect();
            Ok(Box::new(pull_request::FileContents { new }))
        };
        let text_ask = |ticket: u64| TextAsk {
            ticket,
            host: "github.com".into(),
            repository: "krishhgg/t3term".into(),
            number: 31,
            commit: None,
            change: pull_request::ChangeType::Change,
            old_path: "f.rs".into(),
            new_path: "f.rs".into(),
        };
        let mut pulls = conversing(remarks(1));
        assert!(!pulls.answer(1, Part::Done, now()));
        assert!(pulls.forward() && pulls.forward());
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        assert!(pulls.answer_diff(2, Ok(diff()), now()));
        pulls.page(60, 40, now(), true, &theme);
        for _ in 0..6 {
            pulls.step(true);
        }
        assert_eq!(pulls.pick, Some(Control::File(0)));
        pulls.enter();
        pulls.page(60, 40, now(), true, &theme);
        // Nothing reads until Enter on a run.
        assert_eq!(pulls.take_text_ask(true), None);
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Stretch(0, 0)));
        assert_eq!(pulls.action().as_deref(), Some("show 2 unmodified lines"));
        pulls.step(true);
        assert_eq!(pulls.pick, Some(Control::Stretch(0, 1)));
        assert_eq!(pulls.action().as_deref(), Some("show 200 unmodified lines"));

        // Enter asks for the file's text once, and the run stays chosen while it comes.
        pulls.enter();
        assert_eq!(pulls.pick, Some(Control::Stretch(0, 1)));
        assert_eq!(pulls.take_text_ask(false), None);
        assert_eq!(pulls.take_text_ask(true), Some(text_ask(3)));
        assert_eq!(pulls.take_text_ask(true), None);
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(
            rows.contains(&"⋯ 250 unmodified lines · Reading…".to_string()),
            "{rows:#?}"
        );

        // An answer to another read changes nothing. A failure says why under the run, and
        // Enter reads again.
        assert!(!pulls.answer_text(99, text(400)));
        let failure = Err("GitHub took too long to answer.".to_string());
        assert!(pulls.answer_text(3, failure));
        assert!(!pulls.answer_text(3, text(400)));
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        let at = rows
            .iter()
            .position(|row| row == "⋯ 250 unmodified lines")
            .expect("the run");
        assert_eq!(rows[at + 1], "GitHub took too long to answer.");
        assert_eq!(pulls.pick, Some(Control::Stretch(0, 1)));

        // A run that shows whole once the text comes goes, and so does its choice.
        pulls.pick = Some(Control::Stretch(0, 2));
        assert_eq!(pulls.action().as_deref(), Some("show 1 unmodified line"));
        pulls.enter();
        assert_eq!(pulls.take_text_ask(true), Some(text_ask(4)));
        assert!(pulls.answer_text(4, text(400)));
        assert_eq!(pulls.pick, None);
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        let at = rows
            .iter()
            .position(|row| row == "255   l255")
            .expect("the run shown");
        assert_eq!(rows[at - 1], "254 + l254");
        assert_eq!(rows[at + 1], "256 - o256");

        // With the text read, Enter shows lines at once and asks for nothing.
        pulls.pick = Some(Control::Stretch(0, 1));
        pulls.enter();
        assert_eq!(pulls.take_text_ask(true), None);
        assert_eq!(pulls.pick, Some(Control::Stretch(0, 1)));
        assert_eq!(pulls.action().as_deref(), Some("show 50 unmodified lines"));
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(rows.contains(&"103   l103".to_string()), "{rows:#?}");
        assert!(rows.contains(&"⋯ 50 unmodified lines".to_string()));
    }

    #[test]
    fn a_run_under_a_commits_diff_reads_that_commits_text() {
        let theme = theme();
        let mut activity = remarks(1);
        activity["commits"] = json!([{"oid": "abc1234def", "messageHeadline": "One",
            "committedDate": "2026-10-13T00:00:00Z"}]);
        let mut pulls = conversing(activity);
        assert!(!pulls.answer(1, Part::Done, now()));
        assert!(pulls.forward() && pulls.forward());
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        assert!(pulls.answer_diff(2, Ok(stretched()), now()));
        pulls.page(60, 40, now(), true, &theme);
        pulls.pick = Some(Control::Commits);
        pulls.enter();
        pulls.page(60, 40, now(), true, &theme);
        pulls.pick = Some(Control::Commit(0));
        pulls.enter();
        let ask = pulls.take_diff_ask(true, now()).expect("the commit's diff");
        assert_eq!(ask.commit.as_deref(), Some("abc1234def"));
        assert!(pulls.answer_diff(ask.ticket, Ok(stretched()), now()));
        pulls.page(60, 40, now(), true, &theme);
        pulls.pick = Some(Control::File(0));
        pulls.enter();
        pulls.page(60, 40, now(), true, &theme);
        pulls.pick = Some(Control::Stretch(0, 1));
        pulls.enter();
        let ask = pulls.take_text_ask(true).expect("the file's text");
        assert_eq!(ask.commit.as_deref(), Some("abc1234def"));
        assert_eq!(
            (ask.old_path.as_str(), ask.new_path.as_str()),
            ("f.rs", "f.rs")
        );
    }

    #[test]
    fn the_code_goes_where_the_host_stops_giving_a_diff() {
        let mut pulls = on_code();
        assert_eq!(pulls.take_diff_ask(true, now()), Some(diff_ask(2, None)));
        assert!(pulls.reload(now()));
        let ask = pulls.take_ask(true, now()).expect("the read again");
        let mut detail = bare_detail();
        detail["capabilities"]["diff"] = json!(false);
        let detail = Part::Detail(Ok(read_detail(&detail)));
        pulls.answer(ask.ticket, detail, now());
        let activity = pull_request::Activity::read(&remarks(1)).unwrap();
        pulls.answer(ask.ticket, Part::Activity(Ok(Box::new(activity))), now());
        assert_eq!(pulls.tabs(), Some(Tab::Code));
        pulls.answer(ask.ticket, Part::Done, now());
        // The reader sees the Summary, and the Timeline is the last tab.
        assert_eq!(pulls.tabs(), Some(Tab::Summary));
        assert!(!pulls.offers_code());
        assert!(pulls.forward() && !pulls.forward());
        assert_eq!(pulls.take_diff_ask(true, now()), None);
    }

    #[test]
    fn a_commit_on_the_timeline_shows_its_diff_on_the_code_tab() {
        let theme = theme();
        let mut activity = remarks(1);
        activity["commits"] = json!([
            {"oid": "1111111aaaa", "messageHeadline": "First",
                "committedDate": "2026-10-13T00:01:00Z"},
            {"oid": "2222222bbbb", "messageHeadline": "Second",
                "committedDate": "2026-10-13T00:02:00Z"},
        ]);
        activity["reviewThreads"] = json!([
            {"id": "t0", "path": "f0.rs", "line": 1, "side": "right", "isResolved": false,
                "isOutdated": false, "comments": [{"id": "r0",
                "author": {"login": "mona", "name": null}, "body": "On line 1.",
                "createdAt": "2026-10-13T00:30:00Z", "url": null}]},
        ]);
        let scoped = |ticket: u64, commit: &str| DiffAsk {
            commit: Some(commit.to_string()),
            ..diff_ask(ticket, None)
        };
        let commits_of = |pulls: &PullRequests| -> Vec<Control> {
            let targets = &pulls.detail.as_ref().expect("the details").targets;
            let commits = targets.iter().map(|(_, control)| *control);
            commits
                .filter(|control| matches!(control, Control::TimelineCommit(_)))
                .collect()
        };
        let mut pulls = conversing(activity.clone());
        assert!(!pulls.answer(1, Part::Done, now()));
        assert!(pulls.forward());
        pulls.page(60, 40, now(), true, &theme);
        // Each commit's rows are a control, the newest first, by its place as T3 sent it.
        assert_eq!(
            commits_of(&pulls),
            [Control::TimelineCommit(1), Control::TimelineCommit(0)]
        );
        for _ in 0..10 {
            if pulls.pick == Some(Control::TimelineCommit(1)) {
                break;
            }
            pulls.step(true);
        }
        assert_eq!(pulls.pick, Some(Control::TimelineCommit(1)));
        assert_eq!(pulls.action().as_deref(), Some("view commit 2222222"));

        // Enter shows the Code with that commit's diff, though the Code never showed before.
        pulls.enter();
        assert_eq!(pulls.tabs(), Some(Tab::Code));
        assert_eq!(pulls.pick, None);
        assert_eq!(
            pulls.take_diff_ask(true, now()),
            Some(scoped(2, "2222222bbbb"))
        );
        assert!(pulls.answer_diff(2, Ok(added(0, 1, None)), now()));
        pulls.page(60, 40, now(), true, &theme);
        let rows = detail_rows(&pulls);
        assert!(
            rows.contains(&"› Second · 2222222".to_string()),
            "{rows:#?}"
        );
        pulls.pick = Some(Control::OffDiff);
        pulls.enter();
        assert!(pulls.shown.off_diff_open());

        // The same commit again shows the Code where the reader left it, and reads nothing.
        pulls.scroll = Offset::Top(3);
        assert!(pulls.back());
        assert_eq!(pulls.tabs(), Some(Tab::Timeline));
        pulls.pick = Some(Control::TimelineCommit(1));
        pulls.enter();
        assert_eq!(pulls.tabs(), Some(Tab::Code));
        assert_eq!(pulls.scroll, Offset::Top(3));
        assert!(pulls.shown.off_diff_open());
        assert_eq!(pulls.take_diff_ask(true, now()), None);

        // A click on another shows it from the top of the Code, with the list off the diff closed.
        assert!(pulls.back());
        pulls.pick = Some(Control::TimelineCommit(0));
        assert_eq!(pulls.action().as_deref(), Some("view commit 1111111"));
        let (rows, _) = control_rows(&mut pulls, 40, true, Control::TimelineCommit(0));
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(pulls.click(0, rows[1]));
        assert_eq!(pulls.tabs(), Some(Tab::Code));
        assert_eq!(pulls.scroll, Offset::Top(0));
        assert!(!pulls.shown.off_diff_open());
        assert_eq!(
            pulls.take_diff_ask(true, now()),
            Some(scoped(3, "1111111aaaa"))
        );
        // A place T3 sent no commit at opens nothing.
        assert!(pulls.back());
        pulls.pick = Some(Control::TimelineCommit(2));
        pulls.enter();
        assert_eq!(pulls.tabs(), Some(Tab::Timeline));

        // Where the host gives no diff, a commit opens nothing.
        let mut detail = bare_detail();
        detail["capabilities"]["diff"] = json!(false);
        let mut pulls = conversing_on(&detail, activity);
        assert!(!pulls.answer(1, Part::Done, now()));
        assert!(pulls.forward());
        pulls.page(60, 40, now(), true, &theme);
        assert_eq!(commits_of(&pulls), []);
        pulls.pick = Some(Control::TimelineCommit(1));
        pulls.enter();
        assert_eq!(pulls.tabs(), Some(Tab::Timeline));
        assert_eq!(pulls.take_diff_ask(true, now()), None);
    }
}
