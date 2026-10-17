//! Terminal sessions on T3's server: one terminal's `terminal.attach` and `terminal.observe`
//! streams, the environment's `subscribeTerminalMetadata` stream, and input, resizes and
//! closes sent to a terminal.
//!
//! Each stream is a bounded stream from `RpcClient::bounded_stream`. T3's Effect `Chunk`s
//! reach the caller as batches of typed events borrowed from the chunk's values, and a
//! chunk's Ack goes out only when the caller accepts its batch. Nothing here reconnects,
//! keeps output or starts a shell on its own. docs/terminal-events.md has the details.
//!
//! Method names, payloads and event shapes follow T3's packages/contracts/src/terminal.ts and
//! rpc.ts at commit f570bd2, nightly v0.0.46-nightly.20261007.2787.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use crate::error::{T3Error, exit};
use crate::rpc::{self, BoundedChunk, BoundedStream, RpcClient, RpcError, StreamLimits};

const ATTACH: &str = "terminal.attach";
const OBSERVE: &str = "terminal.observe";
const WRITE: &str = "terminal.write";
const RESIZE: &str = "terminal.resize";
const CLOSE: &str = "terminal.close";
const METADATA: &str = "subscribeTerminalMetadata";

/// How long a write, a resize or a close waits for T3's answer.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The most UTF-16 code units T3 takes in one `terminal.write` (`TerminalWriteInput`).
pub const WRITE_LIMIT: usize = 65_536;
/// The longest terminal id, in UTF-16 code units (`TerminalIdSchema`).
const TERMINAL_ID_LIMIT: usize = 128;
/// The longest label T3 sends, in UTF-16 code units.
const LABEL_LIMIT: usize = 128;
/// T3's limits on a shell's extra environment (`TerminalEnvSchema`).
const ENV_LIMIT: usize = 128;
const ENV_KEY_LIMIT: usize = 128;
const ENV_VALUE_LIMIT: usize = 8_192;
const MAX_COLS: u16 = 1000;
const MAX_ROWS: u16 = 500;

/// JavaScript's `Number.MAX_SAFE_INTEGER`. Effect's `Schema.Int` takes safe integers only.
const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// Why a terminal input or request failed.
#[derive(Debug, Clone)]
pub enum TerminalError {
    /// The input breaks T3's schema for it, so t3term sent nothing.
    Invalid(&'static str),
    /// T3 or the connection failed the request.
    Rpc(RpcError),
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TerminalError::Invalid(reason) => f.write_str(reason),
            TerminalError::Rpc(error) => error.fmt(f),
        }
    }
}

impl From<RpcError> for TerminalError {
    fn from(error: RpcError) -> Self {
        TerminalError::Rpc(error)
    }
}

impl From<TerminalError> for anyhow::Error {
    fn from(error: TerminalError) -> Self {
        match error {
            TerminalError::Invalid(reason) => T3Error::new("T3_TERMINAL_INVALID", reason)
                .exit(exit::USAGE)
                .into(),
            TerminalError::Rpc(error) => error.into(),
        }
    }
}

fn invalid<T>(reason: &'static str) -> Result<T, TerminalError> {
    Err(TerminalError::Invalid(reason))
}

/// One terminal: the thread it belongs to, and the id the client chose for it. The desktop
/// calls its first terminal `term-1`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TerminalId {
    thread_id: String,
    terminal_id: String,
}

impl TerminalId {
    /// Both ids must be non-blank and already trimmed, since T3 trims them before it looks
    /// the terminal up (`TrimmedNonEmptyString` in packages/contracts/src/baseSchemas.ts). The
    /// terminal id is at most 128 UTF-16 code units.
    pub fn new(thread_id: &str, terminal_id: &str) -> Result<TerminalId, TerminalError> {
        if !trimmed_non_blank(thread_id) {
            return invalid("a thread id must be non-blank, with no space at either end");
        }
        if !trimmed_non_blank(terminal_id) {
            return invalid("a terminal id must be non-blank, with no space at either end");
        }
        if !utf16_at_most(terminal_id, TERMINAL_ID_LIMIT) {
            return invalid("a terminal id must be at most 128 UTF-16 code units");
        }
        Ok(TerminalId {
            thread_id: thread_id.to_string(),
            terminal_id: terminal_id.to_string(),
        })
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn terminal_id(&self) -> &str {
        &self.terminal_id
    }

    /// `{threadId, terminalId}`, T3's `TerminalSessionInput`.
    fn payload(&self) -> Map<String, Value> {
        let mut payload = Map::new();
        payload.insert("threadId".into(), json!(self.thread_id));
        payload.insert("terminalId".into(), json!(self.terminal_id));
        payload
    }
}

/// A terminal's size: 1 to 1000 columns and 1 to 500 rows, T3's range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    cols: u16,
    rows: u16,
}

impl Size {
    pub fn new(cols: u16, rows: u16) -> Result<Size, TerminalError> {
        if !(1..=MAX_COLS).contains(&cols) {
            return invalid("a terminal has 1 to 1000 columns");
        }
        if !(1..=MAX_ROWS).contains(&rows) {
            return invalid("a terminal has 1 to 500 rows");
        }
        Ok(Size { cols, rows })
    }

    pub fn cols(self) -> u16 {
        self.cols
    }

    pub fn rows(self) -> u16 {
        self.rows
    }

    fn write_into(self, payload: &mut Map<String, Value>) {
        payload.insert("cols".into(), json!(self.cols));
        payload.insert("rows".into(), json!(self.rows));
    }
}

/// What T3 starts a new shell with: its working directory, the thread's worktree and extra
/// environment variables. t3term sends one only in an attach whose caller asks for a shell.
#[derive(Clone, PartialEq, Eq)]
pub struct Launch {
    cwd: String,
    worktree_path: Option<String>,
    env: BTreeMap<String, String>,
}

impl Launch {
    /// A shell in `cwd`, for a thread with the worktree `worktree_path` if it has one. Both
    /// paths must be non-blank and already trimmed, as T3 trims them.
    pub fn new(cwd: &str, worktree_path: Option<&str>) -> Result<Launch, TerminalError> {
        if !trimmed_non_blank(cwd) {
            return invalid("a shell's directory must be non-blank, with no space at either end");
        }
        if worktree_path.is_some_and(|path| !trimmed_non_blank(path)) {
            return invalid("a worktree path must be non-blank, with no space at either end");
        }
        Ok(Launch {
            cwd: cwd.to_string(),
            worktree_path: worktree_path.map(str::to_string),
            env: BTreeMap::new(),
        })
    }

