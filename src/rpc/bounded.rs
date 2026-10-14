//! Streams whose caller acks each chunk, within limits it sets.
//!
//! `RpcClient::stream` acks a chunk as soon as it arrives and queues its values without a
//! bound. A bounded stream from `RpcClient::bounded_stream` hands its caller each Effect
//! `Chunk` whole and sends the chunk's `Ack` only when the caller accepts it. Until then the
//! chunk counts toward the stream's limits, whether it waits in the stream or the caller
//! holds it. docs/bounded-rpc-streams.md describes the limits and how a stream ends.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use super::{Owner, RpcError};

/// The most a bounded stream may hold that its caller has not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLimits {
    chunks: usize,
    bytes: usize,
}

impl StreamLimits {
    /// At most `chunks` chunks, of `bytes` bytes in all as [`BoundedChunk::bytes`] counts
    /// them. `None` if either is zero.
    pub const fn new(chunks: usize, bytes: usize) -> Option<StreamLimits> {
        if chunks == 0 || bytes == 0 {
            return None;
        }
        Some(StreamLimits { chunks, bytes })
    }
}

/// One Effect `Chunk` from a bounded stream: every value T3 sent in it, and the `Ack` it is
/// owed.
///
/// The chunk counts toward the stream's limits until the caller accepts or drops it.
/// [`accept`](Self::accept) acks it. Dropping it unaccepted stops the stream instead, since
/// its values are gone and what follows would have a gap.
pub struct BoundedChunk {
    values: Vec<Value>,
    bytes: usize,
    /// The stream it came from, until the caller accepts or drops it.
    stream: Option<Arc<Shared>>,
}

impl BoundedChunk {
    /// The values T3 sent in the chunk, in order. A chunk can have none.
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// The length of the whole `Chunk` message, `{"_tag":"Chunk","requestId":...,"values":[...]}`,
    /// as compact UTF-8 JSON. T3's terminal output window measures a chunk the same way, as
    /// `Buffer.byteLength(JSON.stringify(response))` in apps/server/src/terminal/
    /// OutputProtocol.ts. serde_json writes strings and integers as JSON.stringify does, but
    /// can write a float with an exponent differently, such as `1e21` for `1e+21`.
    ///
    /// It isn't the memory the values take. A parsed `Value` takes more than its JSON.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Accepts the chunk and returns its values. While the stream is open, this sends T3 the
    /// chunk's `Ack` and takes the chunk out of the stream's limits.
    ///
    /// Accept a chunk once its values are stored or handled within the caller's own bounded
    /// state. From then on the stream doesn't count them. After the stream has ended, been
    /// stopped or been dropped, accepting returns the values and sends nothing.
    pub fn accept(mut self) -> Vec<Value> {
        if let Some(stream) = self.stream.take() {
            stream.release(self.bytes, true);
        }
        std::mem::take(&mut self.values)
    }
}

impl Drop for BoundedChunk {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            stream.release(self.bytes, false);
        }
    }
}

impl fmt::Debug for BoundedChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Counts only, as the values can be a lot of terminal output.
        f.debug_struct("BoundedChunk")
            .field("values", &self.values.len())
            .field("bytes", &self.bytes)
            .finish()
    }
}

/// A stream from `RpcClient::bounded_stream`. Dropping it interrupts the stream on the
/// server if it is still open, and discards the chunks still queued in it.
///
/// The connection stays open while the stream lives, even after every client has dropped.
pub struct BoundedStream {
    pub(super) shared: Arc<Shared>,
    /// Kept here and not in `Shared`, which the connection's map of requests holds too. It
    /// drops after `drop` has queued the `Interrupt`.
    pub(super) _owner: Arc<Owner>,
}

