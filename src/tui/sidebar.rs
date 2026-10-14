//! The sidebar: which threads it shows, the shelf each one sits on, the order inside a shelf,
//! and how the list draws.
//!
//! The rules are those of T3 nightly v0.0.46-nightly.20261007.2787 (commit f570bd2):
//!
//! - Visibility is `filterSidebarV2VisibleThreads` in `apps/web/src/components/Sidebar.logic.ts`.
//! - The shelf is `resolveSidebarThreadSection` in the same file, fed by the partition in
//!   `apps/web/src/components/Sidebar.tsx`.
//! - The order inside each shelf comes from `packages/client-runtime/src/state/threadSort.ts`.
//! - Snooze is `effectiveSnoozed` in `packages/client-runtime/src/state/threadSettled.ts`.
//! - The Working shelf is `isThreadWorking`, `sortInboxThreadsByReturn`,
//!   `sortWorkingThreadsBySend` and `createInboxReturnTracker` in
//!   `packages/client-runtime/src/state/threadInbox.ts`. The GUI's setting for it starts off.
//!   t3term's starts on.
//! - The Done shelf is t3term's own. The GUI has none and lists those threads in its inbox
//!   with the Done word.
//! - A card's status word is `resolveSidebarThreadStatus` and `resolveSidebarV2TopStatus` in
//!   `Sidebar.logic.ts`, with the labels, colors and Woke test of `SidebarThreadRow` in
//!   `Sidebar.tsx` and the wake time from `threadWokeAt` in `threadSettled.ts`.
//!
//! Field names are the shell's (`OrchestrationV2ThreadShell` in
//! `packages/contracts/src/orchestrationV2.ts`), read the way `presentThreadShell` in
//! `packages/client-runtime/src/state/models.ts` hands them to the GUI.
//!
//! A snooze ends at a wall-clock time and no server event marks it, so the sidebar reports its
//! next wake and the event loop sets one timer for it.
//!
//! Done and Woke compare against the shell's `lastVisitedAt`, which the server keeps for every
//! client. The GUI's visit records the thread's `updatedAt`, so a timer wake outlasts a visit,
//! and dismissing Woke records a visit at the wake time. t3term reads the field but doesn't send
//! `thread.visit` yet, so opening a thread here doesn't clear either word. The GUI keeps a visit
//! time of its own for a server that leaves the field out. t3term has none, so on such a server
//! a thread is never Done, and a woken thread stays Woke until it is settled or the server
//! clears its snooze, as a new message does.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::environment::{Feed, Identity};
use super::label;
use super::theme::{self, Badge, Theme, parse_iso_ms, relative_time};
use super::{fit, row, spinner_frame, str_of, with_bg};
use crate::projection::{Applied, ShellState, is_terminal_status, status};

/// The lifecycle features a server supports, from its environment descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub settlement: bool,
    pub snooze: bool,
}

impl Capabilities {
    /// Both keys are optional in `packages/contracts/src/environment.ts`. A server that leaves
    /// one out predates the feature, so only `true` turns it on.
    pub fn of(capabilities: &Value) -> Capabilities {
        Capabilities {
            settlement: capabilities["threadSettlement"] == true,
            snooze: capabilities["threadSnooze"] == true,
        }
    }
}

/// A sidebar shelf, in the order the list shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shelf {
    Pinned,
    /// The threads that are the user's turn, which the GUI calls the inbox. Its heading shows
    /// only under the Pinned shelf, where it marks the end of the pins.
    Inbox,
    /// Threads busy without the user, when the Working shelf is on.
    Working,
    /// The inbox's threads whose latest run finished after the last visit, which read Done.
    Done,
    Snoozed,
    Settled,
}

impl Shelf {
    fn label(self) -> &'static str {
        match self {
            Shelf::Pinned => "Pinned",
            Shelf::Inbox => "Inbox",
            Shelf::Working => "Working",
            Shelf::Done => "Done",
            Shelf::Snoozed => "Snoozed",
            Shelf::Settled => "Settled",
        }
    }

    /// Whether the shelf opens and closes, with a heading that counts its threads.
    fn folds(self) -> bool {
        matches!(self, Shelf::Working | Shelf::Done | Shelf::Settled)
    }
}

/// `resolveSidebarThreadSection`: a snooze wins until it wakes, then settlement wins over a
/// stale pin.
fn shelf_for(snoozed: bool, settled: bool, pinned: bool) -> Shelf {
    if snoozed {
        Shelf::Snoozed
    } else if settled {
        Shelf::Settled
    } else if pinned {
        Shelf::Pinned
    } else {
        Shelf::Inbox
    }
}

/// The shelf a visible thread sits on at `now`. Snooze and settlement count only on a server
/// that supports them, and a pin always counts. A thread is settled when its override says so.
/// A `settledAt` stamp alone is history, not the thread's state.
fn shelf(thread: &Value, capabilities: Capabilities, now: i64) -> Shelf {
    shelf_for(
        capabilities.snooze && effective_snoozed(thread, now),
        capabilities.settlement && thread["settledOverride"] == "settled",
        !thread["pinnedAt"].is_null(),
    )
}

/// `filterSidebarV2VisibleThreads`: archived threads and subagents stay out of the sidebar,
/// since a subagent shows under its parent. A fork is an ordinary thread and stays.
fn visible(thread: &Value) -> bool {
    thread["archivedAt"].is_null() && thread["lineage"]["relationshipToParent"] != "subagent"
}

// ---- timestamps ----

/// Milliseconds for a timestamp in the form T3 writes, `2026-10-08T00:06:52.769Z`. Anything
/// else gives `None`, as `Date.parse` gives `NaN`, so a malformed value never hides a thread.
fn instant(value: &Value) -> Option<i64> {
    let text = value.as_str()?;
    let bytes = text.as_bytes();
    let (head, tail) = (bytes.get(..19)?, &bytes[19..]);
    let shape = b"dddd-dd-ddTdd:dd:dd";
    let fits = head.iter().zip(shape).all(|(&byte, &want)| {
        if want == b'd' {
            byte.is_ascii_digit()
        } else {
            byte == want
        }
    });
    let ends = match tail {
        [b'Z'] => true,
        [b'.', digits @ .., b'Z'] => !digits.is_empty() && digits.iter().all(u8::is_ascii_digit),
        _ => false,
    };
    if !fits || !ends {
        return None;
    }
    let two = |from: usize| text[from..from + 2].parse::<u32>().unwrap_or(u32::MAX);
    let (month, day, hour, minute, second) = (two(5), two(8), two(11), two(14), two(17));
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    parse_iso_ms(text)
}

/// `Date.parse(a) > Date.parse(b)`, which is false when either doesn't parse.
fn later(a: &Value, b: &Value) -> bool {
    matches!((instant(a), instant(b)), (Some(a), Some(b)) if a > b)
}

static NULL: Value = Value::Null;

// ---- snooze ----

/// The latest run as `presentThreadShell` gives it to the GUI.
struct LatestRun<'a> {
    completed: bool,
    requested_at: &'a Value,
    started_at: &'a Value,
    completed_at: &'a Value,
}

/// None before the first run. An idle thread's run counts as completed, and when the server
/// leaves out the completion time, a finished run takes `updatedAt` for it.
fn latest_run(thread: &Value) -> Option<LatestRun<'_>> {
    if thread["latestRunId"].is_null() {
        return None;
    }
    let state = status(thread);
    let completed_at = match thread.get("latestRunCompletedAt") {
        Some(value) => value,
        None if state == "idle" || is_terminal_status(state) => &thread["updatedAt"],
        None => &NULL,
    };
    Some(LatestRun {
        completed: state == "idle" || state == "completed",
        requested_at: &thread["latestRunRequestedAt"],
        started_at: &thread["latestRunStartedAt"],
        completed_at,
    })
}

/// The runtime status `shellRuntime` gives the GUI: idle while background work that wakes the
/// agent is pending, unless the run failed, else the activity run's status or the thread's own.
/// None for a thread with no run and no provider thread.
fn runtime_status(thread: &Value) -> Option<&str> {
    let state = status(thread);
    // `backgroundWorkHoldsCompletion` in `packages/shared/src/orchestrationV2PendingBackgroundWork.ts`:
    // every kind holds but a command, including kinds this build doesn't know.
    let park = state != "failed"
        && thread["pendingBackgroundTasks"]
            .as_array()
            .is_some_and(|tasks| tasks.iter().any(|task| task["kind"] != "command"));
    if thread["latestRunId"].is_null() && thread["activeProviderThreadId"].is_null() && !park {
        return None;
    }
    Some(if park {
        "idle"
    } else {
        thread["activityRunStatus"].as_str().unwrap_or(state)
    })
}

/// What a pending request asks of the user, as `presentThreadShell` sorts it into
/// `hasPendingUserInput` and `hasPendingApprovals`: a question is input, an auth refresh is
/// neither, and every other kind, known or not, is an approval.
fn pending(thread: &Value) -> Option<Word> {
    let request = &thread["pendingRuntimeRequest"];
    if request.is_null() {
        return None;
    }
    match request["kind"].as_str() {
        Some("user_input") => Some(Word::Input),
        Some("auth_refresh") => None,
        _ => Some(Word::Approval),
    }
}

/// `threadRaisedHandWhileSnoozed`: the agent is waiting on the user, it failed after the
/// snooze began, or a run completed after the snooze began.
fn raised_hand(thread: &Value) -> bool {
    if pending(thread).is_some() {
        return true;
    }
    // A thread snoozed while already failed stays snoozed. The runtime's `updatedAt` is the
    // thread's.
    let snoozed_at = &thread["snoozedAt"];
    if matches!(runtime_status(thread), Some("error" | "failed"))
        && (snoozed_at.is_null() || later(&thread["updatedAt"], snoozed_at))
    {
        return true;
    }
    !snoozed_at.is_null()
        && latest_run(thread)
            .is_some_and(|run| run.completed && later(run.completed_at, snoozed_at))
}

/// `effectiveSnoozed`: off the active list until the wake time, unless the thread raised its
/// hand first. A wake time that has passed or doesn't parse leaves the thread where it was.
fn effective_snoozed(thread: &Value, now: i64) -> bool {
    instant(&thread["snoozedUntil"]).is_some_and(|wake| wake > now) && !raised_hand(thread)
}

// ---- status ----

/// The word on a card's first line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Word {
    Approval,
    Input,
    Working,
    /// Working toward a native `/goal`, which keeps the agent going across turns until it is
    /// met.
    Goal,
    /// Stopped, with background work pending that will wake the agent, so not the user's
    /// turn yet.
    Waiting,
    /// Failed on a usage limit.
    Limited,
    Failed,
    Woke,
    /// A run finished that nobody has looked at since.
    Done,
}

impl Word {
    fn label(self) -> &'static str {
        match self {
            Word::Approval => "Approval",
            Word::Input => "Input",
            Word::Working => "Working",
            Word::Goal => "Goal",
            Word::Waiting => "Waiting",
            Word::Limited => "Limited",
            Word::Failed => "Failed",
            Word::Woke => "Woke",
            Word::Done => "Done",
        }
    }

    /// The text color each word has in the GUI.
    fn color(self, t: &Theme) -> Color {
        match self {
            Word::Approval => t.warning_fg,
            Word::Input => t.indigo,
            Word::Working | Word::Goal => t.info,
            Word::Waiting => t.sidebar_muted,
            Word::Limited | Word::Woke => t.warning,
            Word::Failed => t.error,
            Word::Done => t.emerald,
        }
    }

    /// Whether the word comes with a spinner and the working clock.
    fn working(self) -> bool {
        matches!(self, Word::Working | Word::Goal)
    }
}

/// `resolveSidebarThreadStatus`: what the thread is doing, or None when it is at rest. A
/// request for the user comes before anything the runtime is doing.
fn activity(thread: &Value) -> Option<Word> {
    if let Some(word) = pending(thread) {
        return Some(word);
    }
    match runtime_status(thread)? {
        "preparing" | "queued" | "starting" | "running" | "waiting" => {
            Some(if thread["goal"]["status"] == "active" {
                Word::Goal
            } else {
                Word::Working
            })
        }
        "idle" => Some(Word::Waiting),
        "failed" if thread["lastErrorClass"] == "usage_limit" => Some(Word::Limited),
        "failed" => Some(Word::Failed),
        _ => None,
    }
}

/// The word a card shows at `now`, after `resolveSidebarV2TopStatus`: what the thread is
/// doing, else a wake, else a finish nobody has seen. None leaves the card showing its age.
fn status_word(thread: &Value, now: i64) -> Option<Word> {
    activity(thread).or_else(|| {
        if woke(thread, now) {
            Some(Word::Woke)
        } else if unseen_completion(thread) {
            Some(Word::Done)
        } else {
            None
        }
    })
}

/// Whether the thread's card shows Working or Goal, whose spinner and clock move with the
/// event loop's tick.
fn working(thread: &Value) -> bool {
    activity(thread).is_some_and(Word::working)
}

/// `resolveThreadWorkingStartedAt`: when the work on show began, which a wake that continues
/// it doesn't reset. A server that sends `activityRunStartedAt` decides alone. On an older
/// one the clock counts from the latest run while that run is the active one. None leaves the
/// clock off.
fn working_since(thread: &Value) -> Option<i64> {
    if let Some(started) = thread.get("activityRunStartedAt") {
        return instant(started);
    }
    let run = latest_run(thread)?;
    if run.completed_at.is_null() && thread["activeRunId"] == thread["latestRunId"] {
        instant(run.started_at).or_else(|| instant(run.requested_at))
    } else {
        None
    }
}

/// `threadWokeAt`: when a snoozed thread came back, or None while it sleeps or if it never
/// slept. A thread that raised its hand woke at that moment, and keeps that time after the
/// wake time passes, so a visit between the two still clears the word.
fn woke_at(thread: &Value, now: i64) -> Option<&Value> {
    let wake = instant(&thread["snoozedUntil"])?;
    if !raised_hand(thread) {
        return (wake <= now).then_some(&thread["snoozedUntil"]);
    }
    let snoozed_at = &thread["snoozedAt"];
    let finished =
        latest_run(thread).filter(|run| run.completed && later(run.completed_at, snoozed_at));
    if let Some(run) = finished {
        return Some(run.completed_at);
    }
    // The runtime's `updatedAt` is the thread's. Without a runtime, the snooze's start.
    let runtime_at = runtime_status(thread).map(|_| &thread["updatedAt"]);
    runtime_at
        .into_iter()
        .chain([snoozed_at])
        .find(|at| !at.is_null())
}

/// `isWoke` in `SidebarThreadRow`: the thread woke after the last visit, and it isn't
/// settled. A visit time that is missing or doesn't parse counts as no visit.
fn woke(thread: &Value, now: i64) -> bool {
    let Some(at) = woke_at(thread, now).and_then(instant) else {
        return false;
    };
    thread["settledOverride"] != "settled"
        && instant(&thread["lastVisitedAt"]).is_none_or(|visited| visited < at)
}

/// `hasUnseenCompletion`: the latest run finished after the last visit. A thread nobody has
/// visited counts as seen, so a new server doesn't mark its whole history unread, and so
/// does every thread on a server that doesn't send `lastVisitedAt`. A visit time that doesn't
/// parse counts as before the finish.
fn unseen_completion(thread: &Value) -> bool {
    let Some(completed) = latest_run(thread).and_then(|run| instant(run.completed_at)) else {
        return false;
    };
    let visited = &thread["lastVisitedAt"];
    if visited.as_str().is_none_or(str::is_empty) {
        return false;
    }
    instant(visited).is_none_or(|visited| completed > visited)
}

// ---- order ----

fn id(thread: &Value) -> &str {
    str_of(thread, "id")
}

/// `sortPinnedThreadsByOrderKey`: threads the user arranged, by their keys, then the rest
/// newest created first. Ties go by id.
fn sort_pinned(threads: &mut [&Value]) {
    threads.sort_by_cached_key(|&t| match t["pinOrderKey"].as_str() {
        Some(key) => (0, key, Reverse(0), id(t)),
        None => (1, "", Reverse(instant(&t["createdAt"]).unwrap_or(0)), id(t)),
    });
}

/// `sortActiveThreadsByOrderKey`: new and reopened threads first, newest first, then the
/// threads the user arranged, by their keys. Ties go by id.
fn sort_active(threads: &mut [&Value]) {
    threads.sort_by_cached_key(|&t| match t["activeOrderKey"].as_str() {
        None => (0, "", Reverse(anchor(t)), id(t)),
        Some(key) => (1, key, Reverse(0), id(t)),
    });
}

/// `activeThreadAnchorTimestampMs`: when the thread was created, or when it last came back to
/// the active list if that is later.
fn anchor(thread: &Value) -> i64 {
    let at = |key: &str| instant(&thread[key]).unwrap_or(0);
    at("createdAt").max(at("unsettledAt"))
}

/// `sortSettledThreads`: newest first by `resolveSettledThreadTimestamp`. Ties go by id.
fn sort_settled(threads: &mut [&Value]) {
    threads.sort_by_cached_key(|&t| (Reverse(settled_ms(t)), id(t)));
}

/// `resolveSettledThreadTimestamp`: when the thread settled, else its latest message or run
/// time, else `updatedAt`. A thread with none of them sorts as 0.
fn settled_ms(thread: &Value) -> i64 {
    if let Some(at) = instant(&thread["settledAt"]) {
        return at;
    }
    let run = latest_run(thread);
    let run_times = run
        .iter()
        .flat_map(|run| [run.requested_at, run.started_at, run.completed_at]);
    std::iter::once(&thread["latestUserMessageAt"])
        .chain(run_times)
        .filter_map(instant)
        .max()
        .or_else(|| instant(&thread["updatedAt"]))
        .unwrap_or(0)
}

// ---- working ----

/// `isThreadWorking`: the agent is busy and nothing waits on the user. That is a run under
/// way, or one stopped with background work that will wake it. A request for the user, a
/// failure and a plan waiting for a reply keep the thread in the inbox. An auth refresh isn't
/// the user's to answer, so a thread waiting on one can still be working.
fn belongs_in_working(thread: &Value) -> bool {
    if pending(thread).is_some() {
        return false;
    }
    if !matches!(
        runtime_status(thread),
        Some("preparing" | "queued" | "starting" | "running" | "waiting" | "idle")
    ) {
        return false;
    }
    // A plan the user has to answer outranks background work still pending. `presentThreadShell`
    // gives the latest run the thread's status, with idle read as completed.
    let run_settled = !thread["latestRunId"].is_null()
        && !matches!(
            status(thread),
            "preparing" | "queued" | "starting" | "running" | "waiting"
        )
        && thread["activeRunId"] != thread["latestRunId"];
    !(thread["interactionMode"] == "plan"
        && thread["hasActionableProposedPlan"] == true
        && run_settled)
}

/// `createInboxReturnTracker`: when this client saw each thread leave the Working shelf. The
/// server stamps a run's request and its finish, but nothing marks an approval asked partway
/// through a run or background work ending, so the moment t3term sees the change stands in.
/// The stamps last as long as t3term runs.
#[derive(Debug, Default)]
struct Returns {
    /// The threads that belonged in Working at the last look, or None before the first.
    working: Option<HashSet<String>>,
    at: HashMap<String, i64>,
}

impl Returns {
    /// Looks at every thread the shell has, listed or not, at `now`. The first look only notes
    /// which threads are working, so starting t3term stamps nothing and moves no card.
    fn observe(&mut self, threads: &[Value], now: i64) {
        let present: HashSet<&str> = threads.iter().map(id).collect();
        let working: HashSet<String> = threads
            .iter()
            .filter(|t| belongs_in_working(t))
            .map(|t| id(t).to_string())
            .collect();
        // A thread that has gone takes its stamp with it, so the map holds only live threads.
        self.at.retain(|key, _| present.contains(key.as_str()));
        for left in self.working.iter().flatten() {
            if present.contains(left.as_str()) && !working.contains(left) {
                self.at.insert(left.clone(), now);
            }
        }
        self.working = Some(working);
    }

    fn returned_at(&self, thread: &Value) -> Option<i64> {
        self.at.get(id(thread)).copied()
    }
}

/// `sortInboxThreadsByReturn`: with the Working shelf on, the inbox lists threads newest first
/// by when each last came back to the user, so a thread that leaves Working lands on top. The
/// order the user arranged doesn't apply. Ties go by id.
fn sort_by_return(threads: &mut [&Value], returns: &Returns) {
    threads.sort_by_cached_key(|&t| (Reverse(returned_ms(t, returns)), id(t)));
}

/// When the thread last came back: created, reopened, its latest run asked for or finished, or
/// seen leaving Working, whichever is latest.
fn returned_ms(thread: &Value, returns: &Returns) -> i64 {
    let at = |key: &str| instant(&thread[key]).unwrap_or(0);
    let run = latest_run(thread);
    let run_times = run
        .iter()
        .flat_map(|run| [run.requested_at, run.completed_at])
        .filter_map(instant);
    [
        at("createdAt"),
        at("unsettledAt"),
        returns.returned_at(thread).unwrap_or(0),
    ]
    .into_iter()
    .chain(run_times)
    .max()
    .unwrap_or(0)
}

/// `sortWorkingThreadsBySend`: Working lists threads newest first by the last message the user
/// sent, so a run finishing or waking again doesn't move a card. Ties go by id.
fn sort_by_send(threads: &mut [&Value]) {
    threads.sort_by_cached_key(|&t| (Reverse(sent_ms(t)), id(t)));
}

/// When the user last sent the thread a message, or when it was created if that is later. A
/// server that leaves out `latestUserAuthoredMessageAt` gives the latest run's request time
/// instead, which a wake moves. One that sends it as null gives 0.
fn sent_ms(thread: &Value) -> i64 {
    let sent = match thread.get("latestUserAuthoredMessageAt") {
        Some(at) => instant(at),
        None => latest_run(thread).and_then(|run| instant(run.requested_at)),
    };
    instant(&thread["createdAt"])
        .unwrap_or(0)
        .max(sent.unwrap_or(0))
}

/// The visible threads on each shelf, in the order the GUI lists them.
#[derive(Default)]
struct Shelves<'a> {
    pinned: Vec<&'a Value>,
    inbox: Vec<&'a Value>,
    working: Vec<&'a Value>,
    done: Vec<&'a Value>,
    snoozed: Vec<&'a Value>,
    settled: Vec<&'a Value>,
}