    /// The shell the desktop starts for a thread: in the thread's worktree if it has one,
    /// otherwise in the project's workspace root, with `T3CODE_PROJECT_ROOT` set to the root
    /// and `T3CODE_WORKTREE_PATH` to the worktree (`projectScriptCwd` and
    /// `projectScriptRuntimeEnv` in packages/shared/src/projectScripts.ts, called from
    /// apps/web/src/components/ChatView.tsx).
    ///
    /// Matching the desktop matters. T3 stops a running shell and starts it again when a
    /// `terminal.open` of the same terminal brings a different directory, worktree or
    /// environment (`openWithWorkspaceLease` in apps/server/src/terminal/Manager.ts).
    pub fn for_thread(
        workspace_root: &str,
        worktree_path: Option<&str>,
    ) -> Result<Launch, TerminalError> {
        let launch = Launch::new(worktree_path.unwrap_or(workspace_root), worktree_path)?;
        let launch = launch.with_env("T3CODE_PROJECT_ROOT", workspace_root)?;
        match worktree_path {
            Some(path) => launch.with_env("T3CODE_WORKTREE_PATH", path),
            None => Ok(launch),
        }
    }

    /// Sets one environment variable, replacing any earlier value. A key matches
    /// `^[A-Za-z_][A-Za-z0-9_]*$` and is at most 128 characters, a value is at most 8,192
    /// UTF-16 code units, and a launch holds at most 128 keys.
    pub fn with_env(mut self, key: &str, value: &str) -> Result<Launch, TerminalError> {
        if !env_key(key) {
            return invalid(
                "an environment key is a letter or _ and then letters, digits or _, at most 128 in all",
            );
        }
        if !utf16_at_most(value, ENV_VALUE_LIMIT) {
            return invalid("an environment value must be at most 8192 UTF-16 code units");
        }
        if self.env.len() == ENV_LIMIT && !self.env.contains_key(key) {
            return invalid("a shell takes at most 128 environment keys");
        }
        self.env.insert(key.to_string(), value.to_string());
        Ok(self)
    }

    fn write_into(&self, payload: &mut Map<String, Value>) {
        payload.insert("cwd".into(), json!(self.cwd));
        if let Some(path) = &self.worktree_path {
            payload.insert("worktreePath".into(), json!(path));
        }
        if !self.env.is_empty() {
            payload.insert("env".into(), json!(self.env));
        }
    }
}

impl fmt::Debug for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The keys and not their values, which can hold anything a caller passes a shell.
        f.debug_struct("Launch")
            .field("cwd", &self.cwd)
            .field("worktree_path", &self.worktree_path)
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// One terminal on a connection: its two streams, and the input, resizes and close sent to
/// it.
pub struct Terminal {
    rpc: RpcClient,
    id: TerminalId,
    /// Held through each write, so writes go out one after another.
    input: Mutex<()>,
}

impl Terminal {
    pub fn new(rpc: RpcClient, id: TerminalId) -> Terminal {
        Terminal {
            rpc,
            id,
            input: Mutex::new(()),
        }
    }

    pub fn id(&self) -> &TerminalId {
        &self.id
    }

    /// Whether this terminal's connection takes no more requests, as
    /// [`RpcClient::is_closed`] says. A call that fails with [`RpcError::Disconnected`] while
    /// the connection is still open got no answer in time, and the connection and its streams
    /// go on.
    pub fn is_closed(&self) -> bool {
        self.rpc.is_closed()
    }

    /// Attaches to the terminal with `terminal.attach`, which needs the session's
    /// `terminal:operate` scope.
    ///
    /// Without a `launch`, the request names the terminal and holds `size` if there is one,
    /// and T3 fails it with `TerminalSessionLookupError` if the terminal doesn't exist. With
    /// one, T3 starts a shell from it if the terminal doesn't exist, and ignores it if the
    /// terminal does. A `size` resizes a running shell for everyone who watches it, the
    /// desktop too. t3term never sends `restartIfNotRunning` or `providerInstanceId`, so an
    /// attach never starts a shell that has exited again (`openOrAttachForStream` in
    /// Manager.ts).
    ///
    /// On this stream T3 sends on without an Ack until 8 chunks or 64 KiB wait for one, and
    /// it doesn't cap one chunk's size. The first chunk holds the terminal's whole history.
    pub async fn attach(
        &self,
        launch: Option<&Launch>,
        size: Option<Size>,
        limits: StreamLimits,
    ) -> Result<TerminalStream, RpcError> {
        let mut payload = self.id.payload();
        if let Some(launch) = launch {
            launch.write_into(&mut payload);
        }
        if let Some(size) = size {
            size.write_into(&mut payload);
        }
        let stream = self
            .rpc
            .bounded_stream(ATTACH, Value::Object(payload), limits);
        Ok(TerminalStream(Checked::new(stream.await?)))
    }

    /// Watches the terminal with `terminal.observe`, which needs only `terminal:read`. The
    /// request names the terminal and nothing else. T3 never starts, resizes or restarts a
    /// shell for it, and fails it with `TerminalSessionLookupError` if the terminal doesn't
    /// exist.
    ///
    /// T3 waits for each chunk's Ack before it sends the next. The first chunk holds the
    /// terminal's whole history.
    pub async fn observe(&self, limits: StreamLimits) -> Result<TerminalStream, RpcError> {
        let payload = Value::Object(self.id.payload());
        let stream = self.rpc.bounded_stream(OBSERVE, payload, limits);
        Ok(TerminalStream(Checked::new(stream.await?)))
    }

    /// Types `text` into the shell with `terminal.write`. T3 takes at most 65,536 UTF-16 code
    /// units in one write, so longer text goes in pieces, each cut between two characters.
    /// Each piece waits for T3's answer before the next goes out. Writes made at the same
    /// time through this `Terminal` take turns in the order they began to wait, and their
    /// pieces never mix.
    ///
    /// Empty text is refused, as T3's schema refuses it. If a piece fails, or the caller
    /// cancels the write, the pieces before it have gone to the shell and the rest never go.
    pub async fn write(&self, text: &str) -> Result<(), TerminalError> {
        if text.is_empty() {
            return invalid("terminal input can't be empty");
        }
        let _turn = self.input.lock().await;
        for piece in pieces(text) {
            let mut payload = self.id.payload();
            payload.insert("data".into(), json!(piece));
            self.call(WRITE, payload).await?;
        }
        Ok(())
    }

    /// Resizes the shell with `terminal.resize`, for everyone who watches it. T3 does nothing
    /// for a terminal that doesn't exist or isn't running.
    pub async fn resize(&self, size: Size) -> Result<(), RpcError> {
        let mut payload = self.id.payload();
        size.write_into(&mut payload);
        self.call(RESIZE, payload).await
    }

    /// Closes this terminal with `terminal.close`, which ends its shell, and deletes its
    /// history too if `delete_history`. The request always names the terminal, since T3
    /// closes every terminal on the thread when it doesn't (`close` in Manager.ts).
    pub async fn close(&self, delete_history: bool) -> Result<(), RpcError> {
        let mut payload = self.id.payload();
        payload.insert("deleteHistory".into(), json!(delete_history));
        self.call(CLOSE, payload).await
    }

    async fn call(&self, tag: &str, payload: Map<String, Value>) -> Result<(), RpcError> {
        let calling = self.rpc.call(tag, Value::Object(payload), CALL_TIMEOUT);
        calling.await.map_err(explain)?;
        Ok(())
    }
}

