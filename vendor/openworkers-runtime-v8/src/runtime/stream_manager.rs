use bytes::Bytes;
use futures::task::AtomicWaker;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::task::Waker;
use tokio::sync::{mpsc, watch};

pub type StreamId = u64;

/// Default high water mark for stream buffers (number of chunks)
/// This provides backpressure when the consumer is slow
pub const DEFAULT_HIGH_WATER_MARK: usize = 16;

/// The largest chunk a guest may write to the host (blackbox/CONTRACT.md,
/// "Storage: Bounds"): 64 KiB. The platform's own response pump cuts larger
/// chunks into pieces (`js/stream_body.js`); a guest reaching past it for a
/// larger one is refused (vendor/VENDORED.md, patch 7).
pub const MAX_CHUNK_BYTES: usize = 64 * 1024;

/// Streams of a guest's own towards the host, for one request: 16
/// (blackbox/CONTRACT.md, "Storage: Bounds"); one more is an error thrown to
/// the guest (vendor/VENDORED.md, patch 8).
pub const MAX_GUEST_STREAMS: usize = 16;

/// A chunk of data from a stream
#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// Data chunk (bytes)
    Data(Bytes),
    /// Stream finished successfully
    Done,
    /// Stream errored
    Error(String),
}

/// A response body that was a stream the host itself feeds: whether the
/// guest had read from it before the response was read (vendor/VENDORED.md,
/// patch 6). A body the guest did not read is the host's to send itself; one
/// it began to read is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostResponse {
    pub read: bool,
}

/// Manages all active streams and their communication channels
/// Stores both senders (for writing) and receivers (for reading) internally
/// Uses bounded channels for backpressure support
#[derive(Clone)]
pub struct StreamManager {
    /// Senders for writing chunks to streams (bounded for backpressure)
    senders: Arc<Mutex<HashMap<StreamId, mpsc::Sender<StreamChunk>>>>,
    /// Receivers for reading chunks from streams (taken temporarily during reads)
    receivers: Arc<Mutex<HashMap<StreamId, mpsc::Receiver<StreamChunk>>>>,
    /// Stream metadata (URL for debugging)
    metadata: Arc<Mutex<HashMap<StreamId, String>>>,
    /// Streams the host feeds (a request's body; later a file's): their ids
    /// and whether the guest has read from them.
    host_fed: Arc<Mutex<HashMap<StreamId, bool>>>,
    /// The response's body, when it was a host-fed stream: see [`HostResponse`].
    host_response: Arc<Mutex<Option<HostResponse>>>,
    /// Streams of the guest's own made towards the host in this context
    /// (patch 8): at most [`MAX_GUEST_STREAMS`], and the only ids the
    /// guest's own response natives may write to (patch 12).
    guest_streams: Arc<Mutex<HashSet<StreamId>>>,
    /// Host-fed streams the response took to send (patch 11): their sending
    /// goes on after the event, so they are not closed with the request.
    taken: Arc<Mutex<HashSet<StreamId>>>,
    /// Whether the guest (or the response) has asked for the host-fed body:
    /// the host reads it only then (patch 9). The channel says once, and
    /// stays said.
    asked: Arc<Mutex<Option<watch::Sender<bool>>>>,
    /// Next stream ID to allocate
    next_id: Arc<Mutex<StreamId>>,
    /// High water mark for new streams
    high_water_mark: usize,
    /// Waker of the event loop that owns these streams. A consumer leaving is
    /// seen here and nowhere else, and the loop it has to reach is otherwise
    /// waiting on a channel that no one will write to again.
    waker: Arc<AtomicWaker>,
}

impl StreamManager {
    pub fn new() -> Self {
        Self::with_high_water_mark(DEFAULT_HIGH_WATER_MARK)
    }