impl Shelves<'_> {
    /// Whether any shelf has a thread, so the list isn't empty.
    fn any(&self) -> bool {
        [
            &self.pinned,
            &self.inbox,
            &self.working,
            &self.done,
            &self.snoozed,
            &self.settled,
        ]
        .iter()
        .any(|list| !list.is_empty())
    }
}

/// The partition in `Sidebar.tsx`. `returns` is None while the Working shelf is off. With it
/// on, threads in the inbox that belong in Working go there instead. Only the inbox folds
/// away: a pinned, snoozed or settled thread keeps its shelf while it works. Then the inbox's
/// threads that read Done move to their own shelf in the inbox's order. A thread that works
/// never reads Done, so the two never compete.
fn shelves<'a>(
    threads: &'a [Value],
    capabilities: Capabilities,
    now: i64,
    returns: Option<&Returns>,
) -> Shelves<'a> {
    let mut shelves = Shelves::default();
    for thread in threads.iter().filter(|t| visible(t)) {
        let list = match shelf(thread, capabilities, now) {
            Shelf::Pinned => &mut shelves.pinned,
            Shelf::Inbox if returns.is_some() && belongs_in_working(thread) => &mut shelves.working,
            Shelf::Inbox => &mut shelves.inbox,
            Shelf::Working => &mut shelves.working,
            Shelf::Done => &mut shelves.done,
            Shelf::Snoozed => &mut shelves.snoozed,
            Shelf::Settled => &mut shelves.settled,
        };
        list.push(thread);
    }
    sort_pinned(&mut shelves.pinned);
    match returns {
        Some(returns) => sort_by_return(&mut shelves.inbox, returns),
        None => sort_active(&mut shelves.inbox),
    }
    (shelves.done, shelves.inbox) = std::mem::take(&mut shelves.inbox)
        .into_iter()
        .partition::<Vec<_>, _>(|t| status_word(t, now) == Some(Word::Done));
    sort_by_send(&mut shelves.working);
    // Soonest wake first. Every wake here parses, or the thread wouldn't be snoozed.
    shelves
        .snoozed
        .sort_by_cached_key(|&t| instant(&t["snoozedUntil"]).unwrap_or(0));
    sort_settled(&mut shelves.settled);
    shelves
}

// ---- rows ----

#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    /// A shelf's name over its threads. A heading under a thread has a blank line above it. One
    /// under another heading has none, so empty shelves stack.
    Heading { shelf: Shelf, gap: bool },
    /// A thread's card.
    Thread(String),
    /// A settled thread's one-line row, after the GUI's slim row.
    Slim(String),
}

#[cfg(test)]
thread_local! {
    /// How many rows this thread has measured, so a test can see how far a scroll looks.
    static MEASURED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Row {
    /// Rows on screen: a card is three lines and a gap, a slim row one line, and a heading one
    /// line and its gap.
    fn height(&self) -> usize {
        #[cfg(test)]
        MEASURED.set(MEASURED.get() + 1);
        match self {
            Row::Heading { gap, .. } => 1 + usize::from(*gap),
            Row::Thread(_) => 4,
            Row::Slim(_) => 1,
        }
    }

    /// The thread the row shows. None for a heading.
    fn thread(&self) -> Option<&str> {
        match self {
            Row::Thread(id) | Row::Slim(id) => Some(id),
            Row::Heading { .. } => None,
        }
    }

    /// Whether two rows show the same thing, a heading's gap aside.
    fn same(&self, other: &Row) -> bool {
        match (self, other) {
            (Row::Heading { shelf: a, .. }, Row::Heading { shelf: b, .. }) => a == b,
            (Row::Thread(a), Row::Thread(b)) | (Row::Slim(a), Row::Slim(b)) => a == b,
            _ => false,
        }
    }
}

/// Whether `rows` fit in `height` lines. It stops at the first row past the bottom, so the
/// rows below the screen cost nothing to check.
fn fits(rows: &[Row], height: usize) -> bool {
    let mut lines = 0;
    rows.iter().all(|row| {
        lines += row.height();
        lines <= height
    })
}

/// Whether busy threads get the Working shelf, and which of the shelves that fold list their
/// threads. The settings decide them at start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShelfChoices {
    pub working_enabled: bool,
    pub working_open: bool,
    pub done_open: bool,
    pub settled_open: bool,
}

impl Default for ShelfChoices {
    /// As a new settings file has them: the Working shelf on, Working and Done open, and
    /// Settled closed.
    fn default() -> ShelfChoices {
        ShelfChoices {
            working_enabled: true,
            working_open: true,
            done_open: true,
            settled_open: false,
        }
    }
}

impl ShelfChoices {
    /// Whether `shelf` lists all its threads. A shelf that doesn't fold always does.
    fn open(&self, shelf: Shelf) -> bool {
        match shelf {
            Shelf::Working => self.working_open,
            Shelf::Done => self.done_open,
            Shelf::Settled => self.settled_open,
            Shelf::Pinned | Shelf::Inbox | Shelf::Snoozed => true,
        }
    }

    /// The choice that opens `shelf`, when the shelf folds.
    fn open_mut(&mut self, shelf: Shelf) -> Option<&mut bool> {
        match shelf {
            Shelf::Working => Some(&mut self.working_open),
            Shelf::Done => Some(&mut self.done_open),
            Shelf::Settled => Some(&mut self.settled_open),
            Shelf::Pinned | Shelf::Inbox | Shelf::Snoozed => None,
        }
    }
}

/// What the sidebar lists and where the reader is in it.
#[derive(Default)]
pub struct Sidebar {
    rows: Vec<Row>,
    selected: usize,
    /// The first row drawn.
    offset: usize,
    choices: ShelfChoices,
    /// Whether the list has any thread, which gives it the headings of the shelves that fold.
    listed: bool,
    /// How many threads each shelf that folds holds, shown or not.
    working_count: usize,
    done_count: usize,
    settled_count: usize,
    /// When threads came back from Working, for the inbox's order while the shelf is on.
    returns: Returns,
    /// When the soonest snooze ends, in milliseconds. No server event marks it, so the event
    /// loop wakes for it.
    pub next_wake: Option<i64>,
    // Last drawn geometry, for mouse hit-testing.
    pub list: Rect,
    /// The one-column strip on the right that stands in for the GUI's border, which a drag
    /// resizes the sidebar by.
    pub border: Rect,
    /// The screen rows each drawn card or slim row covers, top inclusive and bottom
    /// exclusive, with its thread.
    cards: Vec<(u16, u16, String)>,
    /// Whether the last frame drew a card that reads Working or Goal.
    drew_working: bool,
    /// The screen rows the last frame drew the headings of the shelves that fold on, with
    /// their shelves.
    headings: Vec<(u16, Shelf)>,
    /// Whether the last frame drew the feed's dot by the wordmark, with the environment
    /// under it. Without it the breadcrumb's row shows them.
    drew_environment: bool,
}

impl Sidebar {
    /// An empty sidebar with the shelves as the settings have them.
    pub fn with_choices(choices: ShelfChoices) -> Sidebar {
        Sidebar {
            choices,
            ..Sidebar::default()
        }
    }

    /// Lays the shelves out as rows: a heading over each shelf that has threads, then its
    /// threads. Working, Done and Settled keep their headings, which count their threads,
    /// whenever the list has any thread, so each can be opened before it fills. The inbox has
    /// a heading only under the pins. A closed shelf lists only the open thread, which never
    /// hides there (`renderedSettledThreads` and `visibleWorkingThreads` in `Sidebar.tsx`).
    /// The highlight follows its thread, and the top row stays on top unless the list was
    /// scrolled to its start.
    ///
    /// With the Working shelf on, each call also looks for threads that have left it since the
    /// last, as the GUI does each time it lays out its list.
    pub fn rebuild(
        &mut self,
        threads: &[Value],
        capabilities: Capabilities,
        open_id: Option<&str>,
        now: i64,
    ) {
        let returns = if self.choices.working_enabled {
            self.returns.observe(threads, now);
            Some(&self.returns)
        } else {
            self.returns = Returns::default();
            None
        };
        let shelves = shelves(threads, capabilities, now, returns);
        self.listed = shelves.any();
        self.working_count = shelves.working.len();
        self.done_count = shelves.done.len();
        self.settled_count = shelves.settled.len();
        self.next_wake = shelves
            .snoozed
            .first()
            .and_then(|t| instant(&t["snoozedUntil"]));
        let selected = self.selected_thread_id().map(str::to_string);
        // The top row stays on top, so a change above it doesn't move the cards in view. A
        // list scrolled to its start stays there, so new threads at the top show up.
        let top = self
            .rows
            .get(self.offset)
            .filter(|_| self.offset > 0)
            .cloned();
        let was = self.selected;
        self.rows.clear();
        for (shelf, list) in [
            (Shelf::Pinned, &shelves.pinned),
            (Shelf::Inbox, &shelves.inbox),
            (Shelf::Working, &shelves.working),
            (Shelf::Done, &shelves.done),
            (Shelf::Snoozed, &shelves.snoozed),
            (Shelf::Settled, &shelves.settled),
        ] {
            if list.is_empty() && !self.has_shelf(shelf) {
                continue;
            }
            if shelf != Shelf::Inbox || !self.rows.is_empty() {
                let gap = self.rows.last().is_some_and(|row| row.thread().is_some());
                self.rows.push(Row::Heading { shelf, gap });
            }
            let open = self.choices.open(shelf);
            let shown = list
                .iter()
                .filter(|t| open || open_id == Some(id(t)))
                .map(|t| id(t).to_string());
            if shelf == Shelf::Settled {
                self.rows.extend(shown.map(Row::Slim));
            } else {
                self.rows.extend(shown.map(Row::Thread));
            }
        }
        // When the highlighted thread has gone, the highlight takes the next thread down, or
        // the last.
        let is_card = |row: &Row| row.thread().is_some();
        self.selected = selected
            .and_then(|selected| {
                self.rows
                    .iter()
                    .position(|row| row.thread() == Some(selected.as_str()))
            })
            .or_else(|| {
                self.rows
                    .iter()
                    .skip(was)
                    .position(is_card)
                    .map(|index| index + was)
            })
            .or_else(|| self.rows.iter().rposition(is_card))
            .unwrap_or(0);
        self.offset = match top {
            Some(top) => self
                .rows
                .iter()
                .position(|row| row.same(&top))
                .unwrap_or(self.offset.min(self.rows.len().saturating_sub(1))),
            None => 0,
        };
    }

    pub fn selected_thread_id(&self) -> Option<&str> {
        self.rows.get(self.selected).and_then(Row::thread)
    }

    /// Moves the highlight `delta` threads, past headings. It stops at either end.
    pub fn move_selection(&mut self, delta: isize) {
        let mut index = self.selected as isize;
        loop {
            index += delta;
            if index < 0 || index >= self.rows.len() as isize {
                return;
            }
            if self.rows[index as usize].thread().is_some() {
                self.selected = index as usize;
                return;
            }
        }
    }

    /// Highlights a thread's row. False when the list has no row for it.
    pub fn select(&mut self, id: &str) -> bool {
        let found = self.rows.iter().position(|row| row.thread() == Some(id));
        if let Some(index) = found {
            self.selected = index;
        }
        found.is_some()
    }

    /// The thread whose card or slim row the last frame drew on screen row `y`. It reads what
    /// was drawn rather than the current rows, so a click lands on the thread the reader saw
    /// even when the list has changed since.
    pub fn thread_at(&self, y: u16) -> Option<&str> {
        self.cards
            .iter()
            .find(|(top, bottom, _)| (*top..*bottom).contains(&y))
            .map(|(_, _, id)| id.as_str())
    }

    /// Whether the last frame drew a card that reads Working or Goal, whose spinner and clock
    /// need the event loop's tick. A card scrolled out of view, behind a closed shelf, left
    /// out of the sidebar or in a hidden sidebar has nothing on screen to move, so it doesn't
    /// count.
    pub fn drew_working(&self) -> bool {
        self.drew_working
    }

    /// Whether the last frame drew the feed's dot and the environment, which a hidden
    /// sidebar, or one too narrow for the dot, didn't.
    pub fn drew_environment(&self) -> bool {
        self.drew_environment
    }

    /// Whether the list has `shelf`'s heading and the shelf opens and closes, by its key and a
    /// click on the heading. Working has one while the shelf is on, and Done and Settled
    /// always, once the list has any thread.
    pub fn has_shelf(&self, shelf: Shelf) -> bool {
        self.listed
            && match shelf {
                Shelf::Working => self.choices.working_enabled,
                Shelf::Done | Shelf::Settled => true,
                Shelf::Pinned | Shelf::Inbox | Shelf::Snoozed => false,
            }
    }

    /// The shelf that folds whose heading the last frame drew on screen row `y`.
    pub fn shelf_at(&self, y: u16) -> Option<Shelf> {
        self.headings
            .iter()
            .find(|&&(row, _)| row == y)
            .map(|&(_, shelf)| shelf)
    }

    /// How many threads `shelf` holds, shown or not, for a shelf that folds.
    fn count(&self, shelf: Shelf) -> usize {
        match shelf {
            Shelf::Working => self.working_count,
            Shelf::Done => self.done_count,
            Shelf::Settled => self.settled_count,
            Shelf::Pinned | Shelf::Inbox | Shelf::Snoozed => 0,
        }
    }

    /// Opens or closes `shelf`, and returns whether it is open now. None, and nothing changes,
    /// when the list has no heading for it that opens and closes. The caller runs `rebuild`
    /// next. Closing a shelf moves the highlight and the top row off its threads to its
    /// heading first, so `rebuild` puts the highlight on the first thread below the heading
    /// and keeps the heading where it was. Otherwise both would land on whatever rows fill the
    /// old positions, which can be far down the list.
    ///
    /// The rows under the heading move, so the cards and headings the last frame drew take no
    /// click until the next frame draws them where they are now. A click before then on the
    /// list lands on nothing.
    pub fn toggle(&mut self, shelf: Shelf) -> Option<bool> {
        if !self.has_shelf(shelf) {
            return None;
        }
        let open = self.choices.open_mut(shelf)?;
        *open = !*open;
        let open = *open;
        self.cards.clear();
        self.headings.clear();
        let heading = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Heading { shelf: s, .. } if *s == shelf));
        if let Some(heading) = heading.filter(|_| !open) {
            let end = self.rows[heading + 1..]
                .iter()
                .position(|row| matches!(row, Row::Heading { .. }))
                .map_or(self.rows.len(), |next| heading + 1 + next);
            let threads = heading + 1..end;
            if threads.contains(&self.selected) {
                self.selected = heading;
            }
            if threads.contains(&self.offset) {
                self.offset = heading;
            }
        }
        Some(open)
    }

    /// Scrolls so the highlighted card fits in `height` rows, with the headings straight under
    /// it and its shelf's heading above it when those fit too, and so rows above fill any room
    /// left below the last row. The headings under the last card are the closed shelves at the
    /// end of the list, which come into view this way.
    fn scroll_into_view(&mut self, height: usize) {
        let Some(last) = self.rows.len().checked_sub(1) else {
            self.offset = 0;
            return;
        };
        let selected = self.selected.min(last);
        let under = self.rows[selected + 1..]
            .iter()
            .take_while(|row| matches!(row, Row::Heading { .. }))
            .count();
        let end = if fits(&self.rows[selected..=selected + under], height) {
            selected + under
        } else {
            selected
        };
        self.offset = self.offset.min(selected);
        while self.offset < selected && !fits(&self.rows[self.offset..=end], height) {
            self.offset += 1;
        }
        if self.offset > 0
            && matches!(self.rows[self.offset - 1], Row::Heading { .. })
            && fits(&self.rows[self.offset - 1..=end], height)
        {
            self.offset -= 1;
        }
        while self.offset > 0 && fits(&self.rows[self.offset - 1..], height) {
            self.offset -= 1;
        }
    }

    /// Forgets what the last frame drew, so nothing left from it takes clicks, the wheel or the
    /// tick. `draw` starts with it, and a frame that hides the sidebar calls it instead of
    /// `draw`. The rows, the highlight and the scroll stay, so the sidebar comes back as it
    /// was, brought up to date by any `rebuild` while it was hidden.
    pub fn forget_drawn(&mut self) {
        self.list = Rect::default();
        self.border = Rect::default();
        self.cards.clear();
        self.drew_working = false;
        self.headings.clear();
        self.drew_environment = false;
    }

    /// Draws the wordmark, the environment and the shelves, keeping the highlight in view.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, view: &View) {
        let t = view.theme;
        frame.render_widget(Block::new().style(Style::new().bg(t.sidebar_bg)), area);
        self.forget_drawn();
        // Too small to draw, so nothing there takes clicks.
        if area.width < 6 || area.height < 4 {
            return;
        }
        // A one-column strip stands in for the GUI's 1px border.
        self.border = Rect::new(area.right() - 1, area.y, 1, area.height);
        frame.render_widget(
            Block::new().style(Style::new().bg(t.sidebar_border)),
            self.border,
        );
        let inner = Rect::new(area.x + 1, area.y, area.width - 3, area.height);
        let width = inner.width as usize;
        // A screen with room keeps a blank row over the wordmark, level with the one over the
        // breadcrumb, and another between the environment and the list.
        let inset = super::top_inset(area.height);

        // The wordmark, and the dot for the thread feed with its word while it isn't live and
        // the row has room. A row too narrow for the dot leaves both to the breadcrumb's row.
        const WORDMARK: usize = "T3 Code".len();
        let dot = Style::new().fg(view.feed.dot(t));
        let word = view.feed.word();
        let mut right = Vec::with_capacity(3);
        if view.feed != Feed::Live && WORDMARK + word.width() + 3 <= width {
            right.extend([Span::styled(word, dot), Span::raw(" ")]);
        }
        right.push(Span::styled("●", dot));
        self.drew_environment = WORDMARK < width;
        let wordmark = row(
            vec![
                Span::styled(
                    "T3",
                    Style::new().fg(t.sidebar_fg).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" Code", Style::new().fg(t.sidebar_muted)),
            ],
            right,
            width,
        );
        frame.render_widget(
            Paragraph::new(wordmark),
            Rect::new(inner.x, inner.y + inset, inner.width, 1),
        );
        // The environment's name and address in the row under the wordmark, with the dot.
        if self.drew_environment {
            let label = Style::new().fg(t.sidebar_muted);
            let identity = view
                .environment
                .spans(width, label, Style::new().fg(t.muted));
            frame.render_widget(
                Paragraph::new(Line::from(identity)),
                Rect::new(inner.x, inner.y + inset + 1, inner.width, 1),
            );
        }

        let top = inner.y + 2 + 2 * inset;
        let list = Rect::new(inner.x, top, inner.width, inner.bottom() - top);
        self.list = list;
        let height = list.height as usize;
        self.scroll_into_view(height);
        // When the whole list fits, the shelves under the inbox sit at its bottom, as the GUI's
        // do, and the room left over goes above them.
        let room = if self.offset == 0 && fits(&self.rows, height) {
            height - self.rows.iter().map(Row::height).sum::<usize>()
        } else {
            0
        };
        let lower = if room > 0 {
            self.rows.iter().position(|row| {
                matches!(
                    row,
                    Row::Heading {
                        shelf: Shelf::Working | Shelf::Done | Shelf::Snoozed | Shelf::Settled,
                        ..
                    }
                )
            })
        } else {
            None
        };
        let mut lines: Vec<Line> = Vec::with_capacity(height);
        for (index, row) in self.rows.iter().enumerate().skip(self.offset) {
            if lines.len() >= height {
                break;
            }
            if lower == Some(index) {
                lines.resize(lines.len() + room, Line::default());
            }
            match row {
                Row::Heading { shelf, gap } => {
                    if *gap {
                        lines.push(Line::default());
                    }
                    if shelf.folds() && lines.len() < height {
                        self.headings.push((list.y + lines.len() as u16, *shelf));
                    }
                    let open = self.choices.open(*shelf);
                    lines.push(heading(*shelf, self.count(*shelf), open, width, t));
                }
                Row::Thread(id) | Row::Slim(id) => {
                    let slim = matches!(row, Row::Slim(_));
                    // A card starts no lower than the list's last line, so its first line,
                    // which has the word, is on screen even when the rest is cut off.
                    let top = list.y + lines.len() as u16;
                    let bottom = top + if slim { 1 } else { 3 };
                    self.cards
                        .push((top, bottom.min(list.bottom()), id.clone()));
                    let thread = view.shell.and_then(|shell| find_thread(shell, id));
                    self.drew_working |= thread.is_some_and(working);
                    let is_open = view.open_id == Some(id.as_str());
                    let cursor = index == self.selected;
                    let bg = if is_open {
                        Some(t.row_active)
                    } else if cursor {
                        Some(t.row_selected)
                    } else {
                        None
                    };
                    if slim {
                        lines.push(view.slim_row(thread, width, bg, cursor, is_open));
                    } else {
                        lines.extend(view.thread_card(thread, width, bg, cursor, is_open));
                        lines.push(Line::default());
                    }
                }
            }
        }
        lines.truncate(height);
        if lines.is_empty() {
            lines.push(Line::styled(
                empty_hint(view.shell),
                Style::new().fg(t.sidebar_muted),
            ));
        }
        frame.render_widget(Paragraph::new(lines), list);
    }
}

/// What an empty list says, after the GUI's empty state. A list with any thread, settled or
/// not, has the headings of the shelves that fold, which count them.
fn empty_hint(shell: Option<&ShellState>) -> &'static str {
    match shell {
        None => "Loading threads…",
        Some(shell) if shell.projects.is_empty() => "No projects yet",
        Some(_) => "No threads yet",
    }
}

/// A shelf's heading: its name and a rule out to the cards' right edge. A shelf that folds
/// adds its count and a chevron that points down while it is open, as the GUI's `Working (N)`
/// does. Snoozed takes the GUI's info tone.
fn heading(shelf: Shelf, count: usize, open: bool, width: usize, t: &Theme) -> Line<'static> {
    let color = if shelf == Shelf::Snoozed {
        t.info_fg
    } else {
        t.sidebar_muted
    };
    let (label, chevron) = if !shelf.folds() {
        (shelf.label().to_string(), "")
    } else if open {
        (format!("{} ({count})", shelf.label()), " ⌄")
    } else {
        (format!("{} ({count})", shelf.label()), " ›")
    };
    Line::from(vec![
        Span::styled(format!("{label} "), Style::new().fg(color)),
        Span::styled(
            "─".repeat(width.saturating_sub(label.width() + 2 + chevron.width())),
            Style::new().fg(t.sidebar_border),
        ),
        Span::styled(chevron, Style::new().fg(color)),
    ])
}