/// Cuts `text` into pieces of at most [`WRITE_LIMIT`] UTF-16 code units, each ending between
/// two characters. A character takes one or two units, so each piece holds at least one.
fn pieces(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (piece, after) = rest.split_at(piece_end(rest));
        rest = after;
        Some(piece)
    })
}

/// The byte length of the longest start of `text` that is at most [`WRITE_LIMIT`] UTF-16
/// code units and ends between two characters.
fn piece_end(text: &str) -> usize {
    // A UTF-8 byte never stands for more than one UTF-16 unit.
    if text.len() <= WRITE_LIMIT {
        return text.len();
    }
    let mut units = 0;
    for (index, character) in text.char_indices() {
        units += character.len_utf16();
        if units > WRITE_LIMIT {
            return index;
        }
    }
    text.len()
}

/// Events from `terminal.attach` or `terminal.observe`: a batch for each chunk T3 sends.
///
/// Neither stream ends when the shell exits or closes. T3 sends `exited` or `closed`, and
/// the stream stays open until T3 or the caller ends it. Dropping it interrupts the stream
/// on T3 and nothing else. The shell goes on running, and nothing is closed.
pub struct TerminalStream(Checked);

impl TerminalStream {
    /// The next batch. Once the stream has ended, the batches already queued come first, then
    /// the error that ended it, if there was one, and from then on `None`, as with
    /// `BoundedStream::next`.
    ///
    /// A chunk with an event t3term can't read never reaches the caller. t3term drops it
    /// unacked, which stops the stream and interrupts it on T3, discards the chunks queued
    /// after it, and returns [`RpcError::Protocol`] naming the event and field. Then `None`.
    ///
    /// Cancelling the call loses nothing, so it is safe in a `select!`.
    pub async fn next(&mut self) -> Option<Result<TerminalBatch, RpcError>> {
        let next = self.0.next(check_event).await?;
        Some(next.map(|chunk| TerminalBatch { chunk }))
    }
}

/// The events of one chunk from `terminal.attach` or `terminal.observe`, in the order T3
/// sent them, borrowed from the chunk.
///
/// The chunk counts toward the stream's limits until the caller accepts the batch. Accept it
/// after applying every event. Dropping it unaccepted stops the stream, as dropping a
/// `BoundedChunk` does.
#[derive(Debug)]
pub struct TerminalBatch {
    chunk: BoundedChunk,
}

impl TerminalBatch {
    /// The batch's events, in the order T3 sent them. A chunk can have none.
    pub fn events(&self) -> impl Iterator<Item = TerminalEvent<'_>> {
        // `TerminalStream::next` read every value before it returned the batch, so none
        // fails here.
        let values = self.chunk.values().iter();
        values.filter_map(|value| parse_event(value).ok())
    }

    /// The chunk's size as `BoundedChunk::bytes` counts it: its whole `Chunk` message as
    /// compact JSON.
    pub fn bytes(&self) -> usize {
        self.chunk.bytes()
    }

    /// Sends T3 the chunk's Ack while the stream is open, and frees the chunk.
    pub fn accept(self) {
        self.chunk.accept();
    }
}

/// One event from `terminal.attach` or `terminal.observe` (`TerminalAttachStreamEvent` in
/// terminal.ts), as T3 sent it. Nothing here drops an event for its sequence or puts events
/// in order. That is up to the caller.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TerminalEvent<'a> {
    /// `snapshot`: the first event of every attach and observe, and T3's `started` when the
    /// shell starts again (`terminalEventToAttachEvent` in Manager.ts). Its history replaces
    /// what the caller showed.
    Snapshot(SessionSnapshot<'a>),
    /// `restarted`: `terminal.restart` started the shell again. Its history replaces what the
    /// caller showed.
    Restarted {
        base: EventBase<'a>,
        snapshot: SessionSnapshot<'a>,
    },
    /// `output`: what the shell wrote, raw.
    Output {
        base: EventBase<'a>,
        data: ShellText<'a>,
    },
    /// `cleared`: `terminal.clear` emptied the terminal's history.
    Cleared { base: EventBase<'a> },
    /// `exited`: the shell exited.
    Exited {
        base: EventBase<'a>,
        exit_code: Option<i64>,
        exit_signal: Option<i64>,
    },
    /// `error`: T3 couldn't start or run the shell.
    Error {
        base: EventBase<'a>,
        message: &'a str,
    },
    /// `activity`: a program started or ended in the shell. Only the label and the flag
    /// change.
    Activity {
        base: EventBase<'a>,
        has_running_subprocess: bool,
        label: &'a str,
    },
    /// `closed`: the terminal was closed. The stream stays open.
    Closed { base: EventBase<'a> },
    /// An event type this t3term doesn't know, kept for the caller to skip or report.
    Unknown(UnknownEvent<'a>),
}

/// The fields every event but `snapshot` carries (`TerminalEventBaseSchema`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventBase<'a> {
    pub thread_id: &'a str,
    pub terminal_id: &'a str,
    /// The terminal's event count when T3 sent the event. T3 may leave it out. A new shell
    /// counts again from 0.
    pub sequence: Option<u64>,
}

/// A terminal's state and history (`TerminalSessionSnapshot` in terminal.ts).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SessionSnapshot<'a> {
    pub thread_id: &'a str,
    pub terminal_id: &'a str,
    pub cwd: &'a str,
    pub worktree_path: Option<&'a str>,
    pub status: SessionStatus,
    pub pid: Option<u64>,
    /// T3's copy of what the shell wrote, up to its last 5,000 lines and 8 MiB, without
    /// the terminal queries T3 removes from history.
    pub history: ShellText<'a>,
    pub exit_code: Option<i64>,
    pub exit_signal: Option<i64>,
    /// T3's title for the terminal, at most 128 UTF-16 code units.
    pub label: &'a str,
    pub updated_at: &'a str,
    /// The terminal's event count. T3 may leave it out.
    pub sequence: Option<u64>,
}

/// `TerminalSessionStatus` in terminal.ts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Starting,
    Running,
    Exited,
    Error,
}

/// Text a shell wrote, as T3 sent it: live output, or a snapshot's history. It can hold any
/// escape sequence, so it should reach the screen only through a terminal emulator. Its
/// Debug shows only its length.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ShellText<'a>(&'a str);

impl<'a> ShellText<'a> {
    pub fn as_str(&self) -> &'a str {
        self.0
    }
}

impl fmt::Debug for ShellText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ShellText({} bytes)", self.0.len())
    }
}

/// An event whose `type` this t3term doesn't know, as T3 sent it.
#[derive(Clone, Copy, PartialEq)]
pub struct UnknownEvent<'a> {
    kind: &'a str,
    value: &'a Value,
}

impl<'a> UnknownEvent<'a> {
    /// The event's `type`.
    pub fn kind(self) -> &'a str {
        self.kind
    }

    /// The whole event.
    pub fn value(self) -> &'a Value {
        self.value
    }
}

