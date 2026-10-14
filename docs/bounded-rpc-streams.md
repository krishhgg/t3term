# Bounded RPC streams

`RpcClient::bounded_stream` opens an Effect RPC stream whose caller acks each chunk, within limits the caller sets. An embedded terminal needs it to read terminal output without an unbounded queue. Nothing in t3term opens one yet. The embedded terminal is still missing ([parity.md, group 15](parity.md#15-embedded-terminal)), and the TUI's watches are unchanged: they use `RpcClient::stream`, which acks each chunk as it arrives and queues its values without a bound.

```rust
let limits = StreamLimits::new(16, 256 * 1024).expect("limits above zero");
let mut stream = rpc.bounded_stream(tag, payload, limits).await?;
while let Some(chunk) = stream.next().await {
    let chunk = chunk?;
    screen.feed(chunk.values()); // the caller's own bounded state
    chunk.accept();
}
```

## Limits

`StreamLimits::new(chunks, bytes)` caps what the stream holds that the caller hasn't accepted. A chunk counts from the moment the reader queues it until the caller accepts or drops it, whether it waits in the stream or the caller holds it. `new` returns `None` if either limit is zero. A sum too big for `usize` counts as over the limit.

A chunk's size, `BoundedChunk::bytes`, is the length of its whole `Chunk` message, `{"_tag":"Chunk","requestId":"7","values":[...]}`, as compact UTF-8 JSON. T3's terminal output window measures a chunk the same way, as `Buffer.byteLength(JSON.stringify(response))` in `apps/server/src/terminal/OutputProtocol.ts`. t3term has serde_json write the message into a counter that keeps nothing and stops at the byte limit, so sizing a chunk takes no second copy of it. serde_json writes strings and integers as JSON.stringify does, but can write a float with an exponent differently, such as `1e21` for `1e+21`.

## What the limits don't bound

- Memory. A parsed `serde_json::Value` takes more room than its JSON, so the byte limit isn't a limit on the heap or the process.
- Reading. The connection's reader decodes each WebSocket message whole before it routes the chunks in it. t3term keeps tungstenite 0.28's defaults, which allow a message of up to 64 MiB in frames of up to 16 MiB. A chunk over the byte limit still costs its full decode before t3term refuses it.
- Accepted values. Once the caller accepts a chunk, bounding its values is up to the caller.

## Accepting a chunk

`BoundedChunk::accept` sends T3 `{"_tag":"Ack","requestId":...}` and takes the chunk out of the limits. Receiving a chunk sends nothing. Each chunk is acked once at most, an empty one included. Accept a chunk once its values are stored or handled within the caller's own bounded state. From then on the stream doesn't count them.

An Ack names the request, not the chunk, and T3's window frees its oldest chunk on each Ack. Accept chunks in the order they arrive, so that T3's count and t3term's agree.

## How a stream stops

The connection's reader never waits for a stream's caller. If a chunk would take the stream over either limit, or is over the byte limit by itself, t3term stops the stream. It sends T3 an `Interrupt`, forgets the request, and drops that chunk and anything T3 sends after it. A `Chunk` without a `values` array stops the stream the same way. So does a chunk the caller drops without accepting, since its values are gone and what follows would have a gap. The other calls and streams on the socket, and its pings, carry on.

An end never needs room in the stream, so it reaches a full one. However a stream ends, `next` returns the chunks already queued, in order, then how it ended, then `None` from then on.

| End | What follows the queued chunks |
| --- | --- |
| T3 ends the stream | `None` |
| T3 fails the stream | `RpcError::Failed` |
| A `Defect` or `ClientProtocolError` | `RpcError::Protocol` |
| The connection closes or goes silent | `RpcError::Disconnected` |
| A `Chunk` without a `values` array | `RpcError::Protocol` |
| A chunk over a limit, or a chunk dropped unaccepted | `RpcError::Stopped` with the reason |

t3term forgets a request as soon as its stream ends, however it ends, so the requests it tracks don't build up over a long session.

## Dropping a stream

Dropping an open `BoundedStream` sends T3 an `Interrupt` and discards the chunks still queued in it. Chunks the caller holds keep their values. Accepting one after its stream has ended, stopped or been dropped returns the values, sends nothing and doesn't reopen the stream.

A `BoundedStream` keeps its connection open while it lives, even after every client has dropped, so its chunks are still acked when the caller accepts them. A chunk doesn't keep the connection open. Once the stream and the last client have dropped, t3term closes the socket, even if the caller still holds a chunk. Before the WebSocket Close frame, it tries for up to a second to send the `Ack` of each chunk the caller accepted and then the stream's `Interrupt`. If the stream's request never went out, neither does its `Interrupt`.

T3 may send more before it reads the `Interrupt`. t3term acks no `Chunk` for a request it no longer tracks, as Effect's own client does (`effect/src/rpc/RpcClient.ts`), and ignores the request's `Exit`. Before this change t3term acked every `Chunk`. A stream from `RpcClient::stream` still keeps its request until T3's `Exit`, so its chunks are acked as before.

If the caller drops a stream as T3's `Exit` for it is on the way, the `Interrupt` reaches T3 after the request has finished. Effect answers it with another `Exit`, which t3term ignores.

`BoundedStream::next` is safe to cancel, in a `select!` for example. A chunk it hasn't returned stays queued.

## T3's terminal output window

These notes are from T3 Code at commit `f570bd2`, nightly v0.0.46-nightly.20261007.2787.

`withTerminalOutputWindow` in `OutputProtocol.ts` covers `terminal.attach` and `subscribeTerminalEvents`. It doesn't cover `terminal.observe`. For those two, T3 acks its own server for each chunk while the client has fewer than 8 chunks and fewer than 64 KiB unacked. Once either is reached, T3 waits for the client's Ack. So T3 leaves at most 8 chunks unacked, and under 64 KiB plus the size of the last one. Limits of at least 8 chunks, and 64 KiB plus the largest chunk the caller expects, won't stop a stream T3 sends normally. T3 doesn't cap the size of a single chunk.

On any other stream, Effect's server waits for an Ack after each chunk, so one chunk at most is unacked.

## Out of scope

This adds no terminal pane, PTY command, CLI flag or connection. The terminal methods need the `terminal:read` and `terminal:operate` scopes, and asking T3 for them is left to a later change.