/// A thread from the shell, by id.
pub fn find_thread<'a>(shell: &'a ShellState, id: &str) -> Option<&'a Value> {
    shell.threads.iter().find(|t| str_of(t, "id") == id)
}

/// A project's title as T3 sent it, or `Project` when the shell doesn't have the project.
pub fn project_title<'a>(shell: Option<&'a ShellState>, project_id: &str) -> &'a str {
    shell
        .and_then(|s| s.projects.iter().find(|p| str_of(p, "id") == project_id))
        .map_or("Project", |p| str_of(p, "title"))
}

/// The badge of each project the shell has, by the project's id, worked out from its whole
/// title when the shell's feed brings the project and kept until the feed changes or removes
/// it. A frame looks a badge up, so neither a frame nor an event for a thread reads a title
/// that can be any length.
pub struct Badges {
    by_project: HashMap<String, Badge>,
    /// The badge of `Project`, which stands in for a project the shell doesn't have.
    missing: Badge,
    /// How many badges were worked out from a project's title, so the tests can tell what a
    /// frame or an event did.
    #[cfg(test)]
    pub prepares: usize,
}

impl Default for Badges {
    fn default() -> Badges {
        Badges {
            by_project: HashMap::new(),
            missing: Badge::of("Project"),
            #[cfg(test)]
            prepares: 0,
        }
    }
}

impl Badges {
    /// Follows what an item from the shell's feed did to `shell`'s projects. A snapshot works
    /// out every project's badge again, and an update or a removal that project's. Events for
    /// threads and the enrichment of a project's repository change no title, so they leave
    /// the badges as they are.
    pub fn note(&mut self, applied: &Applied, item: &Value, shell: &ShellState) {
        match applied {
            Applied::Snapshot => {
                self.by_project.clear();
                for project in &shell.projects {
                    // The shell finds the first project with an id, so the badge is its.
                    if !self.by_project.contains_key(str_of(project, "id")) {
                        self.prepare(project);
                    }
                }
            }
            Applied::Event(kind) if kind == "project.updated" || kind == "project.removed" => {
                let id = item
                    .pointer("/project/id")
                    .or_else(|| item.get("projectId"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match shell.projects.iter().find(|p| str_of(p, "id") == id) {
                    Some(project) => self.prepare(project),
                    None => {
                        self.by_project.remove(id);
                    }
                }
            }
            _ => {}
        }
    }

    /// Every project's badge, as a snapshot of `shell` would leave them.
    #[cfg(test)]
    pub fn of(shell: &ShellState) -> Badges {
        let mut badges = Badges::default();
        badges.note(&Applied::Snapshot, &Value::Null, shell);
        badges
    }

    fn prepare(&mut self, project: &Value) {
        let badge = Badge::of(str_of(project, "title"));
        self.by_project
            .insert(str_of(project, "id").to_string(), badge);
        #[cfg(test)]
        {
            self.prepares += 1;
        }
    }

    /// The badge of the project with id `project_id`, or the one for `Project` when the shell
    /// doesn't have it.
    pub fn get(&self, project_id: &str) -> &Badge {
        self.by_project.get(project_id).unwrap_or(&self.missing)
    }

    /// Whether the project with id `project_id` has a badge of its own.
    #[cfg(test)]
    pub fn has(&self, project_id: &str) -> bool {
        self.by_project.contains_key(project_id)
    }

    /// How many projects have a badge of their own.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.by_project.len()
    }
}

/// A title in the sidebar's text color, and bold for the open thread.
fn title_style(t: &Theme, open: bool) -> Style {
    let style = Style::new().fg(t.sidebar_fg);
    if open {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

/// What the sidebar needs from the rest of the app to draw.
pub struct View<'a> {
    pub theme: &'a Theme,
    pub shell: Option<&'a ShellState>,
    /// The badges of the shell's projects.
    pub badges: &'a Badges,
    pub open_id: Option<&'a str>,
    /// Whether the sidebar has the keyboard, which colors the highlight's bar.
    pub focused: bool,
    /// Wall-clock time of the frame, in milliseconds.
    pub now: i64,
    /// Where the thread feed stands, for the dot by the wordmark.
    pub feed: Feed,
    /// The environment, for the row under the wordmark.
    pub environment: &'a Identity,
}

impl View<'_> {
    /// Three lines like the GUI's thread card: project and status, title, branch and provider.
    /// The open thread's title is bold, so it shows without its background's color too.
    fn thread_card(
        &self,
        thread: Option<&Value>,
        width: usize,
        bg: Option<Color>,
        cursor: bool,
        open: bool,
    ) -> Vec<Line<'static>> {
        let t = self.theme;
        // The card borrows the titles and the branch as T3 sent them and reads only what it has
        // room for.
        let title = thread
            .map(|t| str_of(t, "title"))
            .filter(|s| !s.is_empty())
            .unwrap_or("Untitled");
        let unlisted;
        let (project, badge) = match thread {
            Some(thread) => {
                let id = str_of(thread, "projectId");
                (project_title(self.shell, id), self.badges.get(id))
            }
            None => {
                unlisted = Badge::of("");
                ("", &unlisted)
            }
        };
        let inner = width.saturating_sub(2);
        // The status keeps its width and the project name gives way, down to a one-column
        // ellipsis and one column of gap.
        let (status_label, status_color) =
            self.thread_status(thread, inner.saturating_sub(badge.letters.width() + 3));
        let branch = thread.map(|t| str_of(t, "branch")).unwrap_or("");
        let (glyph, glyph_color) = t.provider_glyph(
            thread
                .map(|t| str_of(&t["modelSelection"], "instanceId"))
                .unwrap_or(""),
        );
        let muted = Style::new().fg(t.sidebar_muted);
        let project_room = inner.saturating_sub(badge.letters.width() + status_label.width() + 2);
        let line1 = row(
            vec![
                Span::styled(
                    badge.letters.clone(),
                    Style::new()
                        .fg(t.project_color(badge))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {}", label::cut(project, project_room)), muted),
            ],
            vec![Span::styled(status_label, Style::new().fg(status_color))],
            inner,
        );
        let line2 = row(
            vec![Span::styled(label::cut(title, inner), title_style(t, open))],
            Vec::new(),
            inner,
        );
        let line3 = row(
            vec![Span::styled(
                label::cut(branch, inner.saturating_sub(2)),
                muted,
            )],
            vec![Span::styled(glyph, Style::new().fg(glyph_color))],
            inner,
        );
        [line1, line2, line3]
            .into_iter()
            .map(|line| self.framed(line, cursor, bg))
            .collect()
    }

    /// One line for a settled thread, after the GUI's slim row: the badge in the muted tone,
    /// the title, and at the right the card's word, or else how long ago the thread settled,
    /// which is what the shelf sorts by.
    fn slim_row(
        &self,
        thread: Option<&Value>,
        width: usize,
        bg: Option<Color>,
        cursor: bool,
        open: bool,
    ) -> Line<'static> {
        let t = self.theme;
        let muted = Style::new().fg(t.sidebar_muted);
        let title = thread
            .map(|t| str_of(t, "title"))
            .filter(|s| !s.is_empty())
            .unwrap_or("Untitled");
        let letters = thread
            .map(|thread| self.badges.get(str_of(thread, "projectId")).letters.clone())
            .unwrap_or_default();
        let inner = width.saturating_sub(2);
        // As on a card, the status keeps its width and the title gives way, down to a
        // one-column ellipsis and one column of gap.
        let room = inner.saturating_sub(letters.width() + 3);
        let (status, color) = match thread {
            Some(thread) if status_word(thread, self.now).is_none() => {
                let at = settled_ms(thread);
                let age = if at > 0 && room > 0 {
                    fit(&theme::age(at, self.now), room)
                } else {
                    String::new()
                };
                (age, t.sidebar_muted)
            }
            _ => self.thread_status(thread, room),
        };
        let title_room = inner.saturating_sub(letters.width() + status.width() + 2);
        let style = if open { title_style(t, true) } else { muted };
        let line = row(
            vec![
                Span::styled(format!("{letters} "), muted),
                Span::styled(label::cut(title, title_room), style),
            ],
            vec![Span::styled(status, Style::new().fg(color))],
            inner,
        );
        self.framed(line, cursor, bg)
    }

    /// `line` as a row of the list: the highlight's bar before it, a blank column after it,
    /// and `bg` behind both and behind each span of `line` that has no background of its own.
    fn framed(&self, line: Line<'static>, cursor: bool, bg: Option<Color>) -> Line<'static> {
        let t = self.theme;
        // The bar marks the highlight in the primary color while the sidebar has the
        // keyboard, and in the muted tone while it doesn't.
        let bar = match (cursor, self.focused) {
            (false, _) => Span::raw(" "),
            (true, true) => Span::styled("▎", Style::new().fg(t.primary)),
            (true, false) => Span::styled("▎", Style::new().fg(t.sidebar_muted)),
        };
        let mut spans = vec![bar];
        spans.extend(line.spans);
        spans.push(Span::raw(" "));
        match bg {
            Some(bg) => Line::from(with_bg(spans, bg)),
            None => Line::from(spans),
        }
    }

    /// The card's right-hand status in at most `room` columns: its word, with a spinner and
    /// the working clock while the agent works, or else the time of the latest activity. When
    /// the whole doesn't fit, the clock goes first, then the spinner, then the word is cut.
    fn thread_status(&self, thread: Option<&Value>, room: usize) -> (String, Color) {
        let t = self.theme;
        let Some(thread) = thread.filter(|_| room > 0) else {
            return (String::new(), t.sidebar_muted);
        };
        let Some(word) = status_word(thread, self.now) else {
            let stamp = [
                str_of(thread, "latestUserMessageAt"),
                str_of(thread, "updatedAt"),
            ]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or_default();
            return (fit(&relative_time(stamp, self.now), room), t.sidebar_muted);
        };
        let label = word.label();
        let mut choices = Vec::with_capacity(3);
        if word.working() {
            let spinner = spinner_frame(self.now);
            if let Some(since) = working_since(thread) {
                let clock = theme::working_label(self.now - since);
                choices.push(format!("{spinner} {label} {clock}"));
            }
            choices.push(format!("{spinner} {label}"));
        }
        choices.push(label.to_string());
        let text = choices
            .into_iter()
            .find(|choice| choice.width() <= room)
            .unwrap_or_else(|| fit(label, room));
        (text, word.color(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::{Buffer, Cell};
    use serde_json::json;

    use crate::tui::theme::Depth;

    const ALL: Capabilities = Capabilities {
        settlement: true,
        snooze: true,
    };

    /// The tests' clock: noon on 2026-10-08.
    fn now() -> i64 {
        parse_iso_ms("2026-10-08T12:00:00.000Z").unwrap()
    }

    fn ms(iso: &str) -> i64 {
        parse_iso_ms(iso).unwrap()
    }

    /// An idle top-level thread that has never run, with every field the sidebar reads.
    fn thread(id: &str) -> Value {
        json!({
            "id": id,
            "projectId": "p1",
            "title": format!("Thread {id}"),
            "branch": null,
            "modelSelection": {"instanceId": "codex"},
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
            "latestRunId": null,
            "activeProviderThreadId": null,
            "status": "idle",
            "pendingRuntimeRequest": null,
            "latestUserMessageAt": null,
            "createdAt": "2026-10-01T00:00:00.000Z",
            "updatedAt": "2026-10-01T00:00:00.000Z",
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
        })
    }

    fn with(mut thread: Value, fields: Value) -> Value {
        for (key, value) in fields.as_object().unwrap() {
            thread[key.as_str()] = value.clone();
        }
        thread
    }

    fn ids<'a>(threads: &[&'a Value]) -> Vec<&'a str> {
        threads.iter().map(|&t| id(t)).collect()
    }

    fn shell(threads: Vec<Value>) -> ShellState {
        ShellState {
            sequence: 1,
            projects: vec![json!({"id": "p1", "title": "Demo"})],
            threads,
            synchronized: true,
        }
    }

    fn heading_row(shelf: Shelf, gap: bool) -> Row {
        Row::Heading { shelf, gap }
    }

    fn card(id: &str) -> Row {
        Row::Thread(id.to_string())
    }

    /// Snoozed for an hour from 11:00, so asleep at noon.
    fn snoozed(id: &str) -> Value {
        with(
            thread(id),
            json!({"snoozedUntil": "2026-10-08T13:00:00.000Z", "snoozedAt": "2026-10-08T11:00:00.000Z"}),
        )
    }

    #[test]
    fn a_snooze_outranks_settlement_which_outranks_a_pin() {
        assert_eq!(shelf_for(true, true, true), Shelf::Snoozed);
        assert_eq!(shelf_for(false, true, true), Shelf::Settled);
        assert_eq!(shelf_for(false, false, true), Shelf::Pinned);
        assert_eq!(shelf_for(false, false, false), Shelf::Inbox);

        let everything = with(
            snoozed("t"),
            json!({"settledOverride": "settled", "pinnedAt": "2026-10-02T00:00:00.000Z"}),
        );
        assert_eq!(shelf(&everything, ALL, now()), Shelf::Snoozed);
        let woken = with(
            everything,
            json!({"snoozedUntil": "2026-10-08T11:30:00.000Z"}),
        );
        assert_eq!(shelf(&woken, ALL, now()), Shelf::Settled);
    }

    #[test]
    fn capabilities_gate_snooze_and_settlement_but_never_a_pin() {
        let everything = with(
            snoozed("t"),
            json!({"settledOverride": "settled", "pinnedAt": "2026-10-02T00:00:00.000Z"}),
        );
        let none = Capabilities::default();
        let settlement = Capabilities {
            settlement: true,
            snooze: false,
        };
        let snooze = Capabilities {
            settlement: false,
            snooze: true,
        };
        assert_eq!(shelf(&everything, none, now()), Shelf::Pinned);
        assert_eq!(shelf(&everything, settlement, now()), Shelf::Settled);
        assert_eq!(shelf(&everything, snooze, now()), Shelf::Snoozed);

        assert_eq!(
            Capabilities::of(&json!({"threadSettlement": true, "threadSnooze": "yes"})),
            settlement
        );
        assert_eq!(Capabilities::of(&Value::Null), none);
    }

    #[test]
    fn a_settled_at_stamp_alone_does_not_settle_a_thread() {
        let stamped = with(
            thread("t"),
            json!({"settledAt": "2026-10-05T00:00:00.000Z"}),
        );
        assert_eq!(shelf(&stamped, ALL, now()), Shelf::Inbox);
        let reopened = with(stamped, json!({"settledOverride": "active"}));
        assert_eq!(shelf(&reopened, ALL, now()), Shelf::Inbox);
    }

    #[test]
    fn pinned_threads_follow_their_keys_then_newest_created() {
        let pinned = |id: &str, fields: Value| {
            with(
                with(thread(id), json!({"pinnedAt": "2026-10-02T00:00:00.000Z"})),
                fields,
            )
        };
        let threads = vec![
            pinned("n1", json!({"createdAt": "2026-10-03T00:00:00.000Z"})),
            pinned("k1", json!({"pinOrderKey": "m"})),
            pinned("n3", json!({"createdAt": "not a time"})),
            pinned("k3", json!({"pinOrderKey": "c"})),
            pinned("n2", json!({"createdAt": "2026-10-05T00:00:00.000Z"})),
            pinned("k2", json!({"pinOrderKey": "c"})),
        ];
        // Capabilities don't change the pinned order.
        for capabilities in [ALL, Capabilities::default()] {
            let shelves = shelves(&threads, capabilities, now(), None);
            assert_eq!(ids(&shelves.pinned), ["k2", "k3", "k1", "n2", "n1", "n3"]);
        }
    }

    #[test]
    fn active_threads_lead_with_new_and_reopened_then_arranged() {
        let threads = vec![
            with(
                thread("a1"),
                json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
            ),
            with(thread("k1"), json!({"activeOrderKey": "b"})),
            with(
                thread("a2"),
                json!({"createdAt": "2026-10-01T00:00:00.000Z", "unsettledAt": "2026-10-06T00:00:00.000Z"}),
            ),
            with(
                thread("a4"),
                json!({"createdAt": "2026-10-04T00:00:00.000Z"}),
            ),
            with(thread("k2"), json!({"activeOrderKey": "a"})),
            with(
                thread("a3"),
                json!({"createdAt": "2026-10-04T00:00:00.000Z"}),
            ),
        ];
        let shelves = shelves(&threads, ALL, now(), None);
        assert_eq!(ids(&shelves.inbox), ["a2", "a3", "a4", "a1", "k2", "k1"]);
    }

    #[test]
    fn settled_threads_sort_by_when_they_settled_with_fallbacks() {
        let settled = |id: &str, fields: Value| {
            with(
                with(thread(id), json!({"settledOverride": "settled"})),
                fields,
            )
        };
        let threads = vec![
            settled(
                "s3",
                json!({"settledAt": null, "updatedAt": "2026-10-04T00:00:00.000Z"}),
            ),
            settled("s5", json!({"settledAt": "2026-10-05T00:00:00.000Z"})),
            settled(
                "s2",
                json!({
                    "settledAt": "soon",
                    "latestUserMessageAt": "2026-10-03T00:00:00.000Z",
                    "latestRunId": "r",
                    "latestRunCompletedAt": "2026-10-06T00:00:00.000Z",
                }),
            ),
            settled("s4", json!({"updatedAt": "bad", "createdAt": "bad"})),
            settled("s1", json!({"settledAt": "2026-10-05T00:00:00.000Z"})),
        ];
        let shelves = shelves(&threads, ALL, now(), None);
        assert_eq!(ids(&shelves.settled), ["s2", "s1", "s5", "s3", "s4"]);
        assert_eq!(settled_ms(&threads[2]), ms("2026-10-06T00:00:00.000Z"));
        assert_eq!(settled_ms(&threads[3]), 0);
    }

    #[test]
    fn timestamps_parse_only_in_the_form_t3_writes() {
        let at = |text: &str| instant(&json!(text));
        assert_eq!(
            at("2026-10-08T12:00:00.000Z"),
            Some(ms("2026-10-08T12:00:00.000Z"))
        );
        assert_eq!(at("2026-10-08T12:00:00Z"), Some(now()));
        assert_eq!(at("2026-10-08T12:00:00.5Z"), Some(now() + 500));
        for bad in [
            "",
            "tomorrow",
            "2026-10-08",
            "2026-10-08 12:00:00Z",
            "2026-10-08T12:00:00",
            "2026-10-08T12:00:00.Z",
            "2026-10-08T12:00:00+02:00",
            "2026-13-08T12:00:00Z",
            "2026-10-32T12:00:00Z",
            "2026-10-08T24:00:00Z",
            "2026-10-08T12:60:00Z",
        ] {
            assert_eq!(at(bad), None, "{bad}");
        }
        assert_eq!(instant(&json!(12345)), None);
        assert_eq!(instant(&Value::Null), None);
    }

    #[test]
    fn a_snooze_ends_at_its_wake_time() {
        let thread = with(
            snoozed("t"),
            json!({"snoozedUntil": "2026-10-08T12:00:30.000Z"}),
        );
        let wake = ms("2026-10-08T12:00:30.000Z");
        assert!(effective_snoozed(&thread, now()));
        assert!(effective_snoozed(&thread, wake - 1));
        assert!(!effective_snoozed(&thread, wake));
        assert_eq!(shelf(&thread, ALL, wake), Shelf::Inbox);
    }

    #[test]
    fn a_malformed_or_missing_wake_time_never_hides_a_thread() {
        for wake in [
            json!("tomorrow"),
            json!("2026-13-01T00:00:00Z"),
            json!("2026-10-08 13:00:00Z"),
            json!(12345),
            Value::Null,
        ] {
            let thread = with(snoozed("t"), json!({"snoozedUntil": wake.clone()}));
            assert!(!effective_snoozed(&thread, now()), "{wake}");
            assert_eq!(shelf(&thread, ALL, now()), Shelf::Inbox, "{wake}");
        }
        let mut missing = snoozed("t");
        missing.as_object_mut().unwrap().remove("snoozedUntil");
        assert!(!effective_snoozed(&missing, now()));
    }

    #[test]
    fn a_raised_hand_wakes_a_snooze_early() {
        let woke = |fields: Value| !effective_snoozed(&with(snoozed("t"), fields), now());
        assert!(!woke(json!({})));

        // The agent waits on the user.
        assert!(woke(json!({"pendingRuntimeRequest": {"kind": "approval"}})));
        assert!(woke(
            json!({"pendingRuntimeRequest": {"kind": "user_input"}})
        ));
        assert!(!woke(
            json!({"pendingRuntimeRequest": {"kind": "auth_refresh"}})
        ));

        // A failure counts when it is newer than the snooze.
        let failed =
            |updated: &str| json!({"latestRunId": "r", "status": "failed", "updatedAt": updated});
        assert!(woke(failed("2026-10-08T11:30:00.000Z")));
        assert!(!woke(failed("2026-10-08T10:30:00.000Z")));
        assert!(woke(with(
            failed("2026-10-08T10:30:00.000Z"),
            json!({"snoozedAt": null})
        )));
        // Without a run or a provider thread there is no runtime to fail.
        assert!(!woke(
            json!({"status": "failed", "updatedAt": "2026-10-08T11:30:00.000Z"})
        ));
        // A live activity run outranks the thread's failed status.
        assert!(!woke(with(
            failed("2026-10-08T11:30:00.000Z"),
            json!({"activityRunStatus": "running"})
        )));

        // A run that completed after the snooze began.
        let completed =
            |at: &str| json!({"latestRunId": "r", "status": "idle", "latestRunCompletedAt": at});
        assert!(woke(completed("2026-10-08T11:30:00.000Z")));
        assert!(!woke(completed("2026-10-08T10:30:00.000Z")));
        assert!(!woke(with(
            completed("2026-10-08T11:30:00.000Z"),
            json!({"status": "running"})
        )));
        // A server that leaves out the completion time has it read from `updatedAt`.
        assert!(woke(json!({
            "latestRunId": "r",
            "status": "completed",
            "updatedAt": "2026-10-08T11:30:00.000Z",
        })));
        // Without `snoozedAt`, a completion can't be placed after the snooze.
        assert!(!woke(with(
            completed("2026-10-08T11:30:00.000Z"),
            json!({"snoozedAt": null})
        )));
    }

    #[test]
    fn the_soonest_wake_comes_first_and_sets_the_timer() {
        let wake = |id: &str, at: &str| with(snoozed(id), json!({"snoozedUntil": at}));
        let threads = vec![
            wake("z1", "2026-10-08T14:00:00.000Z"),
            wake("z2", "2026-10-08T12:30:00.000Z"),
            wake("z3", "2026-10-08T13:00:00.000Z"),
        ];
        assert_eq!(
            ids(&shelves(&threads, ALL, now(), None).snoozed),
            ["z2", "z3", "z1"]
        );

        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(sidebar.next_wake, Some(ms("2026-10-08T12:30:00.000Z")));
        sidebar.rebuild(&threads, ALL, None, ms("2026-10-08T12:30:00.000Z"));
        assert_eq!(sidebar.next_wake, Some(ms("2026-10-08T13:00:00.000Z")));
        // Without snooze support nothing is snoozed, so nothing needs a timer.
        sidebar.rebuild(&threads, Capabilities::default(), None, now());
        assert_eq!(sidebar.next_wake, None);
    }

    #[test]
    fn a_snooze_wakes_without_a_server_event() {
        let threads = vec![
            with(
                snoozed("z"),
                json!({"snoozedUntil": "2026-10-08T12:00:30.000Z"}),
            ),
            thread("a"),
        ];
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(
            sidebar.rows,
            [
                card("a"),
                heading_row(Shelf::Working, true),
                heading_row(Shelf::Done, false),
                heading_row(Shelf::Snoozed, false),
                card("z"),
                heading_row(Shelf::Settled, true),
            ]
        );
        // The same shell, read again when the timer fires. Neither thread has come back from
        // anything, so they tie and go by id.
        sidebar.rebuild(&threads, ALL, None, ms("2026-10-08T12:00:30.050Z"));
        assert_eq!(
            sidebar.rows,
            [
                card("a"),
                card("z"),
                heading_row(Shelf::Working, true),
                heading_row(Shelf::Done, false),
                heading_row(Shelf::Settled, false),
            ]
        );
        assert_eq!(sidebar.next_wake, None);
    }

    #[test]
    fn archived_threads_and_subagents_hide_but_forks_stay() {
        let archived = with(
            thread("archived"),
            json!({"archivedAt": "2026-10-05T00:00:00.000Z"}),
        );
        let subagent = with(
            thread("subagent"),
            json!({"lineage": {"parentThreadId": "a", "relationshipToParent": "subagent", "rootThreadId": "a"}}),
        );
        let fork = with(
            thread("fork"),
            json!({"lineage": {"parentThreadId": "a", "relationshipToParent": "fork", "rootThreadId": "a"}}),
        );
        assert!(!visible(&archived));
        assert!(!visible(&subagent));
        assert!(visible(&fork));
        let threads = vec![thread("a"), archived, subagent, fork];
        let shelves = shelves(&threads, ALL, now(), None);
        let mut shown = ids(&shelves.inbox);
        shown.sort();
        assert_eq!(shown, ["a", "fork"]);
    }

    #[test]
    fn a_heading_leads_each_shelf_that_has_cards() {
        let threads = vec![
            with(thread("s"), json!({"settledOverride": "settled"})),
            snoozed("z"),
            thread("a"),
            with(thread("p"), json!({"pinnedAt": "2026-10-02T00:00:00.000Z"})),
        ];
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        // The inbox's heading marks the end of the pins. Working, Done and Settled keep theirs
        // while empty or closed, and headings with nothing between them stack without a gap.
        assert_eq!(
            sidebar.rows,
            [
                heading_row(Shelf::Pinned, false),
                card("p"),
                heading_row(Shelf::Inbox, true),
                card("a"),
                heading_row(Shelf::Working, true),
                heading_row(Shelf::Done, false),
                heading_row(Shelf::Snoozed, false),
                card("z"),
                heading_row(Shelf::Settled, true),
            ]
        );
        assert_eq!(
            (
                sidebar.working_count,
                sidebar.done_count,
                sidebar.settled_count
            ),
            (0, 0, 1)
        );
        // An open Settled shelf lists its threads as slim rows.
        sidebar.choices.settled_open = true;
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(
            sidebar.rows[8..],
            [heading_row(Shelf::Settled, true), Row::Slim("s".into())]
        );

        // Without pins the inbox needs no heading.
        sidebar.rebuild(&[thread("a")], ALL, None, now());
        assert_eq!(
            sidebar.rows,
            [
                card("a"),
                heading_row(Shelf::Working, true),
                heading_row(Shelf::Done, false),
                heading_row(Shelf::Settled, false),
            ]
        );
        // With the Working shelf off it has no heading either.
        sidebar.choices.working_enabled = false;
        sidebar.rebuild(&[thread("a")], ALL, None, now());
        assert_eq!(
            sidebar.rows,
            [
                card("a"),
                heading_row(Shelf::Done, true),
                heading_row(Shelf::Settled, false),
            ]
        );
        // An empty list has no headings at all.
        sidebar.rebuild(&[], ALL, None, now());
        assert!(sidebar.rows.is_empty());
        assert_eq!(sidebar.selected_thread_id(), None);
        assert!(!sidebar.has_shelf(Shelf::Done) && !sidebar.has_shelf(Shelf::Settled));
    }

    #[test]
    fn the_open_thread_stays_listed_on_a_closed_settled_shelf() {
        let threads = vec![
            thread("a"),
            with(thread("s1"), json!({"settledOverride": "settled"})),
            with(thread("s2"), json!({"settledOverride": "settled"})),
        ];
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, Some("s2"), now());
        assert_eq!(
            sidebar.rows,
            [
                card("a"),
                heading_row(Shelf::Working, true),
                heading_row(Shelf::Done, false),
                heading_row(Shelf::Settled, false),
                Row::Slim("s2".into()),
            ]
        );
        assert_eq!(sidebar.settled_count, 2);
    }