impl fmt::Debug for UnknownEvent<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The start of the type alone, since the event can hold anything.
        let end = self.kind.char_indices().nth(32);
        let kind = &self.kind[..end.map_or(self.kind.len(), |(index, _)| index)];
        f.debug_struct("UnknownEvent")
            .field("kind", &kind)
            .finish_non_exhaustive()
    }
}

/// Changes to every terminal in the environment, from `subscribeTerminalMetadata`: a batch
/// for each chunk T3 sends. T3 sends a snapshot first, then an upsert or a remove for each
/// change. Output and clears don't reach this stream.
pub struct MetadataStream(Checked);

impl MetadataStream {
    /// Subscribes with an empty payload. The stream needs the session's `terminal:read`
    /// scope, and T3 waits for each chunk's Ack before it sends the next.
    pub async fn open(rpc: &RpcClient, limits: StreamLimits) -> Result<MetadataStream, RpcError> {
        let stream = rpc.bounded_stream(METADATA, json!({}), limits).await?;
        Ok(MetadataStream(Checked::new(stream)))
    }

    /// The next batch, as [`TerminalStream::next`] returns them.
    pub async fn next(&mut self) -> Option<Result<MetadataBatch, RpcError>> {
        let next = self.0.next(check_metadata).await?;
        Some(next.map(|chunk| MetadataBatch { chunk }))
    }
}

/// The events of one chunk from `subscribeTerminalMetadata`, borrowed from the chunk, as a
/// [`TerminalBatch`] holds terminal events.
#[derive(Debug)]
pub struct MetadataBatch {
    chunk: BoundedChunk,
}

impl MetadataBatch {
    pub fn events(&self) -> impl Iterator<Item = MetadataEvent<'_>> {
        // `MetadataStream::next` read every value before it returned the batch.
        let values = self.chunk.values().iter();
        values.filter_map(|value| parse_metadata(value).ok())
    }

    pub fn bytes(&self) -> usize {
        self.chunk.bytes()
    }

    pub fn accept(self) {
        self.chunk.accept();
    }
}

/// One event from `subscribeTerminalMetadata` (`TerminalMetadataStreamEvent` in
/// terminal.ts).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetadataEvent<'a> {
    /// `snapshot`: every terminal in the environment, on every thread, newest first.
    Snapshot(Summaries<'a>),
    /// `upsert`: a terminal started, changed or exited.
    Upsert(TerminalSummary<'a>),
    /// `remove`: a terminal was closed.
    Remove {
        thread_id: &'a str,
        terminal_id: &'a str,
    },
    Unknown(UnknownEvent<'a>),
}

/// The terminals in a metadata snapshot.
#[derive(Clone, Copy, PartialEq)]
pub struct Summaries<'a>(&'a [Value]);

impl<'a> Summaries<'a> {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = TerminalSummary<'a>> + use<'a> {
        // The snapshot's batch read each one before the caller got it.
        let event = "a terminal metadata snapshot";
        let terminals: &'a [Value] = self.0;
        terminals
            .iter()
            .filter_map(move |value| Fields::of(value, event, "terminals[].").ok())
            .filter_map(|fields| summary(fields).ok())
    }
}

impl fmt::Debug for Summaries<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// A terminal without its history (`TerminalSummary` in terminal.ts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSummary<'a> {
    pub thread_id: &'a str,
    pub terminal_id: &'a str,
    pub cwd: &'a str,
    pub worktree_path: Option<&'a str>,
    pub status: SessionStatus,
    pub pid: Option<u64>,
    pub exit_code: Option<i64>,
    pub exit_signal: Option<i64>,
    pub has_running_subprocess: bool,
    pub label: &'a str,
    pub updated_at: &'a str,
}

/// A bounded stream whose chunks reach the caller only once every value in them reads.
struct Checked {
    /// `None` once a chunk failed to read.
    stream: Option<BoundedStream>,
}

impl Checked {
    fn new(stream: BoundedStream) -> Checked {
        Checked {
            stream: Some(stream),
        }
    }

    /// The next chunk whose values all pass `check`. Cancel-safe: nothing after
    /// `BoundedStream::next` waits.
    async fn next(
        &mut self,
        check: fn(&Value) -> Result<(), Malformed>,
    ) -> Option<Result<BoundedChunk, RpcError>> {
        let chunk = match self.stream.as_mut()?.next().await? {
            Ok(chunk) => chunk,
            Err(error) => return Some(Err(explain(error))),
        };
        let checked = chunk.values().iter().try_for_each(check);
        if let Err(malformed) = checked {
            // Dropping the chunk unaccepted acks nothing and stops the stream, which
            // interrupts it on T3 if it is still open. Dropping the stream then discards the
            // chunks queued after this one.
            drop(chunk);
            self.stream = None;
            return Some(Err(RpcError::Protocol(malformed.to_string())));
        }
        Some(Ok(chunk))
    }
}

fn check_event(value: &Value) -> Result<(), Malformed> {
    parse_event(value)?;
    Ok(())
}

fn check_metadata(value: &Value) -> Result<(), Malformed> {
    parse_metadata(value)?;
    Ok(())
}

fn parse_event(value: &Value) -> Result<TerminalEvent<'_>, Malformed> {
    let (kind, map) = typed(value, "a terminal event")?;
    let fields = |event| Fields {
        map,
        event,
        prefix: "",
    };
    Ok(match kind {
        "snapshot" => {
            let fields = fields("a terminal snapshot event");
            TerminalEvent::Snapshot(snapshot(fields.object("snapshot", "snapshot.")?)?)
        }
        "restarted" => {
            let fields = fields("a terminal restarted event");
            let base = fields.base()?;
            let snapshot = snapshot(fields.object("snapshot", "snapshot.")?)?;
            TerminalEvent::Restarted { base, snapshot }
        }
        "output" => {
            let fields = fields("a terminal output event");
            let base = fields.base()?;
            let data = ShellText(fields.string("data")?);
            TerminalEvent::Output { base, data }
        }
        "cleared" => {
            let base = fields("a terminal cleared event").base()?;
            TerminalEvent::Cleared { base }
        }
        "exited" => {
            let fields = fields("a terminal exited event");
            TerminalEvent::Exited {
                base: fields.base()?,
                exit_code: fields.nullable_int("exitCode")?,
                exit_signal: fields.nullable_int("exitSignal")?,
            }
        }
        "error" => {
            let fields = fields("a terminal error event");
            let base = fields.base()?;
            let message = fields.non_empty("message")?;
            TerminalEvent::Error { base, message }
        }
        "activity" => {
            let fields = fields("a terminal activity event");
            TerminalEvent::Activity {
                base: fields.base()?,
                has_running_subprocess: fields.boolean("hasRunningSubprocess")?,
                label: fields.label()?,
            }
        }
        "closed" => {
            let base = fields("a terminal closed event").base()?;
            TerminalEvent::Closed { base }
        }
        _ => TerminalEvent::Unknown(UnknownEvent { kind, value }),
    })
}