    /// Create a StreamManager with a custom high water mark
    pub fn with_high_water_mark(high_water_mark: usize) -> Self {
        Self {
            senders: Arc::new(Mutex::new(HashMap::new())),
            receivers: Arc::new(Mutex::new(HashMap::new())),
            metadata: Arc::new(Mutex::new(HashMap::new())),
            host_fed: Arc::new(Mutex::new(HashMap::new())),
            host_response: Arc::new(Mutex::new(None)),
            guest_streams: Arc::new(Mutex::new(HashSet::new())),
            taken: Arc::new(Mutex::new(HashSet::new())),
            asked: Arc::new(Mutex::new(None)),
            next_id: Arc::new(Mutex::new(1)),
            high_water_mark,
            waker: Arc::new(AtomicWaker::new()),
        }
    }

    /// Register the event loop that runs the guests writing these streams.
    /// Must happen on every poll: a waker only belongs to the poll it came from.
    pub fn register_waker(&self, waker: &Waker) {
        self.waker.register(waker);
    }

    /// Create a new stream and return its ID
    /// The receiver is stored internally and can be read via `read_chunk`
    /// Uses bounded channel with high_water_mark capacity for backpressure
    ///
    /// The channel holds one slot more than the high water mark, reserved for
    /// the terminal marker: a stream has a single producer, and a `Done` that
    /// does not fit is a consumer waiting forever for a body that ended.
    pub fn create_stream(&self, url: String) -> StreamId {
        let (tx, rx) = mpsc::channel(self.high_water_mark + 1);

        let mut next_id = self.next_id.lock().unwrap();
        let id = *next_id;
        *next_id += 1;

        self.senders.lock().unwrap().insert(id, tx);
        self.receivers.lock().unwrap().insert(id, rx);
        self.metadata.lock().unwrap().insert(id, url);

        id
    }

    /// Write a chunk to a stream (async - waits if buffer is full for backpressure)
    /// Called by Rust when data arrives from upstream
    /// When the channel is closed (receiver dropped), removes the sender from the map
    pub async fn write_chunk(&self, stream_id: StreamId, chunk: StreamChunk) -> Result<(), String> {
        // Get a clone of the sender (so we don't hold the lock during await)
        let tx = {
            let senders = self.senders.lock().unwrap();
            senders.get(&stream_id).cloned()
        };

        if let Some(tx) = tx {
            match tx.send(chunk).await {
                Ok(()) => Ok(()),
                Err(_) => {
                    // Receiver was dropped (client disconnected) - remove sender from map
                    self.senders.lock().unwrap().remove(&stream_id);
                    Err("Stream channel closed".to_string())
                }
            }
        } else {
            Err(format!("Stream {} not found", stream_id))
        }
    }