impl BoundedStream {
    /// The next chunk. Once the stream has ended, the chunks already queued come first, in the
    /// order they arrived. Then comes the error that ended it, if there was one, and from then
    /// on `None`.
    ///
    /// Cancelling the call loses nothing, so it is safe in a `select!`.
    pub async fn next(&mut self) -> Option<Result<BoundedChunk, RpcError>> {
        loop {
            {
                let mut window = self.shared.window();
                if let Some(Queued { values, bytes }) = window.queue.pop_front() {
                    let stream = Some(self.shared.clone());
                    return Some(Ok(BoundedChunk {
                        values,
                        bytes,
                        stream,
                    }));
                }
                match &mut window.state {
                    State::Open => {}
                    State::Ended(error) => {
                        let error = error.take();
                        window.state = State::Done;
                        return error.map(Err);
                    }
                    State::Done => return None,
                }
            }
            // A chunk or the end that arrives after the check above leaves a permit, so this
            // returns at once.
            self.shared.wake.notified().await;
        }
    }
}

impl Drop for BoundedStream {
    fn drop(&mut self) {
        let mut window = self.shared.window();
        if matches!(window.state, State::Open) {
            self.shared.interrupt();
        }
        window.state = State::Done;
        window.queue.clear();
    }
}

/// What a bounded stream shares with its chunks and with the connection's reader. It holds no
/// share in the connection, so neither the reader's map nor a chunk the caller keeps after
/// dropping the stream holds the connection open.
pub(super) struct Shared {
    request_id: String,
    limits: StreamLimits,
    outgoing: mpsc::UnboundedSender<Value>,
    window: Mutex<Window>,
    /// Wakes `BoundedStream::next` when a chunk or the end arrives.
    wake: Notify,
}

struct Window {
    state: State,
    queue: VecDeque<Queued>,
    /// Chunks queued or held by the caller that it hasn't accepted.
    chunks: usize,
    /// Their bytes, as `BoundedChunk::bytes` counts them.
    bytes: usize,
}

enum State {
    /// T3 may send more, and accepting a chunk acks it.
    Open,
    /// The stream has ended. `next` reports why once the queued chunks are out: the error, or
    /// nothing after a normal end.
    Ended(Option<RpcError>),
    /// `next` has reported the end, or the caller dropped the stream.
    Done,
}

/// A chunk waiting in the stream for `next`.
struct Queued {
    values: Vec<Value>,
    bytes: usize,
}

impl Shared {
    pub(super) fn new(
        request_id: String,
        limits: StreamLimits,
        outgoing: mpsc::UnboundedSender<Value>,
    ) -> Arc<Shared> {
        Arc::new(Shared {
            request_id,
            limits,
            outgoing,
            window: Mutex::new(Window::open()),
            wake: Notify::new(),
        })
    }

    fn window(&self) -> MutexGuard<'_, Window> {
        // Nothing that holds the lock can leave the window half changed.
        self.window.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues the `Chunk` message `message`, or stops the stream if it would take the stream
    /// over its limits. Never waits for the caller. Returns false once the stream has ended,
    /// so the reader can forget its route.
    pub(super) fn offer(&self, mut message: Value) -> bool {
        let size = encoded_len(&message, self.limits.bytes);
        let mut window = self.window();
        if !matches!(window.state, State::Open) {
            return false;
        }
        let Some(Value::Array(values)) = message.get_mut("values").map(Value::take) else {
            let error = RpcError::Protocol("a Chunk came without a values array".into());
            self.stop(&mut window, error);
            return false;
        };
        match window.admit(self.limits, size) {
            Ok(bytes) => {
                window.queue.push_back(Queued { values, bytes });
                self.wake.notify_one();
                true
            }
            Err(reason) => {
                self.stop(&mut window, RpcError::Stopped(reason));
                false
            }
        }
    }

    /// Ends an open stream that T3 or the connection ended: with `error`, or normally if it
    /// is `None`. The end needs no room in the queue.
    pub(super) fn end(&self, error: Option<RpcError>) {
        let mut window = self.window();
        if matches!(window.state, State::Open) {
            window.state = State::Ended(error);
            self.wake.notify_one();
        }
    }