fn parse_metadata(value: &Value) -> Result<MetadataEvent<'_>, Malformed> {
    let (kind, map) = typed(value, "a terminal metadata event")?;
    let fields = |event| Fields {
        map,
        event,
        prefix: "",
    };
    Ok(match kind {
        "snapshot" => {
            let event = "a terminal metadata snapshot";
            let terminals = fields(event).array("terminals")?;
            for terminal in terminals {
                summary(Fields::of(terminal, event, "terminals[].")?)?;
            }
            MetadataEvent::Snapshot(Summaries(terminals))
        }
        "upsert" => {
            let fields = fields("a terminal metadata upsert");
            MetadataEvent::Upsert(summary(fields.object("terminal", "terminal.")?)?)
        }
        "remove" => {
            let fields = fields("a terminal metadata remove");
            MetadataEvent::Remove {
                thread_id: fields.non_empty("threadId")?,
                terminal_id: fields.non_empty("terminalId")?,
            }
        }
        _ => MetadataEvent::Unknown(UnknownEvent { kind, value }),
    })
}

fn snapshot(fields: Fields<'_>) -> Result<SessionSnapshot<'_>, Malformed> {
    Ok(SessionSnapshot {
        thread_id: fields.non_empty("threadId")?,
        terminal_id: fields.non_empty("terminalId")?,
        cwd: fields.non_empty("cwd")?,
        worktree_path: fields.nullable_non_empty("worktreePath")?,
        status: fields.status()?,
        pid: fields.pid()?,
        history: ShellText(fields.string("history")?),
        exit_code: fields.nullable_int("exitCode")?,
        exit_signal: fields.nullable_int("exitSignal")?,
        label: fields.label()?,
        updated_at: fields.string("updatedAt")?,
        sequence: fields.sequence()?,
    })
}

fn summary(fields: Fields<'_>) -> Result<TerminalSummary<'_>, Malformed> {
    Ok(TerminalSummary {
        thread_id: fields.non_empty("threadId")?,
        terminal_id: fields.non_empty("terminalId")?,
        cwd: fields.non_empty("cwd")?,
        worktree_path: fields.nullable_non_empty("worktreePath")?,
        status: fields.status()?,
        pid: fields.pid()?,
        exit_code: fields.nullable_int("exitCode")?,
        exit_signal: fields.nullable_int("exitSignal")?,
        has_running_subprocess: fields.boolean("hasRunningSubprocess")?,
        label: fields.label()?,
        updated_at: fields.string("updatedAt")?,
    })
}

/// An event's `type` and fields.
fn typed<'a>(
    value: &'a Value,
    event: &'static str,
) -> Result<(&'a str, &'a Map<String, Value>), Malformed> {
    let bad = Malformed {
        event,
        prefix: "",
        field: "type",
        expected: "a string",
    };
    let map = value.as_object().ok_or(bad)?;
    let kind = map.get("type").and_then(Value::as_str).ok_or(bad)?;
    Ok((kind, map))
}

/// Why t3term couldn't read an event: which field of which event, and what T3's schema says
/// the field holds. It quotes no value, so terminal output never reaches the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Malformed {
    event: &'static str,
    prefix: &'static str,
    field: &'static str,
    expected: &'static str,
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Malformed {
            event,
            prefix,
            field,
            expected,
        } = self;
        write!(
            f,
            "T3 sent {event} whose `{prefix}{field}` isn't {expected}"
        )
    }
}

/// The fields of one object in an event, and how to name them in a [`Malformed`].
#[derive(Clone, Copy)]
struct Fields<'a> {
    map: &'a Map<String, Value>,
    event: &'static str,
    /// Where the object sits in the event, such as `snapshot.`.
    prefix: &'static str,
}

impl<'a> Fields<'a> {
    /// The fields of `value`, an object at `prefix` in `event`.
    fn of(value: &'a Value, event: &'static str, prefix: &'static str) -> Result<Self, Malformed> {
        let Some(map) = value.as_object() else {
            let field = prefix.trim_end_matches('.');
            return Err(Malformed {
                event,
                prefix: "",
                field,
                expected: "an object",
            });
        };
        Ok(Fields { map, event, prefix })
    }

    fn bad(&self, field: &'static str, expected: &'static str) -> Malformed {
        Malformed {
            event: self.event,
            prefix: self.prefix,
            field,
            expected,
        }
    }