    /// Try to write a chunk without waiting (returns error if buffer is full)
    /// Useful for non-async contexts
    /// When the channel is closed (receiver dropped), removes the sender from the map
    ///
    /// Data stops at the high water mark, so the slot beyond it stays free for
    /// `Done` and `Error`. A caller that cannot wait for the consumer, like the
    /// JavaScript side of a response body, can always end the stream it wrote.
    pub fn try_write_chunk(&self, stream_id: StreamId, chunk: StreamChunk) -> Result<(), String> {
        let mut senders = self.senders.lock().unwrap();

        if let Some(tx) = senders.get(&stream_id) {
            let terminal = matches!(chunk, StreamChunk::Done | StreamChunk::Error(_));

            if !terminal && tx.capacity() <= 1 {
                return Err("Stream buffer full (backpressure)".to_string());
            }

            match tx.try_send(chunk) {
                Ok(()) => Ok(()),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    Err("Stream buffer full (backpressure)".to_string())
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // Receiver was dropped (client disconnected) - remove sender from map
                    // This makes has_sender() return false, which __responseStreamIsClosed uses
                    senders.remove(&stream_id);
                    Err("Stream channel closed".to_string())
                }
            }
        } else {
            Err(format!("Stream {} not found", stream_id))
        }
    }

    /// Read the next chunk from a stream (async, called from scheduler)
    /// This temporarily takes ownership of the receiver to await on it
    pub async fn read_chunk(&self, stream_id: StreamId) -> Result<StreamChunk, String> {
        // Take the receiver temporarily (only one reader at a time - matches
        // ReadableStream semantics). Nothing to take means there is nothing
        // to read: the guest closed the stream, or another read holds it. A
        // read that can read nothing neither marks the body read nor asks
        // for it (patch 12: a read after a cancel does not ask the client
        // for the body).
        let rx = {
            let mut receivers = self.receivers.lock().unwrap();
            receivers.remove(&stream_id)
        };
        let Some(mut rx) = rx else {
            return Err(format!(
                "Stream {} not found or already being read",
                stream_id
            ));
        };

        // A read of a host-fed body is noted for the response the guest may
        // give back (patch 6), and it is the guest asking for the body,
        // which the host reads only then (patch 9).
        let host_fed_stream = {
            let mut host_fed = self.host_fed.lock().unwrap();
            match host_fed.get_mut(&stream_id) {
                Some(read) => {
                    *read = true;
                    true
                }
                None => false,
            }
        };
        if host_fed_stream {
            self.mark_asked();
        }

        {
            let result = rx.recv().await;

            // Put it back for next read (unless stream is done/errored)
            if let Some(ref chunk) = result {
                match chunk {
                    StreamChunk::Done | StreamChunk::Error(_) => {
                        // Don't put back - stream is finished
                    }
                    StreamChunk::Data(_) => {
                        // A stream closed while this read was in flight (the
                        // request ended, or the guest cancelled it) stays
                        // closed. The sender is checked and the receiver put
                        // back under the one `senders` lock that
                        // `close_stream` removes it under, so no interleaving
                        // can leave a receiver in the map for a stream with
                        // no sender (patch 11).
                        let senders = self
                            .senders
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if senders.contains_key(&stream_id) {
                            self.receivers
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(stream_id, rx);
                        }
                    }
                }
            }

            result.ok_or_else(|| "Stream closed unexpectedly".to_string())
        }
    }

    /// Take the receiver from a stream (for passing to HttpBody::Stream)
    /// The sender remains active for writing chunks
    pub fn take_receiver(&self, stream_id: StreamId) -> Option<mpsc::Receiver<StreamChunk>> {
        let receiver = self
            .receivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&stream_id);
        // Nothing was there to take (the guest closed it, or a read holds it
        // right now): nothing is asked for and nothing is the host's.
        let receiver = receiver?;
        // A host-fed stream taken for the response's body is one the host is
        // about to send: it asked for it, and the host reads it from then on
        // (patch 9). A stream the guest cancelled was closed, so nothing is
        // taken here and the cancel's "do not ask" stands (patch 12).
        if self
            .host_fed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&stream_id)
        {
            self.mark_asked();
        }
        self.taken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(stream_id);
        Some(receiver)
    }

    /// A guest cancelled a stream (patch 12). A host-fed stream the response
    /// did not take is closed for good, marked used (so patch 6 says it was
    /// read and never sends it back), and the body is not asked for: nothing
    /// is read from the client for it and no `100 Continue` goes out. A
    /// host-fed stream the response took belongs to the host: the cancel is
    /// ignored. Any other stream closes as before.
    pub fn cancel_stream(&self, stream_id: StreamId) {
        if !self.is_host_fed(stream_id) {
            self.close_stream(stream_id);
            return;
        }
        let taken = self
            .taken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&stream_id);
        if taken {
            return;
        }
        if let Some(read) = self
            .host_fed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&stream_id)
        {
            *read = true;
        }
        self.close_stream(stream_id);
    }

    /// Close and remove a stream
    ///
    /// Wakes the event loop, because this is how a guest still writing to the
    /// stream learns that nobody is reading it any more.
    ///
    /// The `senders` lock is held over the `receivers` removal, and a read
    /// that puts its receiver back takes the same lock before it checks the
    /// sender (patch 11): no interleaving of a read and this leaves a
    /// receiver in the map for a stream with no sender. Every lock here is
    /// poison-safe because this is reached from `RequestContext::drop`
    /// (Rust rule 4: no panic on the way out).
    pub fn close_stream(&self, stream_id: StreamId) {
        let mut senders = self
            .senders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        senders.remove(&stream_id);
        self.receivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&stream_id);
        drop(senders);
        self.metadata
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&stream_id);

        self.waker.wake();
    }

    /// Get information about a stream (for debugging)
    pub fn get_stream_info(&self, stream_id: StreamId) -> Option<String> {
        self.metadata.lock().unwrap().get(&stream_id).cloned()
    }

    /// Clear all streams (used for context reuse between requests)
    pub fn clear(&self) {
        self.senders.lock().unwrap().clear();
        self.receivers.lock().unwrap().clear();
        self.metadata.lock().unwrap().clear();
        self.host_fed.lock().unwrap().clear();
        *self.host_response.lock().unwrap() = None;
        self.guest_streams.lock().unwrap().clear();
        self.taken.lock().unwrap().clear();
        // `asked` is not cleared here: it belongs to the request the host is
        // running (the host attaches one per request), and dropping it at a
        // reset would tell the host's body pump that nobody asked.
        // Reset next_id so stream IDs don't grow unbounded across reuses
        *self.next_id.lock().unwrap() = 1;
    }

    /// The channel on which this manager says that the guest (or the
    /// response) has asked for the host-fed body (patch 9): the host waits
    /// on the receiving half before it takes the first byte from the
    /// client.
    pub fn watch_asked(&self, asked: watch::Sender<bool>) {
        *self.asked.lock().unwrap() = Some(asked);
    }

    /// Says that the host-fed body was asked for. Once set it stays set.
    fn mark_asked(&self) {
        if let Some(asked) = self.asked.lock().unwrap().as_ref() {
            asked.send_replace(true);
        }
    }

    /// Closes every host-fed stream the response did not take. Called when
    /// the request ends: the host's pump for such a body has nowhere to
    /// write and ends with it (patch 11). It is the response taking the
    /// stream that keeps it open, not the stream being out of the receivers
    /// map: a guest read still in flight at the request's end has taken the
    /// receiver out of the map without anyone to read what comes next.
    ///
    /// This runs from `RequestContext::drop`, so every lock here recovers a
    /// poisoned mutex instead of panicking (Rust rule 4).
    pub fn close_unread_host_fed(&self) {
        let ids: Vec<StreamId> = {
            let host_fed = self
                .host_fed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            host_fed.keys().copied().collect()
        };
        for id in ids {
            let taken = self
                .taken
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&id);
            if !taken {
                self.close_stream(id);
            }
        }
    }

    /// Refuses the stream past `most` of the guest's own for this request
    /// (patch 8).
    pub fn check_guest_stream(&self, most: usize) -> Result<(), String> {
        let made = self.guest_streams.lock().unwrap();
        if made.len() >= most {
            return Err(format!(
                "at most {most} streams of its own may an Application have open towards the host in one request: this one is one more"
            ));
        }
        Ok(())
    }

    /// Notes a stream as the guest's own (patch 8, patch 12): the only ids
    /// its response natives may write to, end or fail.
    pub fn mark_guest_stream(&self, stream_id: StreamId) {
        self.guest_streams.lock().unwrap().insert(stream_id);
    }

    /// Whether `stream_id` is a stream the guest made itself with
    /// `__responseStreamCreate` in this request (patch 12).
    pub fn is_guest_stream(&self, stream_id: StreamId) -> bool {
        self.guest_streams.lock().unwrap().contains(&stream_id)
    }

    /// Whether `stream_id` is a stream the host feeds (patch 6).
    pub fn is_host_fed(&self, stream_id: StreamId) -> bool {
        self.host_fed.lock().unwrap().contains_key(&stream_id)
    }

    /// Marks a stream as one the host feeds (a request's body; later a
    /// file's), whose reads are noted for the response the guest may give
    /// back (patch 6).
    pub fn mark_host_fed(&self, stream_id: StreamId) {
        self.host_fed.lock().unwrap().insert(stream_id, false);
    }

    /// Whether `stream_id` is a host-fed stream, and whether the guest has
    /// read from it; `None` for any other stream.
    pub fn host_fed_read(&self, stream_id: StreamId) -> Option<bool> {
        self.host_fed.lock().unwrap().get(&stream_id).copied()
    }

    /// Says that the response's body is `stream_id`, when it is a host-fed
    /// stream: the host reads [`StreamManager::host_response`] after the
    /// event. Any other body leaves it `None`.
    pub fn note_host_response(&self, stream_id: StreamId) {
        let read = self.host_fed_read(stream_id);
        *self.host_response.lock().unwrap() = read.map(|read| HostResponse { read });
    }

    /// The response's body, when it was a host-fed stream (patch 6).
    pub fn host_response(&self) -> Option<HostResponse> {
        *self.host_response.lock().unwrap()
    }

    /// Count active streams
    pub fn active_count(&self) -> usize {
        self.senders.lock().unwrap().len()
    }

    /// The sender of a stream, to wait for room in it.
    pub fn sender(&self, stream_id: StreamId) -> Option<mpsc::Sender<StreamChunk>> {
        self.senders.lock().unwrap().get(&stream_id).cloned()
    }

    /// Check if a stream has an active sender (not closed)
    pub fn has_sender(&self, stream_id: StreamId) -> bool {
        self.senders.lock().unwrap().contains_key(&stream_id)
    }

    /// Create a stream and spawn a pump task to forward data from a receiver.
    /// Returns the stream ID that can be passed to JavaScript.
    ///
    /// This is the standard way to bridge a Rust mpsc::Receiver<Result<Bytes, String>>
    /// (e.g., from a streaming HTTP request body) to a JavaScript ReadableStream.
    pub fn pump_request_body(
        self: &std::sync::Arc<Self>,
        rx: tokio::sync::mpsc::Receiver<Result<bytes::Bytes, String>>,
    ) -> StreamId {
        let stream_id = self.create_stream("request_body".to_string());
        // The request's body is the host's, not the guest's: a response that
        // gives it back unread is the host's to send (patch 6).
        self.mark_host_fed(stream_id);
        let stream_manager = self.clone();

        tokio::spawn(async move {
            let mut rx = rx;

            while let Some(result) = rx.recv().await {
                match result {
                    Ok(bytes) => {
                        if stream_manager
                            .write_chunk(stream_id, StreamChunk::Data(bytes))
                            .await
                            .is_err()
                        {
                            // Stream closed by consumer
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = stream_manager
                            .write_chunk(stream_id, StreamChunk::Error(e))
                            .await;
                        break;
                    }
                }
            }

            // Signal end of stream
            let _ = stream_manager
                .write_chunk(stream_id, StreamChunk::Done)
                .await;
        });

        stream_id
    }
}

