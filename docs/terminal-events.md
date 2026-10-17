# Terminal events

`src/terminal.rs` reads one T3 terminal's streams as typed events and sends that terminal input, resizes and a close. It is the first part of the embedded terminal ([parity.md, group 15](parity.md#15-embedded-terminal)). Nothing in the TUI or the CLI calls it yet.

Everything here follows T3 Code at commit `f570bd2`, nightly v0.0.46-nightly.20261007.2787: `packages/contracts/src/terminal.ts` for the shapes, `rpc.ts` for the method names and `apps/server/src/terminal/Manager.ts` for what the server does.

```rust
let id = TerminalId::new(thread_id, "term-1")?;
let terminal = Terminal::new(rpc.clone(), id);
let mut stream = terminal.observe(limits).await?;
while let Some(batch) = stream.next().await {
    let batch = batch?;
    for event in batch.events() {
        screen.apply(event); // the caller's own bounded state
    }
    batch.accept();
}
```

## Requests

| Method | Scope | Payload t3term sends |
| --- | --- | --- |
| `terminal.attach` | `terminal:operate` | `threadId`, `terminalId`, then `cols` and `rows` if the caller gives a size, then `cwd`, `worktreePath` and `env` if it gives a launch |
| `terminal.observe` | `terminal:read` | `threadId`, `terminalId` |
| `terminal.write` | `terminal:operate` | `threadId`, `terminalId`, `data` |
| `terminal.resize` | `terminal:operate` | `threadId`, `terminalId`, `cols`, `rows` |
| `terminal.close` | `terminal:operate` | `threadId`, `terminalId`, `deleteHistory` |
| `subscribeTerminalMetadata` | `terminal:read` | `{}` |

The scopes come from `apps/server/src/auth/RpcAuthorization.ts`. t3term asks for neither yet, so a saved login gets T3's `EnvironmentAuthorizationError`, whose message names the missing scope.

An attach never sends `restartIfNotRunning` or `providerInstanceId`, so it never starts a shell that has exited again. Without a launch, T3 fails the attach with `TerminalSessionLookupError` if the terminal doesn't exist. With one, T3 starts a shell if the terminal doesn't exist and ignores the launch if it does. `Launch::for_thread` builds the launch the desktop sends: the thread's worktree as `cwd` if it has one, otherwise the project root, with `T3CODE_PROJECT_ROOT` and `T3CODE_WORKTREE_PATH` in `env`. A launch's `Debug` shows its environment keys and not their values.

A size resizes a running shell for everyone who watches it. Observe sends no size and no launch, so it never starts, resizes or restarts a shell.

`terminal.close` always names the terminal. Without `terminalId`, T3 closes every terminal on the thread. t3term doesn't send `terminal.open`, `terminal.restart`, `terminal.clear` or `subscribeTerminalEvents`.

### Checked before sending

Input that breaks T3's schema returns `TerminalError::Invalid` and sends nothing:

- Thread and terminal ids are non-blank and have nothing for JavaScript's `trim` to remove at either end, as `TrimmedNonEmptyString` trims before T3 looks a terminal up. A terminal id is at most 128 UTF-16 code units.
- A size has 1 to 1000 columns and 1 to 500 rows.
- A launch's `cwd` and `worktreePath` are trimmed and non-blank. An `env` key matches `^[A-Za-z_][A-Za-z0-9_]*$` and is at most 128 characters, a value is at most 8,192 UTF-16 code units, and there are at most 128 keys.
- Write text isn't empty.

### Writes

T3 takes at most 65,536 UTF-16 code units in one `terminal.write`. `Terminal::write` cuts longer text into pieces between two characters, so an emoji is never split, and sends them in order. Each piece waits for T3's answer before the next goes out. Writes made at the same time through one `Terminal` take turns in the order they began to wait, and their pieces never mix. If a piece fails, or the caller cancels the write, the pieces before it have reached the shell and the rest never go.

## Events

`terminal.attach` and `terminal.observe` send the same events (`TerminalAttachStreamEvent`). Each starts with a `snapshot` holding the terminal's whole history. Every other event carries `threadId`, `terminalId` and an optional `sequence`.

| `type` | Fields | `TerminalEvent` |
| --- | --- | --- |
| `snapshot` | `snapshot` | `Snapshot` |
| `output` | `data` | `Output` |
| `exited` | `exitCode`, `exitSignal`, both required and nullable | `Exited` |
| `error` | `message` | `Error` |
| `cleared` | none | `Cleared` |
| `restarted` | `snapshot` | `Restarted` |
| `activity` | `hasRunningSubprocess`, `label` | `Activity` |
| `closed` | none | `Closed` |