    fn get(&self, field: &str) -> Option<&'a Value> {
        self.map.get(field)
    }

    fn object(&self, field: &'static str, prefix: &'static str) -> Result<Fields<'a>, Malformed> {
        let value = self.get(field).ok_or(self.bad(field, "an object"))?;
        Fields::of(value, self.event, prefix)
    }

    fn array(&self, field: &'static str) -> Result<&'a [Value], Malformed> {
        let array = self.get(field).and_then(Value::as_array);
        array.map(Vec::as_slice).ok_or(self.bad(field, "an array"))
    }

    fn string(&self, field: &'static str) -> Result<&'a str, Malformed> {
        let text = self.get(field).and_then(Value::as_str);
        text.ok_or(self.bad(field, "a string"))
    }

    fn non_empty(&self, field: &'static str) -> Result<&'a str, Malformed> {
        let text = self.get(field).and_then(Value::as_str);
        let text = text.filter(|text| !text.is_empty());
        text.ok_or(self.bad(field, "a non-empty string"))
    }

    /// A required field that holds a non-empty string or null.
    fn nullable_non_empty(&self, field: &'static str) -> Result<Option<&'a str>, Malformed> {
        match self.get(field) {
            Some(Value::Null) => Ok(None),
            Some(Value::String(text)) if !text.is_empty() => Ok(Some(text)),
            _ => Err(self.bad(field, "a non-empty string or null")),
        }
    }

    fn boolean(&self, field: &'static str) -> Result<bool, Malformed> {
        let flag = self.get(field).and_then(Value::as_bool);
        flag.ok_or(self.bad(field, "true or false"))
    }

    /// A required field that holds an integer or null.
    fn nullable_int(&self, field: &'static str) -> Result<Option<i64>, Malformed> {
        match self.get(field) {
            Some(Value::Null) => Ok(None),
            Some(value) => match int(value) {
                Some(number) => Ok(Some(number)),
                None => Err(self.bad(field, "an integer or null")),
            },
            None => Err(self.bad(field, "an integer or null")),
        }
    }

    fn pid(&self) -> Result<Option<u64>, Malformed> {
        let bad = self.bad("pid", "a positive integer or null");
        match self.get("pid") {
            Some(Value::Null) => Ok(None),
            Some(value) => match int(value) {
                Some(pid) if pid > 0 => Ok(u64::try_from(pid).ok()),
                _ => Err(bad),
            },
            None => Err(bad),
        }
    }

    /// The optional `sequence`: left out, or a non-negative integer.
    fn sequence(&self) -> Result<Option<u64>, Malformed> {
        let Some(value) = self.get("sequence") else {
            return Ok(None);
        };
        let sequence = int(value).and_then(|number| u64::try_from(number).ok());
        match sequence {
            Some(sequence) => Ok(Some(sequence)),
            None => Err(self.bad("sequence", "a non-negative integer")),
        }
    }

    fn status(&self) -> Result<SessionStatus, Malformed> {
        let status = match self.get("status").and_then(Value::as_str) {
            Some("starting") => SessionStatus::Starting,
            Some("running") => SessionStatus::Running,
            Some("exited") => SessionStatus::Exited,
            Some("error") => SessionStatus::Error,
            _ => {
                let expected = r#""starting", "running", "exited" or "error""#;
                return Err(self.bad("status", expected));
            }
        };
        Ok(status)
    }

    fn label(&self) -> Result<&'a str, Malformed> {
        let label = self.get("label").and_then(Value::as_str);
        let label = label.filter(|label| utf16_at_most(label, LABEL_LIMIT));
        label.ok_or(self.bad("label", "a string of at most 128 UTF-16 code units"))
    }

    fn base(&self) -> Result<EventBase<'a>, Malformed> {
        Ok(EventBase {
            thread_id: self.non_empty("threadId")?,
            terminal_id: self.non_empty("terminalId")?,
            sequence: self.sequence()?,
        })
    }
}

/// `value` as an integer Effect's `Schema.Int` takes: one JSON.stringify writes without a
/// fraction, within JavaScript's safe range.
fn int(value: &Value) -> Option<i64> {
    let safe = -MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER;
    value.as_i64().filter(|number| safe.contains(number))
}

/// Puts T3's terminal errors into words. They carry their `_tag` and fields but no message
/// (terminal.ts), so rpc.rs can only say that T3 rejected the request. The tag stays in
/// `error_tag`, and any cause rpc.rs found stays after the words.
fn explain(error: RpcError) -> RpcError {
    match error {
        RpcError::Failed {
            tag,
            message,
            error_tag,
        } => {
            let words = error_tag.as_deref().and_then(describe);
            let fallback = rpc::rejected(&tag);
            let message = match (words, message.strip_prefix(fallback.as_str())) {
                (Some(words), Some("")) => format!("{words}."),
                (Some(words), Some(cause)) => format!("{words}{cause}"),
                _ => message,
            };
            RpcError::Failed {
                tag,
                message,
                error_tag,
            }
        }
        other => other,
    }
}

/// What each of T3's terminal errors means, from the `message` getters in terminal.ts,
/// without the ids and paths they quote.
fn describe(error_tag: &str) -> Option<&'static str> {
    Some(match error_tag {
        "TerminalSessionLookupError" => "T3 has no such terminal on this thread",
        "TerminalNotRunningError" => "The terminal's shell isn't running",
        "TerminalCwdNotFoundError" => "The shell's directory doesn't exist",
        "TerminalCwdNotDirectoryError" => "The shell's directory isn't a directory",
        "TerminalCwdStatError" => "T3 couldn't read the shell's directory",
        "TerminalHistoryError" => "T3 couldn't read or change the terminal's history",
        "TerminalProviderInstanceNotFoundError" => {
            "The terminal's provider instance isn't available"
        }
        "TerminalProviderEnvironmentError" => {
            "T3 couldn't prepare the provider's environment for the shell"
        }
        "TerminalWriteError" => "T3 couldn't write to the shell",
        "TerminalResizeError" => "T3 couldn't resize the shell",
        _ => return None,
    })
}

/// Whether T3's `TrimmedNonEmptyString` takes `text` as it is: not blank, with nothing for
/// JavaScript's `trim` to remove at either end.
fn trimmed_non_blank(text: &str) -> bool {
    match (text.chars().next(), text.chars().next_back()) {
        (Some(first), Some(last)) => !js_space(first) && !js_space(last),
        _ => false,
    }
}

/// Whether JavaScript's `trim` removes `character`, as one of ECMAScript's WhiteSpace or
/// LineTerminator characters. Rust's `char::is_whitespace` differs in two: it takes U+0085,
/// which JavaScript keeps, and not U+FEFF, which JavaScript removes.
fn js_space(character: char) -> bool {
    (character.is_whitespace() && character != '\u{85}') || character == '\u{feff}'
}

/// Whether `text` is at most `limit` UTF-16 code units, the length JavaScript and Effect's
/// `isMaxLength` count. It reads at most `limit` + 1 units.
fn utf16_at_most(text: &str, limit: usize) -> bool {
    // A UTF-8 byte never stands for more than one UTF-16 unit.
    text.len() <= limit || text.encode_utf16().nth(limit).is_none()
}