    #[test]
    fn the_highlight_follows_its_thread_when_rows_move() {
        let a = with(
            thread("a"),
            json!({"createdAt": "2026-10-03T00:00:00.000Z"}),
        );
        let b = with(
            thread("b"),
            json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
        );
        let c = with(
            thread("c"),
            json!({"createdAt": "2026-10-01T00:00:00.000Z"}),
        );
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&[a.clone(), b.clone(), c.clone()], ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("a"));
        assert!(sidebar.select("b"));
        assert!(!sidebar.select("missing"));

        // Pinning `c` adds two rows above `b`: the Pinned heading and its card.
        let pinned = with(c, json!({"pinnedAt": "2026-10-08T11:00:00.000Z"}));
        sidebar.rebuild(&[a.clone(), b, pinned.clone()], ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("b"));
        assert_eq!(sidebar.selected, 4);

        // When `b` goes, the highlight takes the last card, since none is below.
        sidebar.rebuild(&[a, pinned], ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("a"));
    }

    #[test]
    fn the_highlight_takes_the_next_card_when_its_thread_goes() {
        let threads: Vec<Value> = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(i, id)| {
                with(
                    thread(id),
                    json!({"createdAt": format!("2026-10-0{}T00:00:00.000Z", 3 - i)}),
                )
            })
            .collect();
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert!(sidebar.select("b"));
        sidebar.rebuild(&[threads[0].clone(), threads[2].clone()], ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("c"));
    }

    #[test]
    fn navigation_skips_headings_and_stops_at_the_ends() {
        let threads = vec![
            with(thread("p"), json!({"pinnedAt": "2026-10-02T00:00:00.000Z"})),
            with(
                thread("a1"),
                json!({"createdAt": "2026-10-03T00:00:00.000Z"}),
            ),
            with(
                thread("a2"),
                json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
            ),
        ];
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("p"));
        sidebar.move_selection(-1);
        assert_eq!(sidebar.selected_thread_id(), Some("p"));
        sidebar.move_selection(1);
        assert_eq!(sidebar.selected_thread_id(), Some("a1"));
        sidebar.move_selection(1);
        sidebar.move_selection(1);
        assert_eq!(sidebar.selected_thread_id(), Some("a2"));
        sidebar.move_selection(-1);
        sidebar.move_selection(-1);
        assert_eq!(sidebar.selected_thread_id(), Some("p"));
    }

    // ---- status words ----

    /// The word a thread made of `fields` shows at noon.
    fn word_of(fields: Value) -> Option<Word> {
        status_word(&with(thread("t"), fields), now())
    }

    /// The fields of a thread whose run started at 11:57 and is still going, as a nightly
    /// server sends them.
    fn running() -> Value {
        json!({
            "latestRunId": "r",
            "activeRunId": "r",
            "status": "running",
            "latestRunRequestedAt": "2026-10-08T11:56:50.000Z",
            "latestRunStartedAt": "2026-10-08T11:57:00.000Z",
            "latestRunCompletedAt": null,
            "activityRunStatus": "running",
            "activityRunStartedAt": "2026-10-08T11:57:00.000Z",
        })
    }

    /// The fields of a thread whose run finished at 11:00 and was last visited at `visited`.
    fn finished(visited: Value) -> Value {
        json!({
            "latestRunId": "r",
            "status": "completed",
            "latestRunCompletedAt": "2026-10-08T11:00:00.000Z",
            "lastVisitedAt": visited,
        })
    }

    fn without(mut thread: Value, key: &str) -> Value {
        thread.as_object_mut().unwrap().remove(key);
        thread
    }

    #[test]
    fn a_request_for_the_user_outranks_what_the_agent_is_doing() {
        let asking =
            |kind: Value| with(running(), json!({"pendingRuntimeRequest": {"kind": kind}}));
        // The shell carries one request, so Approval and Input are never pending together.
        // Every kind but a question is an approval, including kinds this build doesn't know.
        for kind in [
            "command",
            "file-read",
            "file-change",
            "mcp-elicitation",
            "permission",
            "dynamic_tool_call",
            "something_new",
        ] {
            assert_eq!(word_of(asking(json!(kind))), Some(Word::Approval), "{kind}");
        }
        assert_eq!(word_of(asking(json!("user_input"))), Some(Word::Input));
        assert_eq!(
            word_of(json!({"pendingRuntimeRequest": {"id": "q"}})),
            Some(Word::Approval),
            "a request without a kind still waits on the user"
        );

        // An auth refresh isn't the user's to answer, so the runtime's word shows instead.
        assert_eq!(word_of(asking(json!("auth_refresh"))), Some(Word::Working));
        assert_eq!(
            word_of(json!({"pendingRuntimeRequest": {"kind": "auth_refresh"}})),
            None
        );

        // A request outranks a failure, a goal and a wake.
        let failed = json!({"latestRunId": "r", "status": "failed", "pendingRuntimeRequest": {"kind": "permission"}});
        assert_eq!(word_of(failed), Some(Word::Approval));
        let goal = with(
            asking(json!("user_input")),
            json!({"goal": {"objective": "Ship it", "status": "active"}}),
        );
        assert_eq!(word_of(goal), Some(Word::Input));
        let woken = json!({
            "snoozedAt": "2026-10-08T10:00:00.000Z",
            "snoozedUntil": "2026-10-08T11:30:00.000Z",
            "pendingRuntimeRequest": {"kind": "user_input"},
        });
        assert_eq!(word_of(woken), Some(Word::Input));
    }

    #[test]
    fn the_runtime_decides_working_waiting_limited_and_failed() {
        for state in ["preparing", "queued", "starting", "running", "waiting"] {
            assert_eq!(
                word_of(json!({"latestRunId": "r", "status": state})),
                Some(Word::Working),
                "{state}"
            );
        }
        // The activity run outranks the thread's status, as when a wake continues the work.
        assert_eq!(
            word_of(
                json!({"latestRunId": "r", "status": "completed", "activityRunStatus": "starting"})
            ),
            Some(Word::Working)
        );

        // Background work that will wake the agent parks it at Waiting. A command it left
        // running, such as a dev server, doesn't.
        let background = |kinds: &[&str]| {
            let tasks: Vec<Value> = kinds
                .iter()
                .map(|kind| json!({"taskId": format!("task-{kind}"), "kind": kind}))
                .collect();
            json!({"latestRunId": "r", "status": "completed", "pendingBackgroundTasks": tasks})
        };
        for kinds in [
            &["subagent"][..],
            &["monitor"],
            &["background_task"],
            &["kind_from_the_future"],
            &["command", "subagent"],
        ] {
            assert_eq!(word_of(background(kinds)), Some(Word::Waiting), "{kinds:?}");
        }
        assert_eq!(word_of(background(&["command"])), None);
        assert_eq!(word_of(background(&[])), None);
        // Work that holds a thread can hold one that never ran.
        let unrun = json!({"pendingBackgroundTasks": [{"taskId": "w", "kind": "monitor"}]});
        assert_eq!(word_of(unrun), Some(Word::Waiting));

        // A usage limit is Limited. Every other failure, or one with no class, is Failed.
        let failed =
            |class: Value| json!({"latestRunId": "r", "status": "failed", "lastErrorClass": class});
        assert_eq!(word_of(failed(json!("usage_limit"))), Some(Word::Limited));
        for class in [
            json!("provider_error"),
            json!("transport_error"),
            json!("permission_error"),
            json!("validation_error"),
            json!("unknown"),
            Value::Null,
        ] {
            assert_eq!(
                word_of(failed(class.clone())),
                Some(Word::Failed),
                "{class}"
            );
        }
        assert_eq!(
            word_of(without(failed(Value::Null), "lastErrorClass")),
            Some(Word::Failed)
        );
        // A failure outranks the background roster, so it stays visible.
        let held = with(
            failed(json!("usage_limit")),
            json!({"pendingBackgroundTasks": [{"taskId": "s", "kind": "subagent"}]}),
        );
        assert_eq!(word_of(held), Some(Word::Limited));
        // A live activity run outranks the thread's failed status.
        assert_eq!(
            word_of(with(
                failed(json!("usage_limit")),
                json!({"activityRunStatus": "running"})
            )),
            Some(Word::Working)
        );
        // Without a run or a provider thread there is no runtime, so nothing failed.
        assert_eq!(
            word_of(json!({"status": "failed", "lastErrorClass": "usage_limit"})),
            None
        );

        // A run that ended for any other reason leaves the thread at rest.
        for state in [
            "completed",
            "interrupted",
            "cancelled",
            "rolled_back",
            "nonsense",
        ] {
            assert_eq!(
                word_of(json!({"latestRunId": "r", "status": state})),
                None,
                "{state}"
            );
        }
        assert_eq!(word_of(json!({"latestRunId": "r", "status": 7})), None);
        assert_eq!(word_of(json!({})), None, "a thread that never ran");
        // The server sends `idle` only before the first run. The GUI reads it as Waiting
        // whenever a runtime exists, as it does for a provider thread with no run yet.
        assert_eq!(
            word_of(json!({"activeProviderThreadId": "pt"})),
            Some(Word::Waiting)
        );
        assert_eq!(
            word_of(json!({"latestRunId": "r", "status": "idle"})),
            Some(Word::Waiting)
        );
    }

    #[test]
    fn working_reads_goal_only_while_the_goal_is_active() {
        let goal = |status: &str| json!({"goal": {"objective": "Ship it", "status": status}});
        assert_eq!(word_of(with(running(), goal("active"))), Some(Word::Goal));
        for status in [
            "paused",
            "blocked",
            "usage_limited",
            "budget_limited",
            "complete",
        ] {
            assert_eq!(
                word_of(with(running(), goal(status))),
                Some(Word::Working),
                "{status}"
            );
        }
        // A server without goals leaves the field out, and a malformed one is no goal.
        assert_eq!(word_of(running()), Some(Word::Working));
        assert_eq!(
            word_of(with(running(), json!({"goal": null}))),
            Some(Word::Working)
        );
        assert_eq!(
            word_of(with(running(), json!({"goal": "active"}))),
            Some(Word::Working)
        );
        // An active goal on a thread at rest adds no word.
        assert_eq!(
            word_of(with(
                json!({"latestRunId": "r", "status": "completed"}),
                goal("active")
            )),
            None
        );
    }

    #[test]
    fn done_is_a_finish_nobody_has_seen() {
        assert_eq!(
            word_of(finished(json!("2026-10-08T10:00:00.000Z"))),
            Some(Word::Done)
        );
        assert_eq!(
            word_of(finished(json!("2026-10-08T11:00:00.000Z"))),
            None,
            "seen as it finished"
        );
        assert_eq!(word_of(finished(json!("2026-10-08T11:30:00.000Z"))), None);

        // A thread nobody has visited counts as seen, so a server doesn't mark its whole
        // history unread. A server without visit tracking leaves the field out, and then
        // nothing is Done.
        assert_eq!(word_of(finished(Value::Null)), None);
        assert_eq!(word_of(finished(json!(""))), None);
        assert_eq!(
            word_of(without(finished(Value::Null), "lastVisitedAt")),
            None
        );
        // A visit time that doesn't parse can't show the finish was seen.
        assert_eq!(word_of(finished(json!("yesterday"))), Some(Word::Done));

        // No finish time that parses, or no run, means nothing finished.
        let visited = json!("2026-10-08T10:00:00.000Z");
        assert_eq!(
            word_of(with(
                finished(visited.clone()),
                json!({"latestRunCompletedAt": "soon"})
            )),
            None
        );
        assert_eq!(
            word_of(with(
                finished(visited.clone()),
                json!({"latestRunCompletedAt": null})
            )),
            None
        );
        assert_eq!(
            word_of(json!({"lastVisitedAt": "2026-10-08T10:00:00.000Z"})),
            None
        );
        // A server that leaves out the finish time has it read from `updatedAt`.
        let implied = with(
            without(finished(visited.clone()), "latestRunCompletedAt"),
            json!({"updatedAt": "2026-10-08T11:00:00.000Z"}),
        );
        assert_eq!(word_of(implied), Some(Word::Done));
        // An interrupted or cancelled run finished too.
        for state in ["interrupted", "cancelled"] {
            assert_eq!(
                word_of(with(finished(visited.clone()), json!({"status": state}))),
                Some(Word::Done),
                "{state}"
            );
        }
        // What the thread is doing outranks an unseen finish.
        assert_eq!(
            word_of(with(finished(visited.clone()), json!({"status": "failed"}))),
            Some(Word::Failed)
        );
        assert_eq!(
            word_of(with(
                finished(visited),
                json!({"activityRunStatus": "running"})
            )),
            Some(Word::Working)
        );
    }

    #[test]
    fn a_thread_moves_through_its_words_as_the_shell_changes() {
        let mut t = with(
            thread("t"),
            json!({"lastVisitedAt": "2026-10-08T09:00:00.000Z"}),
        );
        let mut step = |fields: Value| {
            t = with(t.clone(), fields);
            status_word(&t, now())
        };
        assert_eq!(step(json!({})), None, "never ran");
        let queued = json!({
            "latestRunId": "r",
            "activeRunId": "r",
            "status": "queued",
            "latestRunRequestedAt": "2026-10-08T11:58:00.000Z",
            "latestRunCompletedAt": null,
        });
        assert_eq!(step(queued), Some(Word::Working));
        assert_eq!(
            step(json!({"status": "running", "pendingRuntimeRequest": {"kind": "command"}})),
            Some(Word::Approval)
        );
        assert_eq!(
            step(json!({"pendingRuntimeRequest": null})),
            Some(Word::Working)
        );
        // The run ends while a subagent it started is still out.
        let ended = json!({
            "status": "completed",
            "activeRunId": null,
            "latestRunCompletedAt": "2026-10-08T11:59:00.000Z",
            "pendingBackgroundTasks": [{"taskId": "s", "kind": "subagent"}],
        });
        assert_eq!(step(ended), Some(Word::Waiting));
        assert_eq!(
            step(json!({"pendingBackgroundTasks": []})),
            Some(Word::Done)
        );
        // Another client opens the thread.
        assert_eq!(
            step(json!({"lastVisitedAt": "2026-10-08T11:59:30.000Z"})),
            None
        );
    }

    #[test]
    fn woke_shows_until_a_visit_after_the_wake() {
        // Snoozed at 10:00 until 11:30, so its timer woke it half an hour before noon.
        let slept = |fields: Value| {
            with(
                json!({
                    "snoozedAt": "2026-10-08T10:00:00.000Z",
                    "snoozedUntil": "2026-10-08T11:30:00.000Z",
                }),
                fields,
            )
        };
        let visited = |at: Value| slept(json!({"lastVisitedAt": at}));
        assert_eq!(word_of(slept(json!({}))), Some(Word::Woke), "never visited");
        assert_eq!(word_of(visited(Value::Null)), Some(Word::Woke));
        // The GUI's visit records the thread's `updatedAt`, which a timer wake doesn't move,
        // so a visit after a timer wake still lands before it.
        assert_eq!(
            word_of(visited(json!("2026-10-08T11:00:00.000Z"))),
            Some(Word::Woke),
            "visited before the wake"
        );
        // Dismissing Woke in the GUI records a visit at the wake time.
        assert_eq!(word_of(visited(json!("2026-10-08T11:30:00.000Z"))), None);
        assert_eq!(word_of(visited(json!("2026-10-08T11:45:00.000Z"))), None);
        assert_eq!(
            word_of(visited(json!("not a time"))),
            Some(Word::Woke),
            "a visit time that doesn't parse is no visit"
        );

        // A settled thread isn't Woke. Any other override is.
        assert_eq!(word_of(slept(json!({"settledOverride": "settled"}))), None);
        assert_eq!(
            word_of(slept(json!({"settledOverride": "active"}))),
            Some(Word::Woke)
        );
        // A wake time that is missing or doesn't parse never woke anything.
        assert_eq!(word_of(slept(json!({"snoozedUntil": "soon"}))), None);
        assert_eq!(word_of(slept(json!({"snoozedUntil": null}))), None);
        assert_eq!(word_of(without(slept(json!({})), "snoozedUntil")), None);

        // Woke outranks an unseen finish, and what the thread is doing outranks Woke.
        let unseen = finished(json!("2026-10-08T08:00:00.000Z"));
        assert_eq!(
            word_of(with(
                unseen.clone(),
                json!({"latestRunCompletedAt": "2026-10-08T09:00:00.000Z"})
            )),
            Some(Word::Done)
        );
        assert_eq!(
            word_of(slept(with(
                unseen,
                json!({"latestRunCompletedAt": "2026-10-08T09:00:00.000Z"})
            ))),
            Some(Word::Woke)
        );
        assert_eq!(word_of(slept(running())), Some(Word::Working));
    }

    #[test]
    fn a_timer_wake_shows_woke_from_the_wake_time() {
        let t = with(
            snoozed("t"),
            json!({"snoozedUntil": "2026-10-08T12:00:30.000Z"}),
        );
        let wake = ms("2026-10-08T12:00:30.000Z");
        assert_eq!(status_word(&t, wake - 1), None, "still asleep");
        assert_eq!(status_word(&t, wake), Some(Word::Woke));
        assert_eq!(woke_at(&t, wake), Some(&t["snoozedUntil"]));
    }

    #[test]
    fn an_early_wake_dates_from_when_the_hand_went_up() {
        // Snoozed at 11:00 until 13:00. A run finished at 11:30 and woke it early.
        let early = with(
            snoozed("t"),
            json!({"latestRunId": "r", "status": "completed", "latestRunCompletedAt": "2026-10-08T11:30:00.000Z"}),
        );
        let after_wake = ms("2026-10-08T14:00:00.000Z");
        assert_eq!(status_word(&early, now()), Some(Word::Woke));
        assert_eq!(
            woke_at(&early, now()),
            Some(&json!("2026-10-08T11:30:00.000Z"))
        );
        let visited = |at: &str| with(early.clone(), json!({"lastVisitedAt": at}));
        assert_eq!(
            status_word(&visited("2026-10-08T11:15:00.000Z"), now()),
            Some(Word::Woke),
            "a visit before the early wake doesn't count"
        );
        let seen = visited("2026-10-08T11:45:00.000Z");
        assert_eq!(status_word(&seen, now()), None);
        // The wake still dates from 11:30 once 13:00 passes, so the 11:45 visit still counts.
        assert_eq!(status_word(&seen, after_wake), None);
        assert_eq!(
            woke_at(&seen, after_wake),
            Some(&json!("2026-10-08T11:30:00.000Z"))
        );

        // A fresh failure woke it at the runtime's last update. The card says Failed.
        let failed = with(
            snoozed("t"),
            json!({"latestRunId": "r", "status": "failed", "updatedAt": "2026-10-08T11:40:00.000Z"}),
        );
        assert_eq!(
            woke_at(&failed, now()),
            Some(&json!("2026-10-08T11:40:00.000Z"))
        );
        assert_eq!(status_word(&failed, now()), Some(Word::Failed));
        // A request woke a thread with no runtime at the snooze's start.
        let asked = with(
            snoozed("t"),
            json!({"pendingRuntimeRequest": {"kind": "user_input"}}),
        );
        assert_eq!(
            woke_at(&asked, now()),
            Some(&json!("2026-10-08T11:00:00.000Z"))
        );
        // Once answered, the hand is down and the thread sleeps until 13:00 again.
        let answered = with(asked, json!({"pendingRuntimeRequest": null}));
        assert_eq!(woke_at(&answered, now()), None);
        assert_eq!(status_word(&answered, now()), None);
        // An auth refresh never raised a hand.
        let refreshing = with(
            snoozed("t"),
            json!({"pendingRuntimeRequest": {"kind": "auth_refresh"}}),
        );
        assert_eq!(woke_at(&refreshing, now()), None);
        assert!(effective_snoozed(&refreshing, now()));
    }

    #[test]
    fn only_a_working_card_needs_the_tick() {
        let ticks = |fields: Value| working(&with(thread("t"), fields));
        assert!(ticks(running()));
        assert!(ticks(json!({"latestRunId": "r", "status": "queued"})));
        assert!(ticks(
            json!({"latestRunId": "r", "status": "completed", "activityRunStatus": "waiting"})
        ));
        assert!(ticks(with(
            running(),
            json!({"goal": {"objective": "Ship it", "status": "active"}})
        )));
        assert!(ticks(with(
            running(),
            json!({"pendingRuntimeRequest": {"kind": "auth_refresh"}})
        )));
        // A card that shows a request, Waiting or a failure has no clock to move.
        assert!(!ticks(with(
            running(),
            json!({"pendingRuntimeRequest": {"kind": "command"}})
        )));
        assert!(!ticks(
            json!({"latestRunId": "r", "status": "completed", "pendingBackgroundTasks": [{"taskId": "s", "kind": "subagent"}]})
        ));
        assert!(!ticks(json!({"latestRunId": "r", "status": "failed"})));
        assert!(!ticks(finished(json!("2026-10-08T10:00:00.000Z"))));
        assert!(!ticks(json!({})));
    }

    #[test]
    fn the_clock_counts_from_when_the_work_began() {
        let since = |fields: Value| working_since(&with(thread("t"), fields));
        let at = |iso: &str| Some(ms(iso));
        // A server that sends `activityRunStartedAt` decides alone, even when it is null. A
        // wake keeps the start of the work it continues.
        assert_eq!(since(running()), at("2026-10-08T11:57:00.000Z"));
        assert_eq!(
            since(with(
                running(),
                json!({"activityRunStartedAt": "2026-10-08T11:00:00.000Z"})
            )),
            at("2026-10-08T11:00:00.000Z")
        );
        assert_eq!(
            since(with(running(), json!({"activityRunStartedAt": null}))),
            None
        );
        assert_eq!(
            since(with(running(), json!({"activityRunStartedAt": "later"}))),
            None
        );

        // An older server: the latest run's start, while it is unfinished and the active run.
        let older = without(running(), "activityRunStartedAt");
        assert_eq!(since(older.clone()), at("2026-10-08T11:57:00.000Z"));
        assert_eq!(
            since(with(older.clone(), json!({"latestRunStartedAt": null}))),
            at("2026-10-08T11:56:50.000Z"),
            "the request time until the run starts"
        );
        assert_eq!(
            since(with(older.clone(), json!({"activeRunId": "another"}))),
            None
        );
        assert_eq!(
            since(with(older.clone(), json!({"activeRunId": null}))),
            None
        );
        assert_eq!(
            since(with(
                older,
                json!({"latestRunCompletedAt": "2026-10-08T11:59:00.000Z"})
            )),
            None
        );
    }

    // ---- drawing ----

    fn render(
        sidebar: &mut Sidebar,
        shell: Option<&ShellState>,
        open: Option<&str>,
        size: (u16, u16),
    ) -> Buffer {
        render_in(Depth::TrueColor, sidebar, shell, open, size)
    }

    /// `render` with the colors a terminal of `depth` gets.
    fn render_in(
        depth: Depth,
        sidebar: &mut Sidebar,
        shell: Option<&ShellState>,
        open: Option<&str>,
        size: (u16, u16),
    ) -> Buffer {
        let theme = Theme::new(depth);
        let badges = shell.map(Badges::of).unwrap_or_default();
        let view = View {
            theme: &theme,
            shell,
            badges: &badges,
            open_id: open,
            focused: true,
            now: now(),
            feed: Feed::Live,
            environment: &Identity::default(),
        };
        let mut terminal = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
        let frame = terminal
            .draw(|frame| {
                let area = frame.area();
                sidebar.draw(frame, area, &view);
            })
            .unwrap();
        frame.buffer.clone()
    }

    /// Each screen row as plain text.
    fn text(buffer: &Buffer) -> Vec<String> {
        let area = buffer.area;
        (area.top()..area.bottom())
            .map(|y| {
                (area.left()..area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    /// The first screen row that shows `needle`.
    fn row_of(lines: &[String], needle: &str) -> u16 {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no row shows {needle:?} in {lines:#?}")) as u16
    }

    #[test]
    fn the_renderer_draws_a_heading_over_each_shelf() {
        let shell = shell(vec![
            with(thread("p"), json!({"pinnedAt": "2026-10-02T00:00:00.000Z"})),
            thread("a"),
            snoozed("z"),
            with(thread("s"), json!({"settledOverride": "settled"})),
        ]);
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&shell.threads, ALL, None, now());
        let buffer = render(&mut sidebar, Some(&shell), None, (30, 32));
        let lines = text(&buffer);
        let pinned = row_of(&lines, "Pinned ─");
        let inbox = row_of(&lines, "Inbox ─");
        let working = row_of(&lines, "Working (0) ─");
        let snoozed = row_of(&lines, "Snoozed ─");
        // A blank row over the wordmark, then the wordmark, the environment and another
        // blank row.
        assert!(lines[0].trim().is_empty() && lines[3].trim().is_empty());
        assert!(lines[1].contains("T3 Code"));
        assert_eq!(pinned, 4, "the list starts under the gap");
        assert!(pinned < row_of(&lines, "Thread p"));
        assert!(row_of(&lines, "Thread p") < inbox);
        assert!(inbox < row_of(&lines, "Thread a"));
        // The list fits, so the shelves under the inbox sit at the bottom with the room
        // left over above them, the GUI's layout.
        assert_eq!(working, 23);
        assert!(lines[15..22].iter().all(|line| line.trim().is_empty()));
        assert_eq!(row_of(&lines, "Done (0) ─"), working + 1);
        assert_eq!(snoozed, working + 2);
        assert!(snoozed < row_of(&lines, "Thread z"));
        // The settled thread stays behind its closed heading, which counts it and ends the
        // list on the last row.
        assert!(!lines.iter().any(|line| line.contains("Thread s")));
        assert!(lines[31].contains("Settled (1) ─"));
        // An open shelf's chevron points down and a closed one's points right, at the cards'
        // right edge.
        assert!(lines[working as usize].trim_end().ends_with("─ ⌄"));
        assert!(lines[31].trim_end().ends_with("─ ›"));
        assert_eq!(buffer[(26, 31)].symbol(), "›");

        // Headings start one column in from the sidebar's edge, like the cards' bar.
        let theme = Theme::new(Depth::TrueColor);
        assert_eq!(buffer[(1, snoozed)].symbol(), "S");
        assert_eq!(buffer[(1, snoozed)].fg, theme.info_fg);
        assert_eq!(buffer[(1, inbox)].fg, theme.sidebar_muted);
        assert_eq!(buffer[(1, working)].fg, theme.sidebar_muted);
        // Only the headings of the shelves that fold take a click.
        assert_eq!(sidebar.shelf_at(working), Some(Shelf::Working));
        assert_eq!(sidebar.shelf_at(working + 1), Some(Shelf::Done));
        assert_eq!(sidebar.shelf_at(31), Some(Shelf::Settled));
        for y in [pinned, inbox, snoozed, working - 1, 30] {
            assert_eq!(sidebar.shelf_at(y), None, "{y}");
        }
    }

    #[test]
    fn a_click_lands_on_the_card_that_was_drawn() {
        let b = with(
            thread("b"),
            json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
        );
        let c = with(
            thread("c"),
            json!({"createdAt": "2026-10-01T00:00:00.000Z"}),
        );
        let first = shell(vec![b.clone(), c.clone()]);
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&first.threads, ALL, None, now());
        let lines = text(&render(&mut sidebar, Some(&first), None, (30, 24)));
        let y = row_of(&lines, "Thread c");
        assert_eq!(sidebar.thread_at(y), Some("c"));
        assert_eq!(sidebar.thread_at(y - 1), Some("c"), "the card's first line");
        assert_eq!(sidebar.thread_at(row_of(&lines, "Working (0)")), None);
        assert_eq!(sidebar.thread_at(y + 2), None, "the gap under the card");

        // A new thread arrives on top before the next frame. The click still means `c`.
        let a = with(
            thread("a"),
            json!({"createdAt": "2026-10-03T00:00:00.000Z"}),
        );
        let second = shell(vec![a, b, c]);
        sidebar.rebuild(&second.threads, ALL, None, now());
        assert_eq!(sidebar.thread_at(y), Some("c"));

        // Once redrawn, the same row shows `b`, and a click there means `b`.
        let lines = text(&render(&mut sidebar, Some(&second), None, (30, 24)));
        assert!(lines[y as usize].contains("Thread b"));
        assert_eq!(sidebar.thread_at(y), Some("b"));
    }

    #[test]
    fn scrolling_keeps_the_highlight_and_its_heading_in_view() {
        let mut threads = vec![with(
            thread("p"),
            json!({"pinnedAt": "2026-10-02T00:00:00.000Z"}),
        )];
        threads.extend((1..=6).map(|n| {
            with(
                thread(&format!("a{n}")),
                json!({"createdAt": format!("2026-10-0{}T00:00:00.000Z", 8 - n)}),
            )
        }));
        let listed = shell(threads);
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&listed.threads, ALL, None, now());
        // Fourteen rows leave twelve for the list, room for three cards.
        let size = (30, 14);
        for _ in 0..5 {
            sidebar.move_selection(1);
        }
        assert_eq!(sidebar.selected_thread_id(), Some("a5"));
        let lines = text(&render(&mut sidebar, Some(&listed), None, size));
        assert_eq!(row_of(&lines, "Thread a5"), 11, "a3 to a5 fill the list");
        assert!(!lines.iter().any(|line| line.contains("Settled")));

        // On the last card, the empty shelves' headings under it come into view with it, so
        // the wheel and the arrow keys reach them.
        sidebar.move_selection(1);
        assert_eq!(sidebar.selected_thread_id(), Some("a6"));
        let lines = text(&render(&mut sidebar, Some(&listed), None, size));
        assert!(lines.iter().any(|line| line.contains("Thread a6")));
        assert!(!lines.iter().any(|line| line.contains("Thread p")));
        assert!(lines[11].contains("Working (0)"));
        assert!(lines[12].contains("Done (0)"));
        assert!(lines[13].contains("Settled (0)"), "{lines:#?}");
        assert_eq!(sidebar.shelf_at(13), Some(Shelf::Settled));

        // Back up to the first inbox card: its heading comes into view above it.
        for _ in 0..5 {
            sidebar.move_selection(-1);
        }
        assert_eq!(sidebar.selected_thread_id(), Some("a1"));
        let lines = text(&render(&mut sidebar, Some(&listed), None, size));
        assert!(row_of(&lines, "Inbox ─") < row_of(&lines, "Thread a1"));
        assert!(!lines.iter().any(|line| line.contains("Thread p")));
        assert_eq!(sidebar.shelf_at(13), None, "the heading scrolled away");

        // A thread pinned above the view doesn't move the rows the reader is looking at.
        assert_eq!(
            sidebar.rows[sidebar.offset],
            heading_row(Shelf::Inbox, true)
        );
        let mut more = listed.threads.clone();
        more.push(with(
            thread("p2"),
            json!({"pinnedAt": "2026-10-03T00:00:00.000Z"}),
        ));
        sidebar.rebuild(&more, ALL, None, now());
        assert_eq!(sidebar.offset, 3);
        assert_eq!(
            sidebar.rows[sidebar.offset],
            heading_row(Shelf::Inbox, true)
        );
        assert_eq!(sidebar.selected_thread_id(), Some("a1"));
        let lines = text(&render(&mut sidebar, Some(&shell(more)), None, size));
        assert!(row_of(&lines, "Inbox ─") < row_of(&lines, "Thread a1"));
        assert!(!lines.iter().any(|line| line.contains("Thread p2")));
    }

    /// The Working shelf with `cards` cards, `c0` first, so card `cN` is row `N + 1`.
    fn long_list(cards: usize) -> Vec<Row> {
        let mut rows = vec![heading_row(Shelf::Working, false)];
        rows.extend((0..cards).map(|n| card(&format!("c{n}"))));
        rows
    }

    /// Scrolls `rows` for a list `height` lines tall, with the highlight on row `selected` and
    /// row `offset` on top. Returns the new top row and how many rows the scroll measured.
    fn scroll(rows: Vec<Row>, selected: usize, offset: usize, height: usize) -> (usize, usize) {
        let mut sidebar = Sidebar {
            rows,
            selected,
            offset,
            ..Sidebar::default()
        };
        MEASURED.set(0);
        sidebar.scroll_into_view(height);
        (sidebar.offset, MEASURED.get())
    }

    #[test]
    fn a_redraw_measures_the_screen_not_the_rows_below_it() {
        // Near the top of the list, as an idle redraw finds it: c2 to c7 fill 24 lines
        // exactly, and the highlight is on c7.
        let (top, few) = scroll(long_list(100), 8, 3, 24);
        let (same_top, many) = scroll(long_list(10_000), 8, 3, 24);
        assert_eq!((top, same_top), (3, 3));
        assert_eq!(few, many, "the rows below the screen cost nothing");
        assert!(many < 30, "measured {many} rows for a 24-line list");
    }

    #[test]
    fn scrolling_fills_the_list_to_its_last_line_and_no_further() {
        let rows = long_list(20);
        // Row 20 is the last card. Six cards fill 24 lines exactly, whether the top starts
        // above the highlight or on it.
        assert_eq!(scroll(rows.clone(), 20, 0, 24).0, 15);
        assert_eq!(scroll(rows.clone(), 20, 20, 24).0, 15);
        // A line short, five fit.
        assert_eq!(scroll(rows.clone(), 20, 0, 23).0, 16);
        assert_eq!(scroll(rows.clone(), 20, 20, 23).0, 16);
        // A heading comes back over its first card only when both fit, one line and four.
        assert_eq!(scroll(rows.clone(), 1, 1, 5).0, 0);
        assert_eq!(scroll(rows.clone(), 1, 1, 4).0, 1);
        // A card taller than the list stays on top, cut off at the bottom.
        assert_eq!(scroll(rows, 20, 0, 3).0, 20);
    }

    #[test]
    fn the_empty_list_says_why() {
        let mut sidebar = Sidebar::default();
        let hint = |sidebar: &mut Sidebar, shell: Option<&ShellState>| {
            text(&render(sidebar, shell, None, (30, 12)))[2]
                .trim()
                .to_string()
        };
        assert_eq!(hint(&mut sidebar, None), "Loading threads…");

        let mut empty = shell(Vec::new());
        assert_eq!(hint(&mut sidebar, Some(&empty)), "No threads yet");
        empty.projects.clear();
        assert_eq!(hint(&mut sidebar, Some(&empty)), "No projects yet");

        let hidden = shell(vec![with(
            thread("archived"),
            json!({"archivedAt": "2026-10-05T00:00:00.000Z"}),
        )]);
        sidebar.rebuild(&hidden.threads, ALL, None, now());
        assert_eq!(hint(&mut sidebar, Some(&hidden)), "No threads yet");

        // A list of settled threads needs no hint. The closed shelves' headings sit at the
        // bottom and count them.
        let settled = shell(vec![with(
            thread("s"),
            json!({"settledOverride": "settled"}),
        )]);
        sidebar.rebuild(&settled.threads, ALL, None, now());
        let lines = text(&render(&mut sidebar, Some(&settled), None, (30, 12)));
        assert_eq!(lines[2].trim(), "");
        assert!(lines[9].contains("Working (0)"), "{lines:#?}");
        assert!(lines[10].contains("Done (0)"));
        assert!(lines[11].contains("Settled (1)"));
    }

    #[test]
    fn each_word_draws_in_its_color_at_the_cards_right_edge() {
        let theme = Theme::new(Depth::TrueColor);
        let spinner = spinner_frame(now());
        let background = json!({"latestRunId": "r", "status": "completed", "pendingBackgroundTasks": [{"taskId": "s", "kind": "subagent"}]});
        let cases = [
            (
                with(
                    running(),
                    json!({"pendingRuntimeRequest": {"kind": "command"}}),
                ),
                "Approval".to_string(),
                theme.warning_fg,
            ),
            (
                with(
                    running(),
                    json!({"pendingRuntimeRequest": {"kind": "user_input"}}),
                ),
                "Input".to_string(),
                theme.indigo,
            ),
            (running(), format!("{spinner} Working 3m"), theme.info),
            (
                with(
                    running(),
                    json!({"goal": {"objective": "Ship it", "status": "active"}}),
                ),
                format!("{spinner} Goal 3m"),
                theme.info,
            ),
            (background, "Waiting".to_string(), theme.sidebar_muted),
            (
                json!({"latestRunId": "r", "status": "failed", "lastErrorClass": "usage_limit"}),
                "Limited".to_string(),
                theme.warning,
            ),
            (
                json!({"latestRunId": "r", "status": "failed", "lastErrorClass": "provider_error"}),
                "Failed".to_string(),
                theme.error,
            ),
            (
                json!({"snoozedAt": "2026-10-08T10:00:00.000Z", "snoozedUntil": "2026-10-08T11:30:00.000Z"}),
                "Woke".to_string(),
                theme.warning,
            ),
            (
                finished(json!("2026-10-08T10:00:00.000Z")),
                "Done".to_string(),
                theme.emerald,
            ),
            // A seen finish has no word, so the card shows the time of the last message.
            (
                with(
                    finished(json!("2026-10-08T11:30:00.000Z")),
                    json!({"latestUserMessageAt": "2026-10-08T10:50:00.000Z"}),
                ),
                "1h".to_string(),
                theme.sidebar_muted,
            ),
        ];
        for (fields, want, color) in cases {
            let shell = shell(vec![with(thread("t"), fields)]);
            let mut sidebar = Sidebar::default();
            sidebar.rebuild(&shell.threads, ALL, None, now());
            let buffer = render(&mut sidebar, Some(&shell), None, (34, 10));
            let lines = text(&buffer);
            let y = row_of(&lines, "Thread t") - 1;
            let line = &lines[y as usize];
            assert!(line.trim_end().ends_with(&want), "{want}: {line:?}");
            // The card's inside ends two columns before the border strip.
            assert_eq!(buffer[(30, y)].fg, color, "{want}");
            assert_eq!(buffer[(31, y)].symbol(), " ", "{want}");
        }
    }

    #[test]
    fn a_narrow_card_cuts_the_project_name_and_keeps_the_status() {
        let mut shell = shell(vec![with(thread("t"), running())]);
        shell.projects = vec![json!({"id": "p1", "title": "Interface experiments"})];
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&shell.threads, ALL, None, now());
        // At the narrowest the app draws the sidebar, 26 columns, the card has 21 inside its
        // bar: the badge, the project cut to one ellipsis, a gap and the whole status.
        let lines = text(&render(&mut sidebar, Some(&shell), None, (26, 10)));
        let y = row_of(&lines, "Thread t") as usize - 1;
        let inside: String = lines[y].chars().skip(2).take(21).collect();
        let spinner = spinner_frame(now());
        assert_eq!(inside, format!("IE Inte… {spinner} Working 3m"));
    }

    #[test]
    fn a_status_short_of_room_drops_the_clock_then_the_spinner_then_cuts_the_word() {
        let theme = Theme::new(Depth::TrueColor);
        let view = View {
            theme: &theme,
            shell: None,
            badges: &Badges::default(),
            open_id: None,
            focused: true,
            now: now(),
            feed: Feed::Live,
            environment: &Identity::default(),
        };
        let spinner = spinner_frame(now());
        let busy = with(thread("t"), running());
        let within = |room| view.thread_status(Some(&busy), room).0;
        assert_eq!(within(40), format!("{spinner} Working 3m"));
        assert_eq!(within(12), format!("{spinner} Working 3m"));
        assert_eq!(within(11), format!("{spinner} Working"));
        assert_eq!(within(9), format!("{spinner} Working"));
        assert_eq!(within(8), "Working");
        assert_eq!(within(7), "Working");
        assert_eq!(within(6), "Worki…");
        assert_eq!(within(1), "…");
        assert_eq!(within(0), "");
        assert_eq!(view.thread_status(Some(&busy), 0).1, theme.sidebar_muted);

        // Without a start time that parses there is no clock, rather than one at 0s.
        let unstarted = with(busy.clone(), json!({"activityRunStartedAt": null}));
        assert_eq!(
            view.thread_status(Some(&unstarted), 40).0,
            format!("{spinner} Working")
        );
        // A clock past an hour takes more room.
        let long = with(
            busy.clone(),
            json!({"activityRunStartedAt": "2026-10-08T10:58:00.000Z"}),
        );
        assert_eq!(
            view.thread_status(Some(&long), 40).0,
            format!("{spinner} Working 1h 2m")
        );
        assert_eq!(
            view.thread_status(Some(&long), 14).0,
            format!("{spinner} Working")
        );

        // A word without a spinner can only be cut.
        let failed = with(thread("t"), json!({"latestRunId": "r", "status": "failed"}));
        assert_eq!(
            view.thread_status(Some(&failed), 6),
            ("Failed".to_string(), theme.error)
        );
        assert_eq!(view.thread_status(Some(&failed), 5).0, "Fail…");

        // With no word the card shows its age, from the last message or else the last update.
        let resting = with(
            thread("t"),
            json!({"latestUserMessageAt": "2026-10-08T11:30:00.000Z"}),
        );
        assert_eq!(
            view.thread_status(Some(&resting), 10),
            ("30m".to_string(), theme.sidebar_muted)
        );
        assert_eq!(view.thread_status(Some(&thread("t")), 10).0, "7d");
        assert_eq!(view.thread_status(Some(&resting), 2).0, "3…");
        assert_eq!(
            view.thread_status(None, 10),
            (String::new(), theme.sidebar_muted)
        );
    }

    #[test]
    fn a_drawn_card_needs_the_tick_only_while_it_reads_working_or_goal() {
        // Draws one card and checks its word, then says whether the frame needs the tick.
        let ticks = |fields: Value, word: &str| {
            let listed = shell(vec![with(thread("t"), fields)]);
            let mut sidebar = Sidebar::default();
            sidebar.rebuild(&listed.threads, ALL, None, now());
            let lines = text(&render(&mut sidebar, Some(&listed), None, (30, 12)));
            let line = &lines[row_of(&lines, "Thread t") as usize - 1];
            assert!(line.contains(word), "{line:?} should show {word}");
            sidebar.drew_working()
        };
        // Queued and continuing work read Working, and a goal reads Goal. Their spinners turn.
        assert!(ticks(
            json!({"latestRunId": "r", "status": "queued"}),
            "Working"
        ));
        assert!(ticks(
            json!({"latestRunId": "r", "status": "completed", "activityRunStatus": "waiting"}),
            "Working"
        ));
        assert!(ticks(
            with(
                running(),
                json!({"goal": {"objective": "Ship it", "status": "active"}})
            ),
            "Goal"
        ));
        // A request, Waiting, a failure or a finish shows a word with nothing to move.
        assert!(!ticks(
            with(
                running(),
                json!({"pendingRuntimeRequest": {"kind": "command"}})
            ),
            "Approval"
        ));
        assert!(!ticks(
            with(
                running(),
                json!({"pendingRuntimeRequest": {"kind": "user_input"}})
            ),
            "Input"
        ));
        assert!(!ticks(
            json!({"latestRunId": "r", "status": "completed", "pendingBackgroundTasks": [{"taskId": "s", "kind": "subagent"}]}),
            "Waiting"
        ));
        assert!(!ticks(
            json!({"latestRunId": "r", "status": "failed"}),
            "Failed"
        ));
        assert!(!ticks(finished(json!("2026-10-08T10:00:00.000Z")), "Done"));
    }

    #[test]
    fn work_the_sidebar_leaves_out_needs_no_tick() {
        // Each of these reads Working by the card rules, but none has a card in the list.
        let archived = with(
            thread("archived"),
            with(running(), json!({"archivedAt": "2026-10-05T00:00:00.000Z"})),
        );
        let subagent = with(
            thread("subagent"),
            json!({
                "latestRunId": "r",
                "status": "queued",
                "lineage": {"parentThreadId": "a", "relationshipToParent": "subagent", "rootThreadId": "a"},
            }),
        );
        let settled = with(
            thread("s"),
            json!({
                "latestRunId": "r",
                "status": "completed",
                "activityRunStatus": "waiting",
                "settledOverride": "settled",
            }),
        );
        assert!([&archived, &subagent, &settled].into_iter().all(working));
        let listed = shell(vec![thread("a"), archived, subagent, settled]);
        let draw = |sidebar: &mut Sidebar, open: Option<&str>| {
            sidebar.rebuild(&listed.threads, ALL, open, now());
            text(&render(sidebar, Some(&listed), open, (30, 24)))
        };
        let spinning = format!("{} Working", spinner_frame(now()));
        let mut sidebar = Sidebar::default();
        let lines = draw(&mut sidebar, None);
        assert!(lines.iter().any(|line| line.contains("Thread a")));
        assert!(!lines.iter().any(|line| line.contains(&spinning)));
        assert!(!sidebar.drew_working());

        // Opening the Settled shelf draws the settled thread's slim row, whose spinner needs
        // the tick.
        assert_eq!(sidebar.toggle(Shelf::Settled), Some(true));
        let lines = draw(&mut sidebar, None);
        assert!(lines[row_of(&lines, "Thread s") as usize].contains(&spinning));
        assert!(sidebar.drew_working());

        // Closed again, the shelf keeps the open thread's row, and the tick with it.
        assert_eq!(sidebar.toggle(Shelf::Settled), Some(false));
        let lines = draw(&mut sidebar, Some("s"));
        assert!(lines[row_of(&lines, "Thread s") as usize].contains(&spinning));
        assert!(sidebar.drew_working());

        // Once another thread is open, the row goes back behind the closed heading.
        let lines = draw(&mut sidebar, Some("a"));
        assert!(!lines.iter().any(|line| line.contains("Thread s")));
        assert!(!sidebar.drew_working());
    }

    #[test]
    fn a_working_card_needs_the_tick_only_while_it_is_in_view() {
        // a1 is the newest, so it heads the list. It works toward a goal, and a8 at the bottom
        // works too.
        let threads = (1..=8)
            .map(|n| {
                let card = with(
                    thread(&format!("a{n}")),
                    json!({"createdAt": format!("2026-10-0{}T00:00:00.000Z", 9 - n)}),
                );
                match n {
                    1 => with(
                        card,
                        with(
                            running(),
                            json!({"goal": {"objective": "Ship it", "status": "active"}}),
                        ),
                    ),
                    8 => with(card, running()),
                    _ => card,
                }
            })
            .collect();
        let listed = shell(threads);
        // With the Working shelf off, the working threads stay in the inbox among the rest.
        let mut sidebar = Sidebar::with_choices(ShelfChoices {
            working_enabled: false,
            ..ShelfChoices::default()
        });
        sidebar.rebuild(&listed.threads, ALL, None, now());
        // Fourteen rows leave twelve for the list: a1, a2 and a3.
        let size = (30, 14);
        let lines = text(&render(&mut sidebar, Some(&listed), None, size));
        assert!(lines[row_of(&lines, "Thread a1") as usize - 1].contains("Goal"));
        assert!(sidebar.drew_working());

        // Down at a5 the view holds a3 to a5, which rest, though a1 and a8 still work.
        for _ in 0..4 {
            sidebar.move_selection(1);
        }
        assert_eq!(sidebar.selected_thread_id(), Some("a5"));
        let lines = text(&render(&mut sidebar, Some(&listed), None, size));
        assert!(lines.iter().any(|line| line.contains("Thread a3")));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("Goal") || line.contains("Working")),
            "{lines:#?}"
        );
        assert!(!sidebar.drew_working());

        // At the bottom of the list, a8 comes into view and the tick with it.
        for _ in 0..3 {
            sidebar.move_selection(1);
        }
        assert_eq!(sidebar.selected_thread_id(), Some("a8"));
        let lines = text(&render(&mut sidebar, Some(&listed), None, size));
        assert!(lines[row_of(&lines, "Thread a8") as usize - 1].contains("Working"));
        assert!(sidebar.drew_working());
    }

    #[test]
    fn a_sidebar_too_small_for_cards_needs_no_tick() {
        let listed = shell(vec![with(thread("t"), running())]);
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&listed.threads, ALL, None, now());
        render(&mut sidebar, Some(&listed), None, (30, 12));
        assert!(sidebar.drew_working());
        // Too narrow or too short to draw a card, so nothing on screen moves.
        render(&mut sidebar, Some(&listed), None, (5, 12));
        assert!(!sidebar.drew_working());
        render(&mut sidebar, Some(&listed), None, (30, 3));
        assert!(!sidebar.drew_working());
        // Four rows leave the list two lines. The card's first line, word and all, takes the
        // top one, ahead of its shelf's heading.
        let lines = text(&render(&mut sidebar, Some(&listed), None, (30, 4)));
        assert!(
            lines[2].contains("Working") && !lines[2].contains("Working ("),
            "{lines:#?}"
        );
        assert!(lines[3].contains("Thread t"));
        assert!(sidebar.drew_working());
    }

    #[test]
    fn a_hidden_sidebar_forgets_its_frame_and_keeps_its_rows() {
        let before = shell(vec![busy("w", json!({})), thread("a"), thread("b")]);
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&before.threads, ALL, None, now());
        assert!(sidebar.select("b"));
        let lines = text(&render(&mut sidebar, Some(&before), None, (30, 24)));
        assert_eq!(sidebar.thread_at(row_of(&lines, "Thread b")), Some("b"));
        let settled = row_of(&lines, "Settled (0)");
        assert_eq!(
            sidebar.shelf_at(row_of(&lines, "Working (1)")),
            Some(Shelf::Working)
        );
        assert_eq!(sidebar.shelf_at(settled), Some(Shelf::Settled));
        assert_ne!(sidebar.list, Rect::default());
        // The edge a drag resizes it by is its last column, top to bottom.
        assert_eq!(sidebar.border, Rect::new(29, 0, 1, 24));
        assert!(sidebar.drew_working());

        // Hidden, the frame draws no sidebar, so nothing it drew before takes a click, the
        // wheel or the tick.
        sidebar.forget_drawn();
        for y in 0..24 {
            assert_eq!(sidebar.thread_at(y), None);
            assert_eq!(sidebar.shelf_at(y), None);
        }
        assert_eq!(sidebar.list, Rect::default());
        assert_eq!(sidebar.border, Rect::default());
        assert!(!sidebar.drew_working());

        // While it is hidden, `b` is renamed and `c` arrives. Shown again, it lists both, with
        // the highlight still on `b`.
        let after = shell(vec![
            busy("w", json!({})),
            thread("a"),
            with(thread("b"), json!({"title": "Renamed b"})),
            with(
                thread("c"),
                json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
            ),
        ]);
        sidebar.rebuild(&after.threads, ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("b"));
        let lines = text(&render(&mut sidebar, Some(&after), None, (30, 24)));
        assert_eq!(sidebar.thread_at(row_of(&lines, "Renamed b")), Some("b"));
        assert_eq!(sidebar.thread_at(row_of(&lines, "Thread c")), Some("c"));
        assert!(sidebar.drew_working());
        assert_eq!(sidebar.border, Rect::new(29, 0, 1, 24));

        // Too narrow to draw, it has no edge to drag either.
        render(&mut sidebar, Some(&after), None, (5, 24));
        assert_eq!(sidebar.border, Rect::default());
        assert_eq!(sidebar.list, Rect::default());
    }

    // ---- the Working shelf ----

    /// A thread whose run started at 11:57 and is still going, so it belongs in Working.
    fn busy(id: &str, fields: Value) -> Value {
        with(with(thread(id), running()), fields)
    }

    #[test]
    fn only_busy_threads_with_nothing_for_the_user_belong_in_working() {
        let works = |fields: Value| belongs_in_working(&with(thread("t"), fields));
        // A run under way, in each of its live states.
        for state in ["preparing", "queued", "starting", "running", "waiting"] {
            let run = json!({"latestRunId": "r", "activeRunId": "r", "status": state});
            assert!(works(run), "{state}");
        }
        assert!(works(running()));
        // A live activity run outranks the thread's status, as when a wake continues the work.
        assert!(works(
            json!({"latestRunId": "r", "status": "failed", "activityRunStatus": "running"})
        ));

        // Stopped with background work that will wake it, which presents as idle. So does a
        // provider thread with no run yet, and the `idle` the server sends before a first run.
        let monitored = json!({
            "latestRunId": "r",
            "status": "completed",
            "pendingBackgroundTasks": [{"taskId": "m", "kind": "monitor"}],
        });
        assert!(works(monitored.clone()));
        assert!(works(json!({"activeProviderThreadId": "pt"})));
        assert!(works(json!({"latestRunId": "r", "status": "idle"})));
        // A command it left running, such as a dev server, doesn't hold it.
        assert!(!works(
            json!({"latestRunId": "r", "status": "completed", "pendingBackgroundTasks": [{"taskId": "c", "kind": "command"}]})
        ));

        // At rest, never run, or ended any other way.
        assert!(!works(json!({})));
        for state in [
            "completed",
            "interrupted",
            "cancelled",
            "rolled_back",
            "failed",
        ] {
            assert!(
                !works(json!({"latestRunId": "r", "status": state})),
                "{state}"
            );
        }
        // A failure outranks the background work, so the thread stays where the user sees it.
        assert!(!works(with(monitored.clone(), json!({"status": "failed"}))));

        // A request for the user keeps the thread in Active. An auth refresh isn't one.
        for kind in [
            "command",
            "file-change",
            "permission",
            "user_input",
            "something_new",
        ] {
            let asking = with(running(), json!({"pendingRuntimeRequest": {"kind": kind}}));
            assert!(!works(asking), "{kind}");
        }
        assert!(works(with(
            running(),
            json!({"pendingRuntimeRequest": {"kind": "auth_refresh"}})
        )));
        assert!(!works(with(
            monitored.clone(),
            json!({"pendingRuntimeRequest": {"kind": "user_input"}})
        )));

        // A plan ready for a reply outranks background work once its run has settled.
        let plan = json!({"interactionMode": "plan", "hasActionableProposedPlan": true});
        assert!(!works(with(monitored.clone(), plan.clone())));
        assert!(!works(with(
            json!({"latestRunId": "r", "status": "idle"}),
            plan.clone()
        )));
        // It does while an activity run other than the latest goes on, too, since the latest
        // run has settled.
        assert!(!works(with(
            json!({"latestRunId": "r", "activeRunId": "r2", "status": "completed", "activityRunStatus": "running"}),
            plan.clone()
        )));
        // While the run that writes the plan is going, or the active run is still the latest,
        // the thread works.
        assert!(works(with(running(), plan.clone())));
        assert!(works(with(
            json!({"latestRunId": "r", "activeRunId": "r", "status": "completed", "activityRunStatus": "running"}),
            plan
        )));
        // Plan mode without a plan, or a plan outside plan mode, holds nothing back.
        assert!(works(with(
            monitored.clone(),
            json!({"interactionMode": "plan", "hasActionableProposedPlan": false})
        )));
        assert!(works(with(
            monitored,
            json!({"interactionMode": "default", "hasActionableProposedPlan": true})
        )));
    }

    #[test]
    fn only_active_threads_fold_into_working() {
        let lineage =
            json!({"parentThreadId": "a", "relationshipToParent": "subagent", "rootThreadId": "a"});
        let threads = vec![
            busy("a", json!({})),
            thread("rest"),
            busy("p", json!({"pinnedAt": "2026-10-02T00:00:00.000Z"})),
            with(snoozed("z"), running()),
            busy("s", json!({"settledOverride": "settled"})),
            busy(
                "archived",
                json!({"archivedAt": "2026-10-05T00:00:00.000Z"}),
            ),
            busy("sub", json!({"lineage": lineage})),
        ];
        let returns = Returns::default();
        let on = shelves(&threads, ALL, now(), Some(&returns));
        assert_eq!(ids(&on.working), ["a"]);
        assert_eq!(ids(&on.inbox), ["rest"]);
        assert_eq!(ids(&on.pinned), ["p"]);
        assert_eq!(ids(&on.snoozed), ["z"]);
        assert_eq!(ids(&on.settled), ["s"]);

        // Off, the busy thread stays in Active.
        let off = shelves(&threads, ALL, now(), None);
        assert!(off.working.is_empty());
        let mut active = ids(&off.inbox);
        active.sort();
        assert_eq!(active, ["a", "rest"]);

        // Without snooze or settlement, the snoozed and settled threads are Active, so they
        // fold away too. All three asked for their runs at once, so they go by id.
        let none = shelves(&threads, Capabilities::default(), now(), Some(&returns));
        assert_eq!(ids(&none.working), ["a", "s", "z"]);

        // When the snooze ends, the thread comes back to Working rather than the inbox.
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(sidebar.next_wake, Some(ms("2026-10-08T13:00:00.000Z")));
        sidebar.rebuild(&threads, ALL, None, ms("2026-10-08T13:00:00.000Z"));
        assert_eq!(sidebar.next_wake, None);
        assert_eq!(
            sidebar.rows,
            [
                heading_row(Shelf::Pinned, false),
                card("p"),
                heading_row(Shelf::Inbox, true),
                card("rest"),
                heading_row(Shelf::Working, true),
                card("a"),
                card("z"),
                heading_row(Shelf::Done, true),
                heading_row(Shelf::Settled, false),
            ]
        );
    }

    #[test]
    fn with_the_shelf_off_nothing_changes() {
        // The threads of `active_threads_lead_with_new_and_reopened_then_arranged`, two of them
        // busy.
        let threads = vec![
            with(
                thread("a1"),
                json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
            ),
            with(thread("k1"), json!({"activeOrderKey": "b"})),
            with(
                thread("a2"),
                json!({"createdAt": "2026-10-01T00:00:00.000Z", "unsettledAt": "2026-10-06T00:00:00.000Z"}),
            ),
            busy("a4", json!({"createdAt": "2026-10-04T00:00:00.000Z"})),
            busy("k2", json!({"activeOrderKey": "a"})),
            with(
                thread("a3"),
                json!({"createdAt": "2026-10-04T00:00:00.000Z"}),
            ),
        ];
        let mut sidebar = Sidebar::with_choices(ShelfChoices {
            working_enabled: false,
            ..ShelfChoices::default()
        });
        sidebar.rebuild(&threads, ALL, None, now());
        let mut want: Vec<Row> = ["a2", "a3", "a4", "a1", "k2", "k1"].map(card).into();
        want.extend([
            heading_row(Shelf::Done, true),
            heading_row(Shelf::Settled, false),
        ]);
        assert_eq!(sidebar.rows, want);
        assert!(!sidebar.has_shelf(Shelf::Working));
        assert_eq!(sidebar.toggle(Shelf::Working), None);
        assert_eq!(sidebar.rows, want);
        assert!(sidebar.returns.working.is_none(), "nothing observed");

        // On, the busy threads leave, and the inbox goes by return, ignoring the arranged
        // keys. k1 and a1 never ran, so they sort by when they were created.
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        let mut want: Vec<Row> = ["a2", "a3", "a1", "k1"].map(card).into();
        want.push(heading_row(Shelf::Working, true));
        want.extend(["a4", "k2"].map(card));
        want.extend([
            heading_row(Shelf::Done, true),
            heading_row(Shelf::Settled, false),
        ]);
        assert_eq!(sidebar.rows, want);
    }

    #[test]
    fn working_lists_the_latest_send_first() {
        // As in the nightly's test: a wake run asked for after the other thread's send doesn't
        // move `sent-first`, and a thread an agent launched has no send, so its creation counts.
        let sent_first = busy(
            "sent-first",
            json!({
                "latestUserAuthoredMessageAt": "2026-10-08T09:00:00.000Z",
                "latestRunRequestedAt": "2026-10-08T11:59:00.000Z",
            }),
        );
        let sent_last = busy(
            "sent-last",
            json!({"latestUserAuthoredMessageAt": "2026-10-08T10:00:00.000Z"}),
        );
        let launched = busy(
            "launched",
            json!({"latestUserAuthoredMessageAt": null, "createdAt": "2026-10-08T08:00:00.000Z"}),
        );
        // A server that leaves the field out has the run's request stand in for the send.
        let older = busy(
            "older",
            json!({"latestRunRequestedAt": "2026-10-08T09:30:00.000Z"}),
        );
        assert!(older.get("latestUserAuthoredMessageAt").is_none());
        let mut threads = vec![launched, sent_first, older, sent_last];
        let order = |threads: &[Value]| {
            let returns = Returns::default();
            let shelves = shelves(threads, ALL, now(), Some(&returns));
            ids(&shelves.working)
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(&threads),
            ["sent-last", "older", "sent-first", "launched"]
        );
        assert_eq!(sent_ms(&threads[0]), ms("2026-10-08T08:00:00.000Z"));
        assert_eq!(sent_ms(&threads[1]), ms("2026-10-08T09:00:00.000Z"));

        // A wake that asks for another run doesn't move `sent-first`.
        threads[1] = with(
            threads[1].clone(),
            json!({"latestRunRequestedAt": "2026-10-08T11:59:30.000Z"}),
        );
        assert_eq!(
            order(&threads),
            ["sent-last", "older", "sent-first", "launched"]
        );
        // A message the user sends does.
        threads[1] = with(
            threads[1].clone(),
            json!({"latestUserAuthoredMessageAt": "2026-10-08T11:59:40.000Z"}),
        );
        assert_eq!(
            order(&threads),
            ["sent-first", "sent-last", "older", "launched"]
        );
        // On the older server a wake moves the thread, as it does in the GUI.
        threads[2] = with(
            threads[2].clone(),
            json!({"latestRunRequestedAt": "2026-10-08T11:59:50.000Z"}),
        );
        assert_eq!(
            order(&threads),
            ["older", "sent-first", "sent-last", "launched"]
        );
    }

    #[test]
    fn active_lists_the_latest_return_first() {
        // As in the nightly's test: the thread that finished last tops a newer one and a
        // reopened one.
        let new = with(
            thread("new"),
            json!({"createdAt": "2026-10-08T11:00:00.000Z"}),
        );
        let finished_now = with(
            thread("old-finished-now"),
            json!({
                "createdAt": "2026-10-01T09:00:00.000Z",
                "latestRunId": "r",
                "status": "completed",
                "latestRunRequestedAt": "2026-10-01T09:00:00.000Z",
                "latestRunCompletedAt": "2026-10-08T11:50:00.000Z",
            }),
        );
        let reopened = with(
            thread("reopened"),
            json!({"createdAt": "2026-10-02T09:00:00.000Z", "unsettledAt": "2026-10-08T11:30:00.000Z"}),
        );
        let threads = vec![new, finished_now, reopened];
        let returns = Returns::default();
        let shelves = shelves(&threads, ALL, now(), Some(&returns));
        assert_eq!(ids(&shelves.inbox), ["old-finished-now", "reopened", "new"]);

        // A return the server doesn't stamp, seen at 11:55, tops them all.
        let returns = Returns {
            working: Some(HashSet::new()),
            at: HashMap::from([("new".to_string(), ms("2026-10-08T11:55:00.000Z"))]),
        };
        assert_eq!(
            returned_ms(&threads[0], &returns),
            ms("2026-10-08T11:55:00.000Z")
        );
        let shelves = super::shelves(&threads, ALL, now(), Some(&returns));
        assert_eq!(ids(&shelves.inbox), ["new", "old-finished-now", "reopened"]);
        // A run's request counts as a return too, and a finish time that doesn't parse
        // counts as none.
        let asked = with(
            thread("asked"),
            json!({"latestRunId": "r", "status": "interrupted", "latestRunRequestedAt": "2026-10-08T11:40:00.000Z", "latestRunCompletedAt": "soon"}),
        );
        assert_eq!(
            returned_ms(&asked, &Returns::default()),
            ms("2026-10-08T11:40:00.000Z")
        );
    }

    #[test]
    fn a_thread_is_stamped_only_when_it_is_seen_leaving_working() {
        let at = |minute: u32| ms(&format!("2026-10-08T11:{minute:02}:00.000Z"));
        let working = busy("a", json!({}));
        let asking = with(
            working.clone(),
            json!({"pendingRuntimeRequest": {"kind": "command"}}),
        );
        let resting = thread("b");
        let mut returns = Returns::default();

        // The first look only notes who works, so it stamps nothing, even for a thread already
        // waiting on the user.
        returns.observe(&[asking.clone(), resting.clone()], at(1));
        assert!(returns.at.is_empty());
        returns.observe(&[working.clone(), resting.clone()], at(2));
        assert!(
            returns.at.is_empty(),
            "a thread that starts working isn't a return"
        );
        returns.observe(&[working.clone(), resting.clone()], at(3));
        assert!(returns.at.is_empty());

        // It asks for approval partway through the run, which no server field marks.
        returns.observe(&[asking.clone(), resting.clone()], at(4));
        assert_eq!(returns.returned_at(&asking), Some(at(4)));
        assert_eq!(returns.returned_at(&resting), None, "never worked");
        // Later looks keep the first stamp, and so does working again.
        returns.observe(&[asking.clone(), resting.clone()], at(5));
        assert_eq!(returns.returned_at(&asking), Some(at(4)));
        returns.observe(&[working.clone(), resting.clone()], at(6));
        assert_eq!(returns.returned_at(&working), Some(at(4)));
        // The next return replaces it.
        let finished = with(
            working.clone(),
            json!({"status": "completed", "activeRunId": null, "activityRunStatus": null}),
        );
        returns.observe(&[finished.clone(), resting.clone()], at(7));
        assert_eq!(returns.returned_at(&finished), Some(at(7)));

        // A thread that goes takes its stamp with it, and coming back isn't a return.
        returns.observe(std::slice::from_ref(&working), at(8));
        returns.observe(std::slice::from_ref(&resting), at(9));
        assert!(returns.at.is_empty());
        returns.observe(&[asking.clone(), resting.clone()], at(10));
        assert!(returns.at.is_empty());

        // The sidebar looks at every thread, listed or not, and forgets what it saw while the
        // shelf is off, so turning it on again starts from a fresh first look.
        let hidden = busy(
            "sub",
            json!({"lineage": {"relationshipToParent": "subagent"}}),
        );
        let mut sidebar = Sidebar::with_choices(ShelfChoices {
            working_open: false,
            ..ShelfChoices::default()
        });
        sidebar.rebuild(std::slice::from_ref(&hidden), ALL, None, at(11));
        let hidden_asks = with(
            hidden,
            json!({"pendingRuntimeRequest": {"kind": "user_input"}}),
        );
        sidebar.rebuild(std::slice::from_ref(&hidden_asks), ALL, None, at(12));
        assert_eq!(sidebar.returns.returned_at(&hidden_asks), Some(at(12)));
        sidebar.choices.working_enabled = false;
        sidebar.rebuild(std::slice::from_ref(&hidden_asks), ALL, None, at(13));
        assert!(sidebar.returns.working.is_none() && sidebar.returns.at.is_empty());
        sidebar.choices.working_enabled = true;
        sidebar.rebuild(std::slice::from_ref(&asking), ALL, None, at(14));
        assert!(sidebar.returns.at.is_empty());
    }

    #[test]
    fn a_thread_that_leaves_working_lands_on_top_of_active() {
        let at = |time: &str| ms(&format!("2026-10-08T{time}.000Z"));
        let new = with(
            thread("new"),
            json!({"createdAt": "2026-10-08T11:00:00.000Z"}),
        );
        let done = with(
            thread("done"),
            json!({
                "createdAt": "2026-10-03T00:00:00.000Z",
                "latestRunId": "r",
                "status": "completed",
                "latestRunRequestedAt": "2026-10-08T11:50:00.000Z",
                "latestRunCompletedAt": "2026-10-08T11:58:00.000Z",
            }),
        );
        // Its run was asked for at 11:56:50, before `done` finished.
        let working = busy("busy", json!({"createdAt": "2026-10-02T00:00:00.000Z"}));
        let mut sidebar = Sidebar::default();
        let mut step = |third: &Value, time: &str| {
            sidebar.rebuild(
                &[new.clone(), done.clone(), third.clone()],
                ALL,
                None,
                at(time),
            );
            sidebar.rows.clone()
        };
        let busy_in_working = [
            card("done"),
            card("new"),
            heading_row(Shelf::Working, true),
            card("busy"),
            heading_row(Shelf::Done, true),
            heading_row(Shelf::Settled, false),
        ];
        let busy_on_top = [
            card("busy"),
            card("done"),
            card("new"),
            heading_row(Shelf::Working, true),
            heading_row(Shelf::Done, false),
            heading_row(Shelf::Settled, false),
        ];
        assert_eq!(step(&working, "11:58:30"), busy_in_working);

        // It asks for approval at 11:59. By its run times alone it would sit under `done`.
        let asking = with(
            working.clone(),
            json!({"pendingRuntimeRequest": {"kind": "command"}}),
        );
        assert!(
            returned_ms(&asking, &Returns::default()) < returned_ms(&done, &Returns::default())
        );
        assert_eq!(step(&asking, "11:59:00"), busy_on_top);

        // Approved, it goes back to Working, and when the run finishes it comes back on top.
        assert_eq!(step(&working, "11:59:30"), busy_in_working);
        let finished = with(
            working,
            json!({
                "status": "completed",
                "activeRunId": null,
                "activityRunStatus": null,
                "latestRunCompletedAt": "2026-10-08T12:00:00.000Z",
            }),
        );
        assert_eq!(step(&finished, "12:00:00"), busy_on_top);
    }

    #[test]
    fn a_closed_working_shelf_counts_its_cards_and_keeps_the_open_one() {
        let threads = vec![
            thread("a"),
            busy(
                "w1",
                json!({"latestUserAuthoredMessageAt": "2026-10-08T11:00:00.000Z"}),
            ),
            busy(
                "w2",
                json!({"latestUserAuthoredMessageAt": "2026-10-08T10:00:00.000Z"}),
            ),
            snoozed("z"),
        ];
        let mut sidebar = Sidebar::with_choices(ShelfChoices {
            working_open: false,
            ..ShelfChoices::default()
        });
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(
            sidebar.rows,
            [
                card("a"),
                heading_row(Shelf::Working, true),
                heading_row(Shelf::Done, false),
                heading_row(Shelf::Snoozed, false),
                card("z"),
                heading_row(Shelf::Settled, true),
            ]
        );
        assert!(sidebar.has_shelf(Shelf::Working));
        assert_eq!(sidebar.working_count, 2);

        // Sending a message from the open thread folds it into the shelf, and its card stays.
        sidebar.rebuild(&threads, ALL, Some("w2"), now());
        assert_eq!(
            sidebar.rows[1..],
            [
                heading_row(Shelf::Working, true),
                card("w2"),
                heading_row(Shelf::Done, true),
                heading_row(Shelf::Snoozed, false),
                card("z"),
                heading_row(Shelf::Settled, true),
            ]
        );
        // Open, the shelf lists every card, newest send first.
        sidebar.choices.working_open = true;
        sidebar.rebuild(&threads, ALL, Some("w2"), now());
        assert_eq!(
            sidebar.rows[1..4],
            [heading_row(Shelf::Working, true), card("w1"), card("w2")]
        );
        // With nothing working, the heading stays and counts none, open or closed.
        for open in [true, false] {
            sidebar.choices.working_open = open;
            sidebar.rebuild(&[thread("a")], ALL, None, now());
            assert_eq!(
                sidebar.rows[..2],
                [card("a"), heading_row(Shelf::Working, true)]
            );
            assert!(sidebar.has_shelf(Shelf::Working));
            assert_eq!(sidebar.working_count, 0);
        }
        // A closed shelf with nothing above it leads the list.
        sidebar.choices.working_open = false;
        sidebar.rebuild(&threads[1..3], ALL, None, now());
        assert_eq!(
            sidebar.rows,
            [
                heading_row(Shelf::Working, false),
                heading_row(Shelf::Done, false),
                heading_row(Shelf::Settled, false),
            ]
        );
        assert_eq!(sidebar.selected_thread_id(), None);
    }

    #[test]
    fn the_working_heading_opens_and_closes_the_shelf() {
        let listed = shell(vec![
            thread("a"),
            busy(
                "w1",
                json!({"latestUserAuthoredMessageAt": "2026-10-08T11:00:00.000Z"}),
            ),
            busy(
                "w2",
                json!({"latestUserAuthoredMessageAt": "2026-10-08T10:00:00.000Z"}),
            ),
        ]);
        let theme = Theme::new(Depth::TrueColor);
        let mut sidebar = Sidebar::with_choices(ShelfChoices {
            working_open: false,
            ..ShelfChoices::default()
        });
        sidebar.rebuild(&listed.threads, ALL, None, now());
        let buffer = render(&mut sidebar, Some(&listed), None, (30, 24));
        let lines = text(&buffer);
        // Closed, it counts its cards, and its chevron points right.
        let y = row_of(&lines, "Working (2) ─");
        assert!(lines[y as usize].trim_end().ends_with("─ ›"), "{lines:#?}");
        assert_eq!(buffer[(1, y)].fg, theme.sidebar_muted);
        assert!(!lines.iter().any(|line| line.contains("Thread w")));
        // Only the heading's own row takes the click, not the gap above it or a card.
        assert_eq!(sidebar.shelf_at(y), Some(Shelf::Working));
        assert_eq!(sidebar.shelf_at(y - 1), None);
        assert_eq!(sidebar.shelf_at(row_of(&lines, "Thread a")), None);
        assert_eq!(sidebar.thread_at(y), None);

        // The app opens the shelf, then lays the list out again. Open, it still counts its
        // cards, and its chevron points down.
        assert_eq!(sidebar.toggle(Shelf::Working), Some(true));
        sidebar.rebuild(&listed.threads, ALL, None, now());
        let lines = text(&render(&mut sidebar, Some(&listed), None, (30, 24)));
        let y = row_of(&lines, "Working (2) ─");
        assert!(lines[y as usize].trim_end().ends_with("─ ⌄"), "{lines:#?}");
        assert!(y < row_of(&lines, "Thread w1"));
        assert!(row_of(&lines, "Thread w1") < row_of(&lines, "Thread w2"));
        assert_eq!(sidebar.shelf_at(y), Some(Shelf::Working));
        assert_eq!(sidebar.thread_at(row_of(&lines, "Thread w2")), Some("w2"));

        // Closed again, the cards go.
        assert_eq!(sidebar.toggle(Shelf::Working), Some(false));
        sidebar.rebuild(&listed.threads, ALL, None, now());
        let lines = text(&render(&mut sidebar, Some(&listed), None, (30, 24)));
        assert!(lines.iter().any(|line| line.contains("Working (2)")));
        assert!(!lines.iter().any(|line| line.contains("Thread w")));
    }

    #[test]
    fn a_fold_forgets_the_cards_and_headings_the_last_frame_drew() {
        let listed = shell(vec![
            thread("a"),
            busy("w", json!({})),
            unseen("d", "11:00"),
        ]);
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&listed.threads, ALL, None, now());
        let lines = text(&render(&mut sidebar, Some(&listed), None, (30, 24)));
        let heading = row_of(&lines, "Working (1)");
        let card = row_of(&lines, "Thread w");
        assert_eq!(sidebar.shelf_at(heading), Some(Shelf::Working));
        assert_eq!(sidebar.thread_at(card), Some("w"));
        let (list, border) = (sidebar.list, sidebar.border);

        // Closing Working takes its card away and moves its heading down to the shelves at
        // the bottom. Until the next frame no row takes a click as a card or a heading, and
        // the list keeps its place, so the app knows a click there lands on nothing.
        assert_eq!(sidebar.toggle(Shelf::Working), Some(false));
        sidebar.rebuild(&listed.threads, ALL, None, now());
        for y in 0..24 {
            assert_eq!(sidebar.thread_at(y), None, "row {y}");
            assert_eq!(sidebar.shelf_at(y), None, "row {y}");
        }
        assert_eq!((sidebar.list, sidebar.border), (list, border));

        // The next frame draws the heading where it is now, and the card's old rows take
        // nothing.
        let lines = text(&render(&mut sidebar, Some(&listed), None, (30, 24)));
        let moved = row_of(&lines, "Working (1)");
        assert!(moved > heading, "{lines:#?}");
        assert_eq!(sidebar.shelf_at(moved), Some(Shelf::Working));
        assert_eq!(sidebar.shelf_at(heading), None);
        assert_eq!(sidebar.thread_at(card), None);
        assert_eq!(sidebar.thread_at(row_of(&lines, "Thread d")), Some("d"));

        // A shelf the list has no heading for changes nothing, so it forgets nothing either.
        assert_eq!(sidebar.toggle(Shelf::Snoozed), None);
        assert_eq!(sidebar.shelf_at(moved), Some(Shelf::Working));
    }

    #[test]
    fn a_working_heading_out_of_view_takes_no_click() {
        let mut threads: Vec<Value> = (1..=6)
            .map(|n| {
                with(
                    thread(&format!("a{n}")),
                    json!({"createdAt": format!("2026-10-0{}T00:00:00.000Z", 8 - n)}),
                )
            })
            .collect();
        threads.push(busy("w", json!({})));
        let listed = shell(threads);
        let mut sidebar = Sidebar::with_choices(ShelfChoices {
            working_open: false,
            ..ShelfChoices::default()
        });
        sidebar.rebuild(&listed.threads, ALL, None, now());
        // Fourteen rows leave twelve for the list, which a1 to a3 fill.
        let size = (30, 14);
        let lines = text(&render(&mut sidebar, Some(&listed), None, size));
        assert!(!lines.iter().any(|line| line.contains("Working")));
        assert!((0..14).all(|y| sidebar.shelf_at(y).is_none()));

        // At the bottom of the inbox the headings come into view under a6, and take the click.
        for _ in 0..5 {
            sidebar.move_selection(1);
        }
        assert_eq!(sidebar.selected_thread_id(), Some("a6"));
        let lines = text(&render(&mut sidebar, Some(&listed), None, size));
        let y = row_of(&lines, "Working (1)");
        assert!(row_of(&lines, "Thread a6") < y);
        assert_eq!(sidebar.shelf_at(y), Some(Shelf::Working));
        assert_eq!(sidebar.shelf_at(y + 1), Some(Shelf::Done));
        assert_eq!(sidebar.shelf_at(y + 2), Some(Shelf::Settled));
        // Too small to draw, nothing takes the click.
        render(&mut sidebar, Some(&listed), None, (5, 14));
        assert!((0..14).all(|y| sidebar.shelf_at(y).is_none()));
    }

    #[test]
    fn closing_the_shelf_moves_the_highlight_off_the_cards_it_hides() {
        let sent =
            |at: &str| json!({"latestUserAuthoredMessageAt": format!("2026-10-08T{at}:00.000Z")});
        let threads = vec![
            thread("a"),
            busy("w1", sent("11:00")),
            busy("w2", sent("10:00")),
            busy("w3", sent("09:00")),
            snoozed("z"),
        ];
        let closed_on = |highlight: &str, open: Option<&str>| {
            let mut sidebar = Sidebar::default();
            sidebar.rebuild(&threads, ALL, open, now());
            assert!(sidebar.select(highlight));
            assert_eq!(sidebar.toggle(Shelf::Working), Some(false));
            sidebar.rebuild(&threads, ALL, open, now());
            sidebar.selected_thread_id().map(str::to_string)
        };
        // A hidden card gives the highlight to the first card under the heading: the open
        // thread's, else the next shelf's.
        assert_eq!(closed_on("w3", Some("w2")).as_deref(), Some("w2"));
        assert_eq!(closed_on("w1", None).as_deref(), Some("z"));
        // The open thread's card stays, and so does a highlight outside the shelf.
        assert_eq!(closed_on("w2", Some("w2")).as_deref(), Some("w2"));
        assert_eq!(closed_on("a", None).as_deref(), Some("a"));
        assert_eq!(closed_on("z", Some("w2")).as_deref(), Some("z"));
        // With no card below the shelf, the last card above it.
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads[..4], ALL, None, now());
        assert!(sidebar.select("w2"));
        sidebar.toggle(Shelf::Working);
        sidebar.rebuild(&threads[..4], ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("a"));

        // A list scrolled to a card the shelf hides keeps the heading on top instead.
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert!(sidebar.select("w3"));
        sidebar.offset = sidebar
            .rows
            .iter()
            .position(|row| *row == card("w2"))
            .unwrap();
        sidebar.toggle(Shelf::Working);
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(
            sidebar.rows[sidebar.offset],
            heading_row(Shelf::Working, true)
        );
        assert_eq!(sidebar.selected_thread_id(), Some("z"));

        // A highlighted thread that starts working while the shelf is closed hands the
        // highlight to the next card down in the inbox, as when a thread goes.
        let resting = vec![
            with(
                thread("b1"),
                json!({"createdAt": "2026-10-03T00:00:00.000Z"}),
            ),
            with(
                thread("b2"),
                json!({"createdAt": "2026-10-02T00:00:00.000Z"}),
            ),
            with(
                thread("b3"),
                json!({"createdAt": "2026-10-01T00:00:00.000Z"}),
            ),
        ];
        let mut sidebar = Sidebar::with_choices(ShelfChoices {
            working_open: false,
            ..ShelfChoices::default()
        });
        sidebar.rebuild(&resting, ALL, None, now());
        assert!(sidebar.select("b2"));
        let mut started = resting;
        started[1] = with(started[1].clone(), running());
        sidebar.rebuild(&started, ALL, None, now());
        assert_eq!(sidebar.selected_thread_id(), Some("b3"));
    }

    #[test]
    fn a_closed_working_shelf_needs_no_tick() {
        let listed = shell(vec![
            thread("a"),
            busy("w1", json!({})),
            busy("w2", json!({})),
        ]);
        let draw = |sidebar: &mut Sidebar, open: Option<&str>| {
            sidebar.rebuild(&listed.threads, ALL, open, now());
            text(&render(sidebar, Some(&listed), open, (30, 24)))
        };
        // Both busy cards hide behind the heading, so nothing on screen moves.
        let mut sidebar = Sidebar::with_choices(ShelfChoices {
            working_open: false,
            ..ShelfChoices::default()
        });
        let lines = draw(&mut sidebar, None);
        assert!(lines.iter().any(|line| line.contains("Working (2)")));
        assert!(!lines.iter().any(|line| line.contains("Thread w")));
        assert!(!sidebar.drew_working());
        // The open thread's card shows, spinner and all, and needs the tick.
        let lines = draw(&mut sidebar, Some("w1"));
        assert!(lines[row_of(&lines, "Thread w1") as usize - 1].contains("Working"));
        assert!(sidebar.drew_working());
        // Open, every card shows.
        sidebar.toggle(Shelf::Working);
        draw(&mut sidebar, None);
        assert!(sidebar.drew_working());
        // Off, the same threads are inbox cards and tick as before.
        let mut sidebar = Sidebar::with_choices(ShelfChoices {
            working_enabled: false,
            ..ShelfChoices::default()
        });
        let lines = draw(&mut sidebar, None);
        assert!(row_of(&lines, "Thread w1") < row_of(&lines, "Done (0)"));
        assert!(!lines.iter().any(|line| line.contains("Working (")));
        assert!(sidebar.drew_working());
    }

    // ---- the Done shelf ----

    /// A thread whose run finished at `at` on 2026-10-08, after the visit at 09:00, so it
    /// reads Done.
    fn unseen(id: &str, at: &str) -> Value {
        with(
            thread(id),
            json!({
                "latestRunId": "r",
                "status": "completed",
                "latestRunCompletedAt": format!("2026-10-08T{at}:00.000Z"),
                "lastVisitedAt": "2026-10-08T09:00:00.000Z",
            }),
        )
    }

    #[test]
    fn unseen_finishes_leave_the_inbox_for_the_done_shelf() {
        let seen = with(
            unseen("seen", "10:00"),
            json!({"lastVisitedAt": "2026-10-08T10:30:00.000Z"}),
        );
        let threads = vec![
            unseen("d1", "10:00"),
            seen,
            unseen("d2", "11:00"),
            with(
                unseen("pinned", "11:30"),
                json!({"pinnedAt": "2026-10-02T00:00:00.000Z"}),
            ),
            with(
                unseen("settled", "11:30"),
                json!({"settledOverride": "settled"}),
            ),
            thread("rest"),
        ];
        // With the Working shelf on, the inbox goes by return, so the latest finish leads.
        let returns = Returns::default();
        let on = shelves(&threads, ALL, now(), Some(&returns));
        assert_eq!(ids(&on.done), ["d2", "d1"]);
        assert_eq!(ids(&on.inbox), ["seen", "rest"]);
        // A pinned or settled thread keeps its shelf whatever its word.
        assert_eq!(status_word(&threads[3], now()), Some(Word::Done));
        assert_eq!(ids(&on.pinned), ["pinned"]);
        assert_eq!(ids(&on.settled), ["settled"]);

        // With it off, Done keeps its shelf, in the inbox's arranged order.
        let off = shelves(&threads, ALL, now(), None);
        assert_eq!(ids(&off.done), ["d1", "d2"]);
        assert_eq!(ids(&off.inbox), ["rest", "seen"]);
        assert!(off.working.is_empty());
    }

    #[test]
    fn a_closed_done_shelf_counts_its_threads_and_keeps_the_open_one() {
        let threads = vec![thread("a"), unseen("d1", "11:00"), unseen("d2", "10:00")];
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(
            sidebar.rows,
            [
                card("a"),
                heading_row(Shelf::Working, true),
                heading_row(Shelf::Done, false),
                card("d1"),
                card("d2"),
                heading_row(Shelf::Settled, true),
            ]
        );
        assert_eq!(sidebar.done_count, 2);

        // Closing it moves the highlight off the card it hides, to the next card below or
        // else the last one above.
        assert!(sidebar.select("d2"));
        assert_eq!(sidebar.toggle(Shelf::Done), Some(false));
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(
            sidebar.rows,
            [
                card("a"),
                heading_row(Shelf::Working, true),
                heading_row(Shelf::Done, false),
                heading_row(Shelf::Settled, false),
            ]
        );
        assert_eq!(sidebar.done_count, 2);
        assert_eq!(sidebar.selected_thread_id(), Some("a"));
        // The open thread keeps its card on the closed shelf.
        sidebar.rebuild(&threads, ALL, Some("d2"), now());
        assert_eq!(
            sidebar.rows[3..],
            [card("d2"), heading_row(Shelf::Settled, true)]
        );
        // Opened again, it lists them all.
        assert_eq!(sidebar.toggle(Shelf::Done), Some(true));
        sidebar.rebuild(&threads, ALL, None, now());
        assert_eq!(sidebar.rows[3..5], [card("d1"), card("d2")]);
    }

    #[test]
    fn only_a_listed_shelf_that_folds_opens_and_closes() {
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&[], ALL, None, now());
        for shelf in [Shelf::Working, Shelf::Done, Shelf::Settled] {
            assert!(!sidebar.has_shelf(shelf), "{shelf:?}");
            assert_eq!(sidebar.toggle(shelf), None);
        }
        sidebar.rebuild(&[thread("a")], ALL, None, now());
        for shelf in [Shelf::Pinned, Shelf::Inbox, Shelf::Snoozed] {
            assert!(!sidebar.has_shelf(shelf), "{shelf:?}");
            assert_eq!(sidebar.toggle(shelf), None);
        }
        assert_eq!(sidebar.choices, ShelfChoices::default());
        assert_eq!(sidebar.toggle(Shelf::Working), Some(false));
        assert_eq!(sidebar.toggle(Shelf::Done), Some(false));
        assert_eq!(sidebar.toggle(Shelf::Settled), Some(true));
        assert_eq!(
            sidebar.choices,
            ShelfChoices {
                working_enabled: true,
                working_open: false,
                done_open: false,
                settled_open: true,
            }
        );
    }

    // ---- layout ----

    #[test]
    fn a_settled_thread_takes_one_line_with_its_age_or_its_word() {
        let listed = shell(vec![
            thread("a"),
            with(
                thread("s1"),
                json!({"settledOverride": "settled", "settledAt": "2026-10-08T09:00:00.000Z"}),
            ),
            with(
                thread("s2"),
                json!({
                    "settledOverride": "settled",
                    "settledAt": "2026-10-06T12:00:00.000Z",
                    "latestRunId": "r",
                    "status": "failed",
                }),
            ),
        ]);
        let mut sidebar = Sidebar::with_choices(ShelfChoices {
            settled_open: true,
            ..ShelfChoices::default()
        });
        sidebar.rebuild(&listed.threads, ALL, None, now());
        let buffer = render(&mut sidebar, Some(&listed), Some("s2"), (30, 24));
        let lines = text(&buffer);
        // Newest settled first, a line each with no gap, at the bottom of the list.
        let (s1, s2) = (row_of(&lines, "Thread s1"), row_of(&lines, "Thread s2"));
        assert_eq!((row_of(&lines, "Settled (2)"), s1, s2), (21, 22, 23));
        assert!(lines[s1 as usize].trim_end().ends_with("3h"));
        assert!(lines[s2 as usize].trim_end().ends_with("Failed"));
        // Each row takes clicks on its own line only.
        assert_eq!(sidebar.thread_at(s1), Some("s1"));
        assert_eq!(sidebar.thread_at(s2), Some("s2"));
        assert_eq!(sidebar.thread_at(s1 - 1), None);

        // The bar, the badge's two letters and a space come before the title. The badge and
        // a title that isn't open take the muted tone. The open thread's title is bold in the
        // sidebar's text color, on the open row's background.
        let theme = Theme::new(Depth::TrueColor);
        assert_eq!(buffer[(2, s1)].symbol(), "D");
        assert_eq!(buffer[(2, s1)].fg, theme.sidebar_muted);
        assert_eq!(buffer[(5, s1)].symbol(), "T");
        assert_eq!(buffer[(5, s1)].fg, theme.sidebar_muted);
        assert!(!buffer[(5, s1)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(5, s2)].fg, theme.sidebar_fg);
        assert!(buffer[(5, s2)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(5, s2)].bg, theme.row_active);
        assert_eq!(buffer[(26, s2)].fg, theme.error);
    }

    #[test]
    fn the_open_thread_shows_by_its_bold_title_as_well_as_its_background() {
        let listed = shell(vec![thread("a"), thread("b")]);
        for depth in [Depth::TrueColor, Depth::Indexed] {
            let theme = Theme::new(depth);
            let mut sidebar = Sidebar::default();
            sidebar.rebuild(&listed.threads, ALL, Some("b"), now());
            assert!(sidebar.select("a"));
            let buffer = render_in(depth, &mut sidebar, Some(&listed), Some("b"), (30, 24));
            let lines = text(&buffer);
            let (a, b) = (row_of(&lines, "Thread a"), row_of(&lines, "Thread b"));
            // The highlight is a bar in the primary color on the selected row's background.
            assert_eq!(buffer[(1, a)].symbol(), "▎", "{depth:?}");
            assert_eq!(buffer[(1, a)].fg, theme.primary);
            assert_eq!(buffer[(2, a)].bg, theme.row_selected);
            assert!(!buffer[(2, a)].modifier.contains(Modifier::BOLD));
            // The open thread has no bar while the highlight is elsewhere, the open row's
            // background and a bold title, which shows even where the backgrounds look alike.
            assert_eq!(buffer[(1, b)].symbol(), " ");
            assert_eq!(buffer[(2, b)].bg, theme.row_active);
            assert_eq!(buffer[(2, b)].fg, theme.sidebar_fg);
            assert!(buffer[(2, b)].modifier.contains(Modifier::BOLD));
            // The headings keep their tone at either depth.
            let working = row_of(&lines, "Working (0)");
            assert_eq!(buffer[(1, working)].fg, theme.sidebar_muted);
        }
    }

    #[test]
    fn a_tall_sidebar_keeps_a_blank_row_over_the_wordmark_and_another_over_the_list() {
        let listed = shell(vec![thread("a")]);
        // The heights of 120x36, 80x24, 40x12 and 10x5 screens, and either side of the cut.
        for (height, top) in [(36, 1), (24, 1), (20, 1), (19, 0), (12, 0), (5, 0)] {
            let mut sidebar = Sidebar::default();
            sidebar.rebuild(&listed.threads, ALL, None, now());
            let lines = text(&render(&mut sidebar, Some(&listed), None, (30, height)));
            assert_eq!(row_of(&lines, "T3 Code"), top, "{height}");
            assert_eq!(sidebar.list.y, 2 + 2 * top, "{height}");
            assert_eq!(sidebar.list.bottom(), height, "{height}");
            if top == 1 {
                assert!(lines[0].trim().is_empty() && lines[3].trim().is_empty());
            }
            // The card's first line is on screen at every height.
            assert_eq!(sidebar.thread_at(sidebar.list.y), Some("a"), "{height}");
        }
    }

    #[test]
    fn the_shelves_sit_at_the_bottom_only_while_the_whole_list_fits() {
        let listed = shell(vec![thread("a"), thread("b")]);
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&listed.threads, ALL, None, now());
        // Two cards and three headings take twelve lines. Twenty-four rows leave twenty for
        // the list, so the eight left over go between the inbox and Working.
        let lines = text(&render(&mut sidebar, Some(&listed), None, (30, 24)));
        assert_eq!(row_of(&lines, "Thread b"), 9);
        assert!(lines[12..20].iter().all(|line| line.trim().is_empty()));
        assert_eq!(row_of(&lines, "Working (0)"), 21);
        assert_eq!(row_of(&lines, "Settled (0)"), 23);
        // Twelve rows leave ten, two short, so the list starts at the top and its end waits
        // below the screen.
        let lines = text(&render(&mut sidebar, Some(&listed), None, (30, 12)));
        assert_eq!(row_of(&lines, "Thread a"), 3);
        assert_eq!(row_of(&lines, "Working (0)"), 11);
        assert!(!lines.iter().any(|line| line.contains("Settled")));
        // On the last card, the headings under it come into view.
        sidebar.move_selection(1);
        let lines = text(&render(&mut sidebar, Some(&listed), None, (30, 12)));
        assert_eq!(row_of(&lines, "Thread b"), 3);
        assert_eq!(row_of(&lines, "Settled (0)"), 9);
        assert_eq!(sidebar.shelf_at(9), Some(Shelf::Settled));
    }

    // ---- synthetic capture for review ----

    /// Writes the real renderer's styled output for a few invented sidebars, as ANSI text, to
    /// the directory in `T3TERM_SIDEBAR_CAPTURE`. Without it the test does nothing. Every name
    /// is made up, so the captures can be shared.
    #[test]
    fn capture_sidebar_for_review() {
        let Some(dir) = std::env::var_os("T3TERM_SIDEBAR_CAPTURE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let demo = demo_shell();
        let size = std::cell::Cell::new((34, 40));
        let depth = std::cell::Cell::new(Depth::TrueColor);
        let capture =
            |name: &str, sidebar: &mut Sidebar, shell: Option<&ShellState>, open: Option<&str>| {
                let buffer = render_in(depth.get(), sidebar, shell, open, size.get());
                std::fs::write(dir.join(format!("sidebar-{name}.ans")), ansi(&buffer)).unwrap();
                std::fs::write(
                    dir.join(format!("sidebar-{name}.txt")),
                    text(&buffer).join("\n") + "\n",
                )
                .unwrap();
            };

        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&demo.threads, ALL, Some("t-search"), now());
        assert!(sidebar.select("t-search"));
        capture("shelves", &mut sidebar, Some(&demo), Some("t-search"));
        depth.set(Depth::Indexed);
        capture(
            "shelves-indexed",
            &mut sidebar,
            Some(&demo),
            Some("t-search"),
        );
        depth.set(Depth::TrueColor);

        sidebar.toggle(Shelf::Settled);
        sidebar.rebuild(&demo.threads, ALL, Some("t-search"), now());
        capture("settled-open", &mut sidebar, Some(&demo), Some("t-search"));

        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&demo.threads, ALL, Some("t-notes"), now());
        assert!(sidebar.select("t-notes"));
        capture(
            "open-settled-thread",
            &mut sidebar,
            Some(&demo),
            Some("t-notes"),
        );

        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&demo.threads, Capabilities::default(), None, now());
        capture("old-server", &mut sidebar, Some(&demo), None);

        let empty = shell(Vec::new());
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&empty.threads, ALL, None, now());
        capture("empty", &mut sidebar, Some(&empty), None);

        // Every status word, at the usual width and at the narrowest the app draws, with the
        // shelves as a new settings file has them.
        let statuses = status_shell();
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&statuses.threads, ALL, None, now());
        size.set((34, 46));
        capture("statuses", &mut sidebar, Some(&statuses), None);
        size.set((26, 46));
        capture("statuses-narrow", &mut sidebar, Some(&statuses), None);

        // The same threads with the Working shelf off, as the GUI starts.
        let mut sidebar = Sidebar::with_choices(ShelfChoices {
            working_enabled: false,
            ..ShelfChoices::default()
        });
        sidebar.rebuild(&statuses.threads, ALL, None, now());
        size.set((34, 46));
        capture("working-off", &mut sidebar, Some(&statuses), None);

        // Working and Done closed, each with nothing but the open thread's card.
        let mut sidebar = Sidebar::default();
        sidebar.rebuild(&statuses.threads, ALL, None, now());
        sidebar.toggle(Shelf::Working);
        sidebar.toggle(Shelf::Done);
        sidebar.rebuild(&statuses.threads, ALL, Some("t-c"), now());
        assert!(sidebar.select("t-c"));
        capture("closed", &mut sidebar, Some(&statuses), Some("t-c"));
        sidebar.rebuild(&statuses.threads, ALL, Some("t-i"), now());
        assert!(sidebar.select("t-i"));
        capture("closed-done", &mut sidebar, Some(&statuses), Some("t-i"));
    }

    /// Invented threads, one for each status word and one seen thread with none. With the
    /// Working shelf off, all but the Done one list in that order in the inbox.
    fn status_shell() -> ShellState {
        let card = |key: &str, project: &str, title: &str, fields: Value| {
            let id = format!("t-{key}");
            with(
                with(
                    thread(&id),
                    json!({
                        "projectId": project,
                        "title": title,
                        "branch": format!("demo/{key}"),
                        "activeOrderKey": key,
                    }),
                ),
                fields,
            )
        };
        let slept = json!({"snoozedAt": "2026-10-08T09:00:00.000Z", "snoozedUntil": "2026-10-08T11:00:00.000Z"});
        ShellState {
            sequence: 1,
            projects: vec![
                json!({"id": "atlas", "title": "atlas"}),
                json!({"id": "lab", "title": "interface experiments"}),
            ],
            threads: vec![
                card(
                    "a",
                    "atlas",
                    "Run the database migration",
                    with(
                        running(),
                        json!({"pendingRuntimeRequest": {"kind": "command"}}),
                    ),
                ),
                card(
                    "b",
                    "lab",
                    "Pick a chart palette",
                    json!({"pendingRuntimeRequest": {"kind": "user_input"}}),
                ),
                card("c", "atlas", "Speed up the search index", running()),
                card(
                    "d",
                    "lab",
                    "Get the test suite green",
                    with(
                        running(),
                        json!({
                            "goal": {"objective": "All tests pass", "status": "active"},
                            "activityRunStartedAt": "2026-10-08T10:48:00.000Z",
                        }),
                    ),
                ),
                card(
                    "e",
                    "atlas",
                    "Audit the dependencies",
                    json!({
                        "latestRunId": "r",
                        "status": "completed",
                        "latestRunCompletedAt": "2026-10-08T11:58:00.000Z",
                        "pendingBackgroundTasks": [{"taskId": "s", "kind": "subagent"}],
                    }),
                ),
                card(
                    "f",
                    "lab",
                    "Port the layout engine",
                    json!({"latestRunId": "r", "status": "failed", "lastErrorClass": "usage_limit"}),
                ),
                card(
                    "g",
                    "atlas",
                    "Rotate the signing keys",
                    json!({"latestRunId": "r", "status": "failed", "lastErrorClass": "provider_error"}),
                ),
                card("h", "lab", "Revisit the color tokens", slept),
                card(
                    "i",
                    "atlas",
                    "Write the release notes",
                    finished(json!("2026-10-08T10:00:00.000Z")),
                ),
                card(
                    "j",
                    "lab",
                    "Sketch the settings page",
                    with(
                        finished(json!("2026-10-08T11:30:00.000Z")),
                        json!({"latestUserMessageAt": "2026-10-08T10:50:00.000Z"}),
                    ),
                ),
            ],
            synchronized: true,
        }
    }

    /// Invented projects and threads covering every shelf and card label.
    fn demo_shell() -> ShellState {
        let card = |id: &str, project: &str, title: &str, branch: &str, fields: Value| {
            with(
                with(
                    thread(id),
                    json!({"projectId": project, "title": title, "branch": branch}),
                ),
                fields,
            )
        };
        ShellState {
            sequence: 1,
            projects: vec![
                json!({"id": "atlas", "title": "atlas"}),
                json!({"id": "harbor", "title": "harbor"}),
            ],
            threads: vec![
                card(
                    "t-checklist",
                    "atlas",
                    "Tidy the release checklist",
                    "release/checklist",
                    json!({
                        "pinnedAt": "2026-10-05T00:00:00.000Z",
                        "pinOrderKey": "m",
                        "latestUserMessageAt": "2026-10-08T09:00:00.000Z",
                        "modelSelection": {"instanceId": "claudeAgent"},
                    }),
                ),
                card(
                    "t-search",
                    "harbor",
                    "Speed up the search index",
                    "perf/search-index",
                    json!({
                        "createdAt": "2026-10-08T08:00:00.000Z",
                        "latestRunId": "r1",
                        "activeRunId": "r1",
                        "status": "running",
                        "latestRunStartedAt": "2026-10-08T11:57:00.000Z",
                        "latestRunCompletedAt": null,
                        "activityRunStatus": "running",
                        "activityRunStartedAt": "2026-10-08T11:57:00.000Z",
                    }),
                ),
                card(
                    "t-upload",
                    "atlas",
                    "Fix the flaky upload test",
                    "fix/upload-test",
                    json!({
                        "createdAt": "2026-10-07T00:00:00.000Z",
                        "pendingRuntimeRequest": {"kind": "file-change"},
                        "modelSelection": {"instanceId": "claudeAgent"},
                    }),
                ),
                card(
                    "t-guide",
                    "harbor",
                    "Write the onboarding guide",
                    "docs/onboarding",
                    json!({
                        "createdAt": "2026-10-06T00:00:00.000Z",
                        "latestRunId": "r2",
                        "status": "failed",
                    }),
                ),
                card(
                    "t-charts",
                    "atlas",
                    "Try a dark theme for charts",
                    "",
                    json!({
                        "createdAt": "2026-10-04T00:00:00.000Z",
                        "snoozedUntil": "2026-10-09T09:00:00.000Z",
                        "snoozedAt": "2026-10-08T10:00:00.000Z",
                        "latestUserMessageAt": "2026-10-08T10:00:00.000Z",
                    }),
                ),
                card(
                    "t-notes",
                    "harbor",
                    "Draft the changelog",
                    "docs/changelog",
                    json!({
                        "settledOverride": "settled",
                        "settledAt": "2026-10-08T07:00:00.000Z",
                        "latestUserMessageAt": "2026-10-08T06:00:00.000Z",
                    }),
                ),
                card(
                    "t-api",
                    "atlas",
                    "Plan the API cleanup",
                    "",
                    json!({
                        "settledOverride": "settled",
                        "settledAt": "2026-10-07T07:00:00.000Z",
                        "latestUserMessageAt": "2026-10-07T06:00:00.000Z",
                    }),
                ),
                card(
                    "t-helper",
                    "atlas",
                    "Subagent: scan the logs",
                    "",
                    json!({
                        "lineage": {"parentThreadId": "t-search", "relationshipToParent": "subagent", "rootThreadId": "t-search"},
                    }),
                ),
                card(
                    "t-fork",
                    "harbor",
                    "Speed up the search index (fork)",
                    "perf/search-fork",
                    json!({
                        "createdAt": "2026-10-08T09:00:00.000Z",
                        "lineage": {"parentThreadId": "t-search", "relationshipToParent": "fork", "rootThreadId": "t-search"},
                        "latestUserMessageAt": "2026-10-08T11:00:00.000Z",
                    }),
                ),
            ],
            synchronized: true,
        }
    }

    /// A buffer as ANSI text: 24-bit or indexed colors, bold and dim, one line per row.
    fn ansi(buffer: &Buffer) -> String {
        let area = buffer.area;
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            let mut skip = 0;
            for x in area.left()..area.right() {
                let cell = &buffer[(x, y)];
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                out.push_str(&sgr(cell));
                out.push_str(cell.symbol());
                skip = cell.symbol().width().saturating_sub(1);
            }
            out.push_str("\x1b[0m\n");
        }
        out
    }

    fn sgr(cell: &Cell) -> String {
        let mut codes = vec!["0".to_string()];
        if cell.modifier.contains(Modifier::BOLD) {
            codes.push("1".into());
        }
        if cell.modifier.contains(Modifier::DIM) {
            codes.push("2".into());
        }
        codes.extend(color_code(cell.fg, 38));
        codes.extend(color_code(cell.bg, 48));
        format!("\x1b[{}m", codes.join(";"))
    }

    fn color_code(color: Color, base: u8) -> Option<String> {
        match color {
            Color::Rgb(r, g, b) => Some(format!("{base};2;{r};{g};{b}")),
            Color::Indexed(index) => Some(format!("{base};5;{index}")),
            _ => None,
        }
    }
}
