use super::super::{CallbackId, SchedulerMessage, stream_manager};
use super::state::{ResponseStreamState, StreamState};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::mpsc;
use v8;

/// Native stream read - reads a chunk from a native stream
#[glue_v8::method(state = Rc<StreamState>)]
fn native_stream_read(
    scope: &mut v8::PinScope,
    state: &Rc<StreamState>,
    stream_id: u64,
    resolve: v8::Local<v8::Function>,
) {
    let resolve_global = v8::Global::new(scope.as_ref(), resolve);

    // Generate callback ID
    let callback_id = {
        let mut next_id = state.next_id.borrow_mut();
        let id = *next_id;
        *next_id += 1;
        id
    };

    // Store callback
    state
        .callbacks
        .borrow_mut()
        .insert(callback_id, resolve_global);

    // Send StreamRead message to scheduler
    let _ = state
        .scheduler_tx
        .send(SchedulerMessage::StreamRead(callback_id, stream_id));
}

/// Native stream cancel - cancels a native stream
///
/// The cancel acts on the stream here, inside the guest's call, and not only
/// through a scheduler message: the guest may cancel its body and give it
/// back in the same handler, and the response takes its stream before the
/// scheduler would get to a message (vendor/VENDORED.md, patch 12).
#[glue_v8::method(state = Rc<StreamState>)]
fn native_stream_cancel(scope: &mut v8::PinScope, state: &Rc<StreamState>, stream_id: u64) {
    let _ = scope;
    state.manager.cancel_stream(stream_id);
    let _ = state
        .scheduler_tx
        .send(SchedulerMessage::StreamCancel(stream_id));
}

/// Setup native streaming operations
/// These ops allow JavaScript to read chunks from Rust streams
pub fn setup_stream_ops(
    scope: &mut v8::PinScope,
    scheduler_tx: mpsc::UnboundedSender<SchedulerMessage>,
    stream_callbacks: Rc<RefCell<HashMap<CallbackId, v8::Global<v8::Function>>>>,
    next_callback_id: Rc<RefCell<CallbackId>>,
    manager: Arc<stream_manager::StreamManager>,
) {
    let state = Rc::new(StreamState {
        scheduler_tx,
        callbacks: stream_callbacks,
        next_id: next_callback_id,
        manager,
    });

    // Keeps the Rc alive for the context; the templates below only borrow it.
    crate::context_slots::set(scope, state.clone());

    let stream_read_fn = native_stream_read_v8_template(scope, &state)
        .get_function(scope)
        .unwrap();
    let stream_cancel_fn = native_stream_cancel_v8_template(scope, &state)
        .get_function(scope)
        .unwrap();

    register_fn!(scope, "__nativeStreamRead", stream_read_fn);
    register_fn!(scope, "__nativeStreamCancel", stream_cancel_fn);

    // JavaScript helper: createNativeStream(streamId) -> ReadableStream
    exec_js!(
        scope,
        r#"
        globalThis.__createNativeStream = function(streamId) {
            const stream = new ReadableStream({
                type: 'bytes',
                async pull(controller) {
                    return new Promise((resolve) => {
                        __nativeStreamRead(streamId, (result) => {
                            // A stream cancelled with a read in flight refuses the
                            // chunk that still lands; the read has to settle anyway.
                            try {
                                if (result.error) {
                                    controller.error(new Error(result.error));
                                } else if (result.done) {
                                    controller.close();
                                } else {
                                    controller.enqueue(result.value);
                                }
                            } catch (_) {}
                            resolve();
                        });
                    });
                },
                cancel(reason) {
                    __nativeStreamCancel(streamId);
                }
            });
            // Mark this stream as a native stream so we can forward it directly
            stream._nativeStreamId = streamId;
            return stream;
        };

        // https://fetch.spec.whatwg.org/#null-body-status: Response refuses a body
        // on these, and a constructor that throws inside a native callback leaves
        // the fetch promise pending for the life of the request.
        globalThis.__responseFromMeta = function(meta) {
            const nullBody = [101, 103, 204, 205, 304].includes(meta.status);
            const hasStream = meta.streamId !== undefined && meta.streamId !== null;
            let body = null;

            if (nullBody) {
                if (hasStream) {
                    __nativeStreamCancel(meta.streamId);
                }
            } else if (hasStream) {
                body = __createNativeStream(meta.streamId);
            } else if (meta.body !== undefined) {
                body = meta.body;
            }

            // A 101 of a binding hands over a socket, which this context can
            // only pass on in its own 101
            const webSocket = meta.socketId === undefined || meta.socketId === null
                ? null
                : Object.freeze({ _pairId: meta.socketId });

            return new Response(body, {
                status: meta.status,
                statusText: meta.statusText,
                headers: meta.headers,
                webSocket
            });
        };
    "#
    );
}

/// Create a new response stream
#[glue_v8::method(state = Rc<ResponseStreamState>)]
fn response_stream_create(
    scope: &mut v8::PinScope,
    state: &Rc<ResponseStreamState>,
) -> Result<u64, String> {
    let _ = scope;
    // At most 16 streams of the guest's own towards the host for one request
    // (vendor/VENDORED.md, patch 8): the 17th is an error thrown to the
    // guest, which the platform's glue turns into a failed response body.
    state
        .manager
        .check_guest_stream(stream_manager::MAX_GUEST_STREAMS)?;
    let stream_id = state.manager.create_stream("response".to_string());
    state.manager.mark_guest_stream(stream_id);
    Ok(stream_id)
}