/// Whether `key` matches `^[A-Za-z_][A-Za-z0-9_]*$` and is at most 128 characters.
fn env_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    let first = bytes.next();
    key.len() <= ENV_KEY_LIMIT
        && first.is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused<T: fmt::Debug>(result: Result<T, TerminalError>) -> &'static str {
        match result {
            Err(TerminalError::Invalid(reason)) => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn ids_must_be_trimmed_as_javascript_trims() {
        let id = TerminalId::new("thread-1", "term-1").unwrap();
        assert_eq!((id.thread_id(), id.terminal_id()), ("thread-1", "term-1"));
        for blank in ["", " ", "\t", "\u{3000}"] {
            refused(TerminalId::new(blank, "term-1"));
            refused(TerminalId::new("thread-1", blank));
        }
        // JavaScript's trim removes a no-break space, a byte order mark and a line separator
        // at either end.
        for untrimmed in [
            " term-1",
            "term-1\n",
            "\u{a0}term-1",
            "term-1\u{feff}",
            "\u{2028}a",
        ] {
            refused(TerminalId::new("thread-1", untrimmed));
            refused(TerminalId::new(untrimmed, "term-1"));
        }
        // It keeps U+0085 and anything inside, so T3 looks up these ids as they are.
        for kept in ["term-1\u{85}", "\u{85}term", "term 1", "term\u{1b}1"] {
            assert!(TerminalId::new("thread-1", kept).is_ok(), "{kept:?}");
        }
    }

    #[test]
    fn a_terminal_id_counts_utf16_code_units() {
        assert!(TerminalId::new("t", &"a".repeat(128)).is_ok());
        let reason = refused(TerminalId::new("t", &"a".repeat(129)));
        assert_eq!(
            reason,
            "a terminal id must be at most 128 UTF-16 code units"
        );
        // "漢" takes three UTF-8 bytes and one UTF-16 unit, and "🙂" four bytes and two units.
        assert!(TerminalId::new("t", &"漢".repeat(128)).is_ok());
        refused(TerminalId::new("t", &"漢".repeat(129)));
        assert!(TerminalId::new("t", &"🙂".repeat(64)).is_ok());
        refused(TerminalId::new(
            "t",
            &"🙂".repeat(64).replacen('🙂', "🙂a", 1),
        ));
        // A thread id has no limit on its length.
        assert!(TerminalId::new(&"a".repeat(10_000), "term-1").is_ok());
    }

    #[test]
    fn sizes_stay_in_t3s_range() {
        for (cols, rows) in [(1, 1), (1000, 500), (120, 30)] {
            let size = Size::new(cols, rows).unwrap();
            assert_eq!((size.cols(), size.rows()), (cols, rows));
        }
        assert_eq!(refused(Size::new(0, 1)), "a terminal has 1 to 1000 columns");
        assert_eq!(
            refused(Size::new(1001, 1)),
            "a terminal has 1 to 1000 columns"
        );
        assert_eq!(refused(Size::new(1, 0)), "a terminal has 1 to 500 rows");
        assert_eq!(refused(Size::new(1, 501)), "a terminal has 1 to 500 rows");
    }

    #[test]
    fn a_launch_follows_t3s_schema_for_paths_and_environment() {
        refused(Launch::new("", None));
        refused(Launch::new(" /repo", None));
        refused(Launch::new("/repo", Some("")));
        refused(Launch::new("/repo", Some("/work ")));
        let launch = Launch::new("/repo", None).unwrap();
        let (longest, too_long) = ("A".repeat(128), "A".repeat(129));
        for key in ["", "1A", "A-B", "A B", "É", too_long.as_str()] {
            refused(launch.clone().with_env(key, "x"));
        }
        for key in ["A", "_", "_a1", "PATH", longest.as_str()] {
            assert!(launch.clone().with_env(key, "x").is_ok(), "{key}");
        }
        // A value counts UTF-16 code units, and may be empty or untrimmed.
        assert!(launch.clone().with_env("A", &"🙂".repeat(4096)).is_ok());
        refused(
            launch
                .clone()
                .with_env("A", &"🙂".repeat(4096).replacen('🙂', "🙂a", 1)),
        );
        assert!(launch.clone().with_env("A", "").is_ok());
        assert!(launch.clone().with_env("A", " x ").is_ok());

        // 128 keys fit. A 129th is refused, and setting one of the 128 again isn't.
        let mut full = launch;
        for n in 0..128 {
            full = full.with_env(&format!("K{n}"), "x").unwrap();
        }
        let reason = refused(full.clone().with_env("K128", "x"));
        assert_eq!(reason, "a shell takes at most 128 environment keys");
        let replaced = full.with_env("K0", "y").unwrap();
        assert_eq!(replaced.env.len(), 128);
        assert_eq!(replaced.env["K0"], "y");
    }

    #[test]
    fn a_thread_launch_matches_the_desktop() {
        let mut payload = Map::new();
        Launch::for_thread("/repo", Some("/repo/.t3/worktrees/a"))
            .unwrap()
            .write_into(&mut payload);
        let expected = json!({"cwd": "/repo/.t3/worktrees/a",
            "worktreePath": "/repo/.t3/worktrees/a",
            "env": {"T3CODE_PROJECT_ROOT": "/repo", "T3CODE_WORKTREE_PATH": "/repo/.t3/worktrees/a"}});
        assert_eq!(Value::Object(payload), expected);

        let mut payload = Map::new();
        Launch::for_thread("/repo", None)
            .unwrap()
            .write_into(&mut payload);
        let expected = json!({"cwd": "/repo", "env": {"T3CODE_PROJECT_ROOT": "/repo"}});
        assert_eq!(Value::Object(payload), expected);

        // A launch with no environment sends no env.
        let mut payload = Map::new();
        Launch::new("/repo", None).unwrap().write_into(&mut payload);
        assert_eq!(Value::Object(payload), json!({"cwd": "/repo"}));
    }

    #[test]
    fn debug_shows_no_environment_value_or_shell_text() {
        let launch = Launch::new("/repo", None).unwrap();
        let launch = launch.with_env("TOKEN_LIKE", "value-not-shown").unwrap();
        let debug = format!("{launch:?}");
        assert!(
            debug.contains("TOKEN_LIKE") && !debug.contains("value-not-shown"),
            "{debug}"
        );

        let data = "\u{1b}]52;c;c2VjcmV0\u{7}secret";
        let output = json!({"type": "output", "threadId": "t", "terminalId": "term-1",
            "data": data});
        let event = parse_event(&output).unwrap();
        let debug = format!("{event:?}");
        assert!(
            debug.contains(&format!("ShellText({} bytes)", data.len())),
            "{debug}"
        );
        assert!(
            !debug.contains("secret") && !debug.contains("52;c"),
            "{debug}"
        );

        let kind = format!("future-{}", "x".repeat(100));
        let unknown = json!({"type": kind, "data": "secret"});
        let debug = format!("{:?}", parse_event(&unknown).unwrap());
        assert!(
            debug.contains(&kind[..32]) && !debug.contains(&kind[..33]),
            "{debug}"
        );
        assert!(!debug.contains("secret"), "{debug}");
    }

    /// A snapshot with every field T3 sends, from `snapshot` in Manager.ts.
    fn snapshot_fields() -> Value {
        json!({"threadId": "t", "terminalId": "term-1", "cwd": "/repo", "worktreePath": null,
            "status": "running", "pid": 4242, "history": "$ ls\r\n", "exitCode": null,
            "exitSignal": null, "label": "zsh", "updatedAt": "2026-10-07T12:00:00.000Z",
            "sequence": 3})
    }

    #[test]
    fn a_snapshot_reads_required_nullable_and_optional_fields() {
        let event = json!({"type": "snapshot", "snapshot": snapshot_fields()});
        let TerminalEvent::Snapshot(snapshot) = parse_event(&event).unwrap() else {
            panic!("expected a snapshot");
        };
        assert_eq!(snapshot.worktree_path, None);
        assert_eq!(snapshot.status, SessionStatus::Running);
        assert_eq!(
            (snapshot.pid, snapshot.exit_code, snapshot.sequence),
            (Some(4242), None, Some(3))
        );
        assert_eq!(snapshot.history.as_str(), "$ ls\r\n");

        // `sequence` may be left out, but not sent as null.
        let mut fields = snapshot_fields();
        fields.as_object_mut().unwrap().remove("sequence");
        let event = json!({"type": "snapshot", "snapshot": fields});
        assert!(
            matches!(parse_event(&event), Ok(TerminalEvent::Snapshot(s)) if s.sequence.is_none())
        );

        // Each of these breaks one field of T3's schema.
        let broken = [
            ("sequence", Value::Null, "a non-negative integer"),
            ("sequence", json!(-1), "a non-negative integer"),
            ("sequence", json!(1.5), "a non-negative integer"),
            ("sequence", json!(1_i64 << 53), "a non-negative integer"),
            ("worktreePath", json!(""), "a non-empty string or null"),
            (
                "status",
                json!("stopped"),
                r#""starting", "running", "exited" or "error""#,
            ),
            ("pid", json!(0), "a positive integer or null"),
            ("exitCode", json!("1"), "an integer or null"),
            (
                "label",
                json!("é".repeat(129)),
                "a string of at most 128 UTF-16 code units",
            ),
            ("history", Value::Null, "a string"),
            ("cwd", json!(""), "a non-empty string"),
        ];
        for (field, value, expected) in broken {
            let mut fields = snapshot_fields();
            fields[field] = value;
            let event = json!({"type": "snapshot", "snapshot": fields});
            let message = parse_event(&event).unwrap_err().to_string();
            let wanted = format!(
                "T3 sent a terminal snapshot event whose `snapshot.{field}` isn't {expected}"
            );
            assert_eq!(message, wanted);
        }
        // A nullable field must still be there.
        for field in ["worktreePath", "pid", "exitCode", "exitSignal"] {
            let mut fields = snapshot_fields();
            fields.as_object_mut().unwrap().remove(field);
            let event = json!({"type": "snapshot", "snapshot": fields});
            assert!(parse_event(&event).is_err(), "{field}");
        }
        // A label of 128 two-byte characters fits.
        let mut fields = snapshot_fields();
        fields["label"] = json!("é".repeat(128));
        assert!(parse_event(&json!({"type": "snapshot", "snapshot": fields})).is_ok());
    }

    #[test]
    fn a_bad_event_is_named_without_its_values() {
        let secret = "\u{1b}]52;c;c2VjcmV0\u{7}";
        let cases = [
            (
                json!(secret),
                "T3 sent a terminal event whose `type` isn't a string",
            ),
            (
                json!({"data": secret}),
                "T3 sent a terminal event whose `type` isn't a string",
            ),
            (
                json!({"type": "output", "threadId": "t", "terminalId": "term-1", "data": 7}),
                "T3 sent a terminal output event whose `data` isn't a string",
            ),
            (
                json!({"type": "output", "terminalId": "term-1", "data": secret}),
                "T3 sent a terminal output event whose `threadId` isn't a non-empty string",
            ),
            (
                json!({"type": "error", "threadId": "t", "terminalId": "x", "message": ""}),
                "T3 sent a terminal error event whose `message` isn't a non-empty string",
            ),
            (
                json!({"type": "restarted", "threadId": "t", "terminalId": "x", "snapshot": secret}),
                "T3 sent a terminal restarted event whose `snapshot` isn't an object",
            ),
        ];
        for (event, expected) in cases {
            let message = parse_event(&event).unwrap_err().to_string();
            assert_eq!(message, expected);
        }
    }

    #[test]
    fn metadata_reads_every_summary_and_names_a_bad_one() {
        let summary = json!({"threadId": "t", "terminalId": "term-1", "cwd": "/repo",
            "worktreePath": "/work", "status": "exited", "pid": null, "exitCode": 0,
            "exitSignal": null, "hasRunningSubprocess": false, "label": "zsh",
            "updatedAt": "2026-10-07T12:00:00.000Z"});
        let event = json!({"type": "snapshot", "terminals": [summary, summary]});
        let MetadataEvent::Snapshot(terminals) = parse_metadata(&event).unwrap() else {
            panic!("expected a snapshot");
        };
        assert_eq!(terminals.len(), 2);
        let first = terminals.iter().next().unwrap();
        assert_eq!(first.worktree_path, Some("/work"));
        assert_eq!(
            (first.status, first.exit_code),
            (SessionStatus::Exited, Some(0))
        );

        let mut broken = summary.clone();
        broken
            .as_object_mut()
            .unwrap()
            .remove("hasRunningSubprocess");
        let event = json!({"type": "snapshot", "terminals": [summary, broken]});
        let message = parse_metadata(&event).unwrap_err().to_string();
        let expected = "T3 sent a terminal metadata snapshot whose \
            `terminals[].hasRunningSubprocess` isn't true or false";
        assert_eq!(message, expected);
        let event = json!({"type": "snapshot", "terminals": [7]});
        let message = parse_metadata(&event).unwrap_err().to_string();
        let expected = "T3 sent a terminal metadata snapshot whose `terminals[]` isn't an object";
        assert_eq!(message, expected);
    }

    #[test]
    fn writes_are_cut_between_characters_at_t3s_limit() {
        let lengths = |text: &str| {
            let cut: Vec<&str> = pieces(text).collect();
            assert_eq!(cut.concat(), text);
            cut.iter()
                .map(|piece| piece.encode_utf16().count())
                .collect::<Vec<_>>()
        };
        assert_eq!(lengths("a"), [1]);
        assert_eq!(lengths(&"a".repeat(WRITE_LIMIT)), [WRITE_LIMIT]);
        assert_eq!(lengths(&"a".repeat(WRITE_LIMIT + 1)), [WRITE_LIMIT, 1]);
        assert_eq!(lengths(&"漢".repeat(WRITE_LIMIT + 1)), [WRITE_LIMIT, 1]);
        // 32,768 emoji are exactly the limit. One unit before it, an emoji goes to the next
        // piece whole.
        assert_eq!(lengths(&"🙂".repeat(WRITE_LIMIT / 2)), [WRITE_LIMIT]);
        let text = format!("{}🙂b", "a".repeat(WRITE_LIMIT - 1));
        let cut: Vec<&str> = pieces(&text).collect();
        assert_eq!(cut, ["a".repeat(WRITE_LIMIT - 1).as_str(), "🙂b"]);
        let three = format!("{}{}", "a".repeat(WRITE_LIMIT * 2), "🙂");
        assert_eq!(lengths(&three), [WRITE_LIMIT, WRITE_LIMIT, 2]);
        assert_eq!(pieces("").count(), 0);
    }

    #[test]
    fn terminal_errors_get_words_and_keep_their_tag_and_cause() {
        let failed = |message: &str, error_tag: &str| RpcError::Failed {
            tag: ATTACH.into(),
            message: message.into(),
            error_tag: Some(error_tag.into()),
        };
        let words = |error: RpcError| match explain(error) {
            RpcError::Failed {
                message, error_tag, ..
            } => (message, error_tag.unwrap()),
            other => panic!("unexpected {other:?}"),
        };
        let lookup = failed("T3 rejected terminal.attach.", "TerminalSessionLookupError");
        let expected = "T3 has no such terminal on this thread.";
        let tag = "TerminalSessionLookupError".to_string();
        assert_eq!(words(lookup), (expected.to_string(), tag));
        // rpc.rs puts a defect's message after the fallback.
        let write = failed("T3 rejected terminal.attach.: EIO", "TerminalWriteError");
        assert_eq!(words(write).0, "T3 couldn't write to the shell: EIO");
        // A message T3 wrote itself, as for a missing scope, stays.
        let scope = "The authenticated token is missing required scope: terminal:operate.";
        let missing = failed(scope, "EnvironmentAuthorizationError");
        assert_eq!(words(missing).0, scope);
        // So does a terminal error that came with a message.
        let own = failed("Unknown terminal thread", "TerminalSessionLookupError");
        assert_eq!(words(own).0, "Unknown terminal thread");
        let lost = RpcError::Disconnected("socket ended".into());
        assert!(matches!(explain(lost), RpcError::Disconnected(_)));
    }
}