impl Default for StreamManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stream_manager_create() {
        let manager = StreamManager::new();
        let id = manager.create_stream("https://example.com".to_string());

        assert_eq!(id, 1);
        assert_eq!(manager.active_count(), 1);

        // Write a chunk (now async)
        manager
            .write_chunk(id, StreamChunk::Data(Bytes::from("test")))
            .await
            .unwrap();

        // Read it using read_chunk
        let chunk = manager.read_chunk(id).await.unwrap();
        match chunk {
            StreamChunk::Data(data) => assert_eq!(data, Bytes::from("test")),
            _ => panic!("Expected data chunk"),
        }
    }

    #[tokio::test]
    async fn test_stream_manager_multiple_chunks() {
        let manager = StreamManager::new();
        let id = manager.create_stream("https://example.com".to_string());

        // Write multiple chunks (now async)
        manager
            .write_chunk(id, StreamChunk::Data(Bytes::from("chunk1")))
            .await
            .unwrap();
        manager
            .write_chunk(id, StreamChunk::Data(Bytes::from("chunk2")))
            .await
            .unwrap();
        manager.write_chunk(id, StreamChunk::Done).await.unwrap();

        // Read them using read_chunk
        let chunk1 = manager.read_chunk(id).await.unwrap();
        let chunk2 = manager.read_chunk(id).await.unwrap();
        let done = manager.read_chunk(id).await.unwrap();

        match (chunk1, chunk2, done) {
            (StreamChunk::Data(d1), StreamChunk::Data(d2), StreamChunk::Done) => {
                assert_eq!(d1, Bytes::from("chunk1"));
                assert_eq!(d2, Bytes::from("chunk2"));
            }
            _ => panic!("Expected data chunks + done"),
        }
    }

    #[tokio::test]
    async fn test_stream_manager_close() {
        let manager = StreamManager::new();
        let id = manager.create_stream("https://example.com".to_string());

        assert_eq!(manager.active_count(), 1);

        manager.close_stream(id);

        assert_eq!(manager.active_count(), 0);
    }

    #[tokio::test]
    async fn test_stream_info() {
        let manager = StreamManager::new();
        let id = manager.create_stream("https://api.example.com/data".to_string());

        assert_eq!(
            manager.get_stream_info(id),
            Some("https://api.example.com/data".to_string())
        );
    }

    #[tokio::test]
    async fn test_backpressure() {
        // Create manager with small buffer to test backpressure
        let manager = StreamManager::with_high_water_mark(2);
        let id = manager.create_stream("https://example.com".to_string());

        // Fill the buffer
        manager
            .write_chunk(id, StreamChunk::Data(Bytes::from("1")))
            .await
            .unwrap();
        manager
            .write_chunk(id, StreamChunk::Data(Bytes::from("2")))
            .await
            .unwrap();

        // try_write should fail when buffer is full
        let result = manager.try_write_chunk(id, StreamChunk::Data(Bytes::from("3")));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("backpressure"));

        // Read one to free space
        let _ = manager.read_chunk(id).await.unwrap();

        // Now try_write should succeed
        let result = manager.try_write_chunk(id, StreamChunk::Data(Bytes::from("3")));
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stream_ends_on_a_full_buffer() {
        let manager = StreamManager::with_high_water_mark(2);
        let id = manager.create_stream("https://example.com".to_string());

        manager
            .try_write_chunk(id, StreamChunk::Data(Bytes::from("1")))
            .unwrap();
        manager
            .try_write_chunk(id, StreamChunk::Data(Bytes::from("2")))
            .unwrap();

        // Full for data, still open for the marker that ends the stream.
        assert!(
            manager
                .try_write_chunk(id, StreamChunk::Data(Bytes::from("3")))
                .is_err()
        );
        manager.try_write_chunk(id, StreamChunk::Done).unwrap();

        assert!(matches!(
            manager.read_chunk(id).await.unwrap(),
            StreamChunk::Data(_)
        ));
        assert!(matches!(
            manager.read_chunk(id).await.unwrap(),
            StreamChunk::Data(_)
        ));
        assert!(matches!(
            manager.read_chunk(id).await.unwrap(),
            StreamChunk::Done
        ));
    }

    #[tokio::test]
    async fn test_stream_errors_on_a_full_buffer() {
        let manager = StreamManager::with_high_water_mark(1);
        let id = manager.create_stream("https://example.com".to_string());

        manager
            .try_write_chunk(id, StreamChunk::Data(Bytes::from("1")))
            .unwrap();
        manager
            .try_write_chunk(id, StreamChunk::Error("boom".to_string()))
            .unwrap();

        assert!(matches!(
            manager.read_chunk(id).await.unwrap(),
            StreamChunk::Data(_)
        ));

        match manager.read_chunk(id).await.unwrap() {
            StreamChunk::Error(message) => assert_eq!(message, "boom"),
            other => panic!("expected an error chunk, got {other:?}"),
        }
    }
}