/// Whether `stream_id` is a stream this guest made itself, refusing any
/// other with an error thrown to it (vendor/VENDORED.md, patch 12): the
/// request's own body (a host-fed stream) is not the guest's to write to,
/// end or fail.
fn only_guest_stream(
    state: &ResponseStreamState,
    stream_id: u64,
    what: &str,
) -> Result<(), String> {
    if state.manager.is_guest_stream(stream_id) {
        return Ok(());
    }
    Err(format!(
        "{what}: stream {stream_id} is not a stream this Application made with __responseStreamCreate in this request"
    ))
}

/// Write to a response stream - returns true if written, false if full/error
#[glue_v8::method(state = Rc<ResponseStreamState>)]
fn response_stream_write(
    scope: &mut v8::PinScope,
    state: &Rc<ResponseStreamState>,
    stream_id: u64,
    chunk: v8::Local<v8::Uint8Array>,
) -> Result<bool, String> {
    let _ = scope;
    // Only the guest's own streams can be written to (vendor/VENDORED.md,
    // patch 12): writing to the request's body (a host-fed stream) would put
    // bytes the client never sent into a body the host sends back.
    only_guest_stream(state, stream_id, "a chunk written to the host")?;
    let len = chunk.byte_length();
    // A chunk the guest writes to the host is at most 64 KiB
    // (vendor/VENDORED.md, patch 7): a larger one is refused, an error thrown
    // to the guest.
    if len > stream_manager::MAX_CHUNK_BYTES {
        return Err(format!(
            "a chunk written to the host is at most {} bytes; this one is {len}",
            stream_manager::MAX_CHUNK_BYTES
        ));
    }
    let mut bytes_vec = vec![0u8; len];
    chunk.copy_contents(&mut bytes_vec);

    Ok(state
        .manager
        .try_write_chunk(
            stream_id,
            stream_manager::StreamChunk::Data(bytes::Bytes::from(bytes_vec)),
        )
        .is_ok())
}

/// End a response stream
#[glue_v8::method(state = Rc<ResponseStreamState>)]
fn response_stream_end(
    scope: &mut v8::PinScope,
    state: &Rc<ResponseStreamState>,
    stream_id: u64,
) -> Result<(), String> {
    let _ = scope;
    // Ending a host-fed stream from the guest would cut the request's own
    // body short (vendor/VENDORED.md, patch 12).
    only_guest_stream(state, stream_id, "ending a response stream")?;
    let _ = state
        .manager
        .try_write_chunk(stream_id, stream_manager::StreamChunk::Done);
    Ok(())
}

/// Fail a response stream
///
/// A guest that errors its own body has produced a truncated response, so the
/// host has to receive an error rather than the clean end `response_stream_end`
/// would write.
#[glue_v8::method(state = Rc<ResponseStreamState>)]
fn response_stream_error(
    scope: &mut v8::PinScope,
    state: &Rc<ResponseStreamState>,
    stream_id: u64,
    message: String,
) -> Result<(), String> {
    let _ = scope;
    // Failing a host-fed stream from the guest would fail the request's own
    // body (vendor/VENDORED.md, patch 12).
    only_guest_stream(state, stream_id, "failing a response stream")?;
    let _ = state
        .manager
        .try_write_chunk(stream_id, stream_manager::StreamChunk::Error(message));
    Ok(())
}

/// Check if a response stream is closed
#[glue_v8::method(state = Rc<ResponseStreamState>)]
fn response_stream_is_closed(
    scope: &mut v8::PinScope,
    state: &Rc<ResponseStreamState>,
    stream_id: u64,
) -> bool {
    let _ = scope;
    !state.manager.has_sender(stream_id)
}

/// Setup response streaming operations
/// These allow JS to stream response body chunks back to Rust
pub fn setup_response_stream_ops(
    scope: &mut v8::PinScope,
    stream_manager: Arc<stream_manager::StreamManager>,
) {
    let state = Rc::new(ResponseStreamState {
        manager: stream_manager,
    });

    // Keeps the Rc alive for the context; the templates below only borrow it.
    crate::context_slots::set(scope, state.clone());

    let create_fn = response_stream_create_v8_template(scope, &state)
        .get_function(scope)
        .unwrap();
    let write_fn = response_stream_write_v8_template(scope, &state)
        .get_function(scope)
        .unwrap();
    let end_fn = response_stream_end_v8_template(scope, &state)
        .get_function(scope)
        .unwrap();
    let error_fn = response_stream_error_v8_template(scope, &state)
        .get_function(scope)
        .unwrap();
    let is_closed_fn = response_stream_is_closed_v8_template(scope, &state)
        .get_function(scope)
        .unwrap();

    register_fn!(scope, "__responseStreamCreate", create_fn);
    register_fn!(scope, "__responseStreamWrite", write_fn);
    register_fn!(scope, "__responseStreamEnd", end_fn);
    register_fn!(scope, "__responseStreamError", error_fn);
    register_fn!(scope, "__responseStreamIsClosed", is_closed_fn);
}