A snapshot (`TerminalSessionSnapshot`) requires `threadId`, `terminalId`, `cwd`, `status`, `history`, `label` and `updatedAt`. It requires `worktreePath`, `pid`, `exitCode` and `exitSignal` too, but each may be null. `sequence` may be left out, and is a non-negative integer when it's there. A `label` is at most 128 UTF-16 code units and a `pid` is above zero. Integers must be safe JavaScript integers, as Effect's `Schema.Int` requires.

`subscribeTerminalMetadata` covers every terminal on every thread (`TerminalMetadataStreamEvent`): a `snapshot` with `terminals`, then an `upsert` with one `terminal` or a `remove` with `threadId` and `terminalId` for each change. A summary (`TerminalSummary`) has a snapshot's fields without `history` and `sequence`, plus `hasRunningSubprocess`. Picking out one thread's terminals is up to the caller.

Events borrow from the chunk. `output` text and snapshot history come as `ShellText`, raw and possibly full of escape sequences. It should reach the screen only through a terminal emulator, and its `Debug` shows only its length. An event with a `type` t3term doesn't know comes through as `Unknown`, with the whole event borrowed.

t3term passes events on in the order T3 sent them. It doesn't drop an event for its `sequence`, doesn't reorder and doesn't track shells across restarts. A `closed` or `exited` doesn't end the stream.

### A malformed event

If any event in a chunk breaks T3's schema, the caller never sees that chunk. t3term drops it unacked, which stops the stream and sends T3 an `Interrupt`, discards the chunks queued after it, and returns `RpcError::Protocol`. The message names the event and the field, such as ``T3 sent a terminal output event whose `data` isn't a string``. It never quotes a value, so no terminal output reaches it. After that, `next` returns `None`.

## Batches and Acks

Each Effect `Chunk` becomes one batch, `TerminalBatch` or `MetadataBatch`. A chunk can hold several events, and one WebSocket message can hold several chunks, each its own batch. Receiving a batch sends nothing. `accept` sends that chunk's one Ack and frees its place in the stream's limits, so call it after applying every event. The events borrow from the batch, so they're gone by then.

Dropping a batch unaccepted stops the stream, as dropping a `BoundedChunk` does. Dropping a stream interrupts that one subscription. Neither closes the terminal or touches its shell. `next` is safe to cancel, in a `select!` for example: a batch it hasn't returned stays queued.

T3 sends attach chunks without waiting while fewer than 8 chunks and 64 KiB wait for an Ack. On observe and metadata it waits for each Ack before the next chunk. [bounded-rpc-streams.md](bounded-rpc-streams.md#t3s-terminal-output-window) has the details.

## Byte budgets

The caller picks the `StreamLimits` for every stream. This module sets no default.

The byte limit counts each chunk's whole `Chunk` message as compact JSON, the encoded size T3 sends. It caps what a stream holds unaccepted. It doesn't cap the heap, and it can't stop a message from being read. The connection's reader takes in each WebSocket message whole, and serde_json parses all of it, before the limit sees any chunk in it. [bounded-rpc-streams.md](bounded-rpc-streams.md#what-the-limits-dont-bound) has more.

The socket has its own limit. t3term reads a message of up to 64 MiB, and T3 sends each message in one frame, so t3term allows a frame of up to 64 MiB too ([One frame per message](bounded-rpc-streams.md#one-frame-per-message)). A message over 64 MiB closes the socket and fails every call and stream on it.

The first chunk of every attach and observe holds the terminal's history. T3 keeps up to 5,000 lines and 8 MiB of it per terminal, counting UTF-8 bytes (`DEFAULT_HISTORY_LINE_LIMIT` and `DEFAULT_HISTORY_BYTE_LIMIT` in `Manager.ts`). JSON writes a control character such as ESC as `\u001b`, six bytes for one, so 8 MiB of history can take up to about 48 MiB as JSON. That fits in one 64 MiB message, and with tungstenite's default 16 MiB frame limit it wouldn't have.

A byte limit below a history's JSON size stops that stream at its snapshot. The socket and its other calls and streams go on, and nothing attaches again on its own. A caller that wants every history T3 keeps needs a byte limit of about 48 MiB plus the rest of the snapshot's message. A smaller limit is a choice to stop on large histories. t3term doesn't know how large T3's messages get in practice.

## Out of scope

- A terminal pane, an emulator or any drawing. Nothing writes shell text to stdout.
- Reconnecting, watching in the background or keeping output. The only output t3term holds is in the batches the caller hasn't accepted.
- `terminal.open`, `terminal.restart`, `terminal.clear` and `subscribeTerminalEvents`.
- The `terminal:read` and `terminal:operate` scopes for a saved login.