    /// Takes a chunk the caller accepted or dropped out of the limits. While the stream is
    /// open, an accepted chunk is acked and a dropped one stops the stream. The Ack goes out
    /// under the lock, so it can never follow the stream's Interrupt.
    fn release(&self, bytes: usize, accepted: bool) {
        let mut window = self.window();
        window.chunks = window.chunks.saturating_sub(1);
        window.bytes = window.bytes.saturating_sub(bytes);
        if !matches!(window.state, State::Open) {
            return;
        }
        if accepted {
            let ack = json!({"_tag": "Ack", "requestId": self.request_id});
            let _ = self.outgoing.send(ack);
        } else {
            let reason = "a chunk was dropped before it was accepted".to_string();
            self.stop(&mut window, RpcError::Stopped(reason));
        }
    }

    /// Ends an open stream with `error`, and interrupts it on the server.
    fn stop(&self, window: &mut Window, error: RpcError) {
        window.state = State::Ended(Some(error));
        self.interrupt();
        self.wake.notify_one();
    }

    fn interrupt(&self) {
        let interrupt =
            json!({"_tag": "Interrupt", "requestId": self.request_id, "interruptors": []});
        let _ = self.outgoing.send(interrupt);
    }
}

impl Window {
    fn open() -> Window {
        Window {
            state: State::Open,
            queue: VecDeque::new(),
            chunks: 0,
            bytes: 0,
        }
    }

    /// Counts a chunk of `size` bytes, or `None` for one over the byte limit by itself, and
    /// returns its size. Says why instead if it would go over either limit.
    fn admit(&mut self, limits: StreamLimits, size: Option<usize>) -> Result<usize, String> {
        let StreamLimits { chunks, bytes } = limits;
        if self.chunks >= chunks {
            return Err(format!("a chunk would go over the {chunks}-chunk limit"));
        }
        let Some(size) = size else {
            return Err(format!("a chunk was over the {bytes}-byte limit by itself"));
        };
        let held = self.bytes;
        match held.checked_add(size) {
            Some(total) if total <= bytes => {
                self.chunks += 1;
                self.bytes = total;
                Ok(size)
            }
            _ => Err(format!(
                "a {size}-byte chunk would take {held} unaccepted bytes past the {bytes}-byte limit"
            )),
        }
    }
}

/// The length of `message` as compact JSON, or `None` once it passes `limit`. It writes into
/// a counter that keeps nothing and stops at the limit, so a huge chunk costs no second
/// buffer and no more than `limit` bytes of writing.
fn encoded_len(message: &Value, limit: usize) -> Option<usize> {
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, message).ok()?;
    Some(counter.bytes)
}

/// Counts the bytes written to it, and fails a write that takes them past `limit`.
struct Counter {
    bytes: usize,
    limit: usize,
}

impl io::Write for Counter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buf.len());
        if self.bytes > self.limit {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_refuse_zero() {
        assert!(StreamLimits::new(0, 1).is_none());
        assert!(StreamLimits::new(1, 0).is_none());
        assert!(StreamLimits::new(usize::MAX, usize::MAX).is_some());
    }

    /// No socket can bring a window this close to `usize::MAX`, so this checks the sum alone.
    #[test]
    fn a_window_near_the_largest_size_refuses_a_chunk_rather_than_wrap() {
        let limits = StreamLimits::new(usize::MAX, usize::MAX).unwrap();
        let mut window = Window::open();
        window.chunks = 1;
        window.bytes = usize::MAX - 10;
        assert!(window.admit(limits, Some(20)).is_err());
        assert_eq!((window.chunks, window.bytes), (1, usize::MAX - 10));
        assert_eq!(window.admit(limits, Some(10)), Ok(10));
        assert_eq!((window.chunks, window.bytes), (2, usize::MAX));
    }
}
