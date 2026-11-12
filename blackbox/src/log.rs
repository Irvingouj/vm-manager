//! Collects the program's log from its standard error, one line at a time,
//! and lets a test wait, with a timeout, until the lines so far satisfy a
//! condition.
//!
//! Bounded: at most [`MAX_LINES`] lines of at most [`MAX_LINE_BYTES`] bytes
//! are kept. A longer line is cut; lines past the limit are counted and
//! dropped, and every later wait fails saying so, rather than the buffer
//! growing without end.

use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;

/// The most lines kept from one program run.
pub const MAX_LINES: usize = 100_000;
/// The longest line kept; the rest of a longer line is dropped.
pub const MAX_LINE_BYTES: usize = 256 * 1024;

/// One line of the program's standard error.
#[derive(Clone, Debug)]
pub struct Line {
    pub raw: String,
    /// The line parsed as a JSON object, when it is one.
    pub record: Option<Value>,
}

#[derive(Default)]
struct State {
    lines: Vec<Line>,
    dropped: usize,
    ended: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock can only come from this module's
        // own bookkeeping; the lines collected so far are still worth showing.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Why a wait ended without its condition holding.
#[derive(Debug)]
pub enum WaitFailure {
    /// The time ran out.
    TimedOut,
    /// The program closed its standard error (it has exited).
    Ended,
    /// The log grew past [`MAX_LINES`].
    Overflowed,
}

/// The reader of one program's standard error, and what it has read.
pub struct LogCollector {
    shared: Arc<Shared>,
    /// The thread reading the stream. It ends when the stream ends, which
    /// happens when the program exits; `Drop` waits for it.
    reader: Option<JoinHandle<()>>,
}

impl LogCollector {
    /// Starts reading `stream` on a thread owned by the returned collector.
    pub fn spawn(stream: impl Read + Send + 'static) -> Self {
        let shared = Arc::new(Shared::default());
        let writer = Arc::clone(&shared);
        let reader = thread::Builder::new()
            .name("vm-log-reader".to_owned())
            .spawn(move || read_lines(stream, &writer))
            .expect("cannot start the thread that reads the program's log");
        Self {
            shared,
            reader: Some(reader),
        }
    }

    /// Waits until `check` returns something for the lines so far, or until
    /// `timeout`, or until the stream ends with `check` still unsatisfied.
    pub fn wait_for<T>(
        &self,
        timeout: Duration,
        mut check: impl FnMut(&[Line]) -> Option<T>,
    ) -> Result<T, WaitFailure> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        loop {
            if let Some(found) = check(&state.lines) {
                return Ok(found);
            }
            if state.dropped > 0 {
                return Err(WaitFailure::Overflowed);
            }
            if state.ended {
                return Err(WaitFailure::Ended);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(WaitFailure::TimedOut);
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Runs `look` on the lines so far, under the lock.
    pub fn with_lines<T>(&self, look: impl FnOnce(&[Line]) -> T) -> T {
        look(&self.shared.lock().lines)
    }

    /// Whether the stream has ended (the program has exited).
    pub fn ended(&self) -> bool {
        self.shared.lock().ended
    }

    /// The last `n` lines, raw, for failure messages.
    pub fn tail(&self, n: usize) -> String {
        let state = self.shared.lock();
        let skip = state.lines.len().saturating_sub(n);
        let mut text = String::new();
        if skip > 0 {
            text.push_str(&format!("  ... {skip} earlier lines not shown ...\n"));
        }
        for line in state.lines.iter().skip(skip) {
            text.push_str("  ");
            text.push_str(&line.raw);
            text.push('\n');
        }
        if state.dropped > 0 {
            text.push_str(&format!(
                "  ... and {} lines dropped past the limit of {MAX_LINES} ...\n",
                state.dropped
            ));
        }
        if text.is_empty() {
            text.push_str("  (nothing)\n");
        }
        text
    }
}

impl Drop for LogCollector {
    fn drop(&mut self) {
        // The program has been ended by now, so the stream is closing. Wait
        // a bounded time for the reader; a reader still blocked after that
        // (a grandchild holding the pipe) is left to end with the pipe.
        if let Some(reader) = self.reader.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !reader.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if reader.is_finished() {
                // The reader does not panic; if it did, the test has
                // already seen everything it could tell.
                drop(reader.join());
            }
        }
    }
}

fn read_lines(stream: impl Read, shared: &Shared) {
    let mut reader = BufReader::new(stream);
    let mut buffer = Vec::with_capacity(1024);
    loop {
        buffer.clear();
        let limit = u64::try_from(MAX_LINE_BYTES).unwrap_or(u64::MAX);
        let read = (&mut reader).take(limit).read_until(b'\n', &mut buffer);
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let cut = !buffer.ends_with(b"\n");
        if cut && buffer.len() >= MAX_LINE_BYTES {
            skip_rest_of_line(&mut reader);
        }
        let raw = String::from_utf8_lossy(&buffer)
            .trim_end_matches(['\r', '\n'])
            .to_owned();
        let record = serde_json::from_str::<Value>(&raw)
            .ok()
            .filter(Value::is_object);
        let mut state = shared.lock();
        if state.lines.len() < MAX_LINES {
            state.lines.push(Line { raw, record });
        } else {
            state.dropped += 1;
        }
        drop(state);
        shared.changed.notify_all();
    }
    shared.lock().ended = true;
    shared.changed.notify_all();
}

fn skip_rest_of_line(reader: &mut impl BufRead) {
    loop {
        let (done, used) = match reader.fill_buf() {
            Ok([]) | Err(_) => return,
            Ok(available) => match available.iter().position(|b| *b == b'\n') {
                Some(at) => (true, at + 1),
                None => (false, available.len()),
            },
        };
        reader.consume(used);
        if done {
            return;
        }
    }
}
