// The platform's glue, run in every fresh context before the Application's
// code. It is joined into one line before the Application's first line, so
// the Application's line numbers stay right: every statement ends with a
// semicolon, and comments stand on lines of their own.
//
// What it does:
// - captures, before Application code can replace them, every built-in it
//   uses later;
// - once the Application has loaded (install, called by the epilogue),
//   takes the handler the Application registered as globalThis.default and
//   puts the platform's own handler there, so the runtime calls the
//   platform's handler;
// - for each request, asks the host once whose request it is (binding
//   __vm_platform, operation begin; read by the host from the request in
//   flight), then calls the Application's fetch(request, ctx) with a frozen
//   ctx whose user() answers that User, until the request is over;
// - reports what the Application throws to the host (operation uncaught),
//   which logs it and answers the User with the platform's fixed 500;
// - hands Storage bodies across the raw interface as streams (CONTRACT.md,
//   "Storage"; ADR 0019, "Bodies: a handle and a stream"): a put's `body`
//   that JSON cannot carry becomes a marker naming a stream the host can
//   read (one the host itself feeds by its id, or one the glue pumps the
//   code's own stream or bytes into, locked from the call on), and a get's
//   answer names the stream the host feeds, which the glue turns into the
//   ReadableStream the code reads.
//
// The identity is the host's: nothing here decides who is asking. A
// hostile Application that breaks this glue fools only itself.
const __vm_platform_glue_5c1e = (function (global) {
  'use strict';
  const nativeInvoke = global.__nativeBindingInvoke;
  const freeze = Object.freeze;
  const defineProperty = Object.defineProperty;
  const apply = Reflect.apply;
  const PromiseCtor = Promise;
  const promiseThen = Promise.prototype.then;
  const promiseResolve = Promise.resolve;
  const ErrorCtor = Error;
  const TypeErrorCtor = TypeError;
  const StringCtor = String;
  const ResponseCtor = Response;
  const PLATFORM = '__vm_platform';
  const noop = function () {};
  let handler = null;
  let installed = false;
  let user = null;
  let outstanding = 0;
  let over = false;
  const later = function (callback) {
    apply(promiseThen, apply(promiseResolve, PromiseCtor, []), [callback]);
  };
  const describe = function (error) {
    try {
      if (error !== null && typeof error === 'object') {
        const stack = error.stack;
        if (typeof stack === 'string' && stack.length > 0) {
          return stack;
        }
      }
      return StringCtor(error);
    } catch (e) {
      return 'a thrown value that cannot be shown';
    }
  };
  const report = function (error) {
    nativeInvoke(PLATFORM, 'uncaught', { error: describe(error) }, noop, noop);
  };
  const settled = function () {
    outstanding -= 1;
    if (outstanding <= 0) {
      over = true;
    }
  };
  const track = function (promise) {
    outstanding += 1;
    apply(promiseThen, apply(promiseResolve, PromiseCtor, [promise]), [settled, settled]);
  };
  const currentUser = function () {
    if (over || user === null) {
      throw new ErrorCtor('no request in flight: ctx.user() was called after its request was over');
    }
    return user;
  };
  const failure = function () {
    return new ResponseCtor('', { status: 500 });
  };
  // A response whose body is a stream the code still holds a reader on is
  // not one it can give back (CONTRACT.md, "Any other response body": as
  // `new Response` of a locked stream throws in a browser). The lock is
  // read through the getter captured here, before Application code runs, so
  // a guest that redefines `locked` on its stream or on
  // `ReadableStream.prototype`, or that replaces the stream's prototype,
  // does not change the answer. A stream of the guest's own or the request's
  // body is recognised by `instanceof` or by the native id it carries.
  const StreamCtor = global.ReadableStream;
  const lockedGetter = (function () {
    try {
      const descriptor =
        StreamCtor === undefined ? null : Object.getOwnPropertyDescriptor(StreamCtor.prototype, 'locked');
      return descriptor !== null && typeof descriptor.get === 'function' ? descriptor.get : null;
    } catch (e) {
      return null;
    }
  })();
  const holdsReaderOn = function (response) {
    let body;
    try {
      body = response.body;
    } catch (e) {
      return false;
    }
    if (body === null || typeof body !== 'object' || lockedGetter === null) {
      return false;
    }
    let isStream = false;
    try {
      isStream = (StreamCtor !== undefined && body instanceof StreamCtor) || typeof body._nativeStreamId === 'number';
    } catch (e) {
      isStream = false;
    }
    if (!isStream) {
      return false;
    }
    try {
      return apply(lockedGetter, body, []) === true;
    } catch (e) {
      return false;
    }
  };
  // Storage bodies across the raw interface (CONTRACT.md, "Storage"; ADR
  // 0019, "Bodies: a handle and a stream"). What the arguments cannot
  // carry as JSON — a stream, an ArrayBuffer, a view of one — the glue
  // hands the host as a marker naming a stream it can read; a get's
  // answer names the stream the host feeds, which the glue turns into the
  // ReadableStream the code reads. Nothing here decides who is allowed or
  // what a key may be: every call reaches the host, which decides and
  // logs it. A stream given to a put is the put's from the call on: the
  // pump takes its reader, which locks it, and its `cancel` is refused
  // (see `claimStream`), so the code can neither read it nor cancel it.
  const CHUNK = 65536;
  const nativeCreateStream = global.__createNativeStream;
  const nativeResponseCreate = global.__responseStreamCreate;
  const nativeResponseWrite = global.__responseStreamWrite;
  const nativeResponseEnd = global.__responseStreamEnd;
  const nativeResponseError = global.__responseStreamError;
  const nativeResponseIsClosed = global.__responseStreamIsClosed;
  const nativeScheduleWritable = global.__nativeScheduleWritable;
  const TextEncoderCtor = global.TextEncoder;
  // A chunk of a stream given to a put: its bytes (a `Uint8Array`, any
  // other view of one, an `ArrayBuffer`) or a string, written as UTF-8.
  // `null` for anything else, which fails the put as an errored stream
  // does — a chunk is never left out of a file that then lands as if whole
  // (CONTRACT.md, "Storage: Through the SDK").
  const viewBytes = function (value) {
    if (value instanceof Uint8Array) return value;
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    if (typeof value === 'string') return new TextEncoderCtor().encode(value);
    return null;
  };
  // Waits for room in the stream the glue writes to, as the runtime's own
  // response pump waits (its `__nativeScheduleWritable`).
  const writable = function (id) {
    return new PromiseCtor(function (resolve) {
      const timer = globalThis.__nextTimerId++;
      globalThis.__timerCallbacks.set(timer, resolve);
      nativeScheduleWritable(timer, id);
    });
  };
  // Writes `bytes` to the stream `id` in pieces of at most 64 KiB,
  // waiting for room between them: an empty chunk writes nothing, and the
  // file has the same bytes however large the chunks the code made.
  const writePieces = async function (id, bytes) {
    for (let at = 0; at < bytes.length; at += CHUNK) {
      const piece = bytes.subarray(at, Math.min(at + CHUNK, bytes.length));
      for (;;) {
        if (nativeResponseWrite(id, piece)) break;
        if (nativeResponseIsClosed(id)) return false;
        await writable(id);
      }
    }
    return true;
  };
  const pumpBytes = async function (id, bytes) {
    if (await writePieces(id, bytes)) nativeResponseEnd(id);
  };
  const pumpStream = async function (id, stream) {
    let failed = null;
    try {
      const reader = stream.getReader();
      for (;;) {
        const read = await reader.read();
        if (read.done) break;
        const bytes = viewBytes(read.value);
        if (bytes === null) {
          // Not bytes and not a string: the put fails `body-failed`, as a
          // stream that errors does, and nothing lands.
          throw new TypeErrorCtor('a chunk of a stream given to a put must be a Uint8Array, an ArrayBuffer, a view of one, or a string');
        }
        if (bytes.length === 0) continue;
        if (!(await writePieces(id, bytes))) return;
      }
    } catch (error) {
      failed = error;
    }
    // A stream that failed ends the put as `body-failed`, never as if it
    // had ended cleanly.
    if (failed !== null) nativeResponseError(id, describe(failed));
    else nativeResponseEnd(id);
  };
  // Whether a stream can still be read: one locked (another reader holds
  // it) or already read to its end cannot be. The state is read through
  // the guest's own implementation marker, as the locked getter above is.
  const streamState = function (stream) {
    if (stream.locked === true) return 'locked';
    const state = stream._state;
    return (state === 'closed' || state === 'errored') ? 'ended' : 'readable';
  };
  // What the glue hands the host in place of a put's `body`, or the body
  // itself when JSON can carry it, or nothing when it can carry the
  // refusal too (a number, a boolean, a Blob: the host answers
  // `malformed`).
  const putBody = function (body) {
    if (typeof body === 'string' || body === null) return body;
    if (body !== null && typeof body === 'object') {
      if (typeof body._nativeStreamId === 'number') {
        // The stream is the put's from the call on: like a body given to
        // fetch, it cannot be cancelled any more (`cancel()` rejects with
        // a TypeError), and the put goes on.
        claimStream(body);
        return { $native: body._nativeStreamId };
      }
      if (StreamCtor !== undefined && body instanceof StreamCtor) {
        const state = streamState(body);
        if (state === 'locked') return { $locked: true };
        if (state === 'ended') return { $closed: true };
        let id;
        try {
          id = nativeResponseCreate();
        } catch (e) {
          // The code may not have another stream of its own (the bound is
          // 16 a request): the host answers `unavailable` in its place in
          // the order.
          return { $busy: true };
        }
        // The pump takes the stream's reader, which locks it, and the
        // stream is the put's from the call on.
        pumpStream(id, body);
        claimStream(body);
        return { $pumped: id };
      }
      if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) {
        const bytes = body instanceof ArrayBuffer ? new Uint8Array(body) : new Uint8Array(body.buffer, body.byteOffset, body.byteLength);
        let id;
        try {
          id = nativeResponseCreate();
        } catch (e) {
          return { $busy: true };
        }
        pumpBytes(id, bytes);
        return { $pumped: id, $length: bytes.length };
      }
    }
    return undefined;
  };
  // A stream a put has taken: the code can neither read it (the pump's
  // reader holds it) nor cancel it. A browser's Streams refuses `cancel()`
  // of a locked stream with a TypeError; the surface here does not, so the
  // platform's glue refuses it: the stream belongs to the host until the
  // put has ended.
  const claimStream = function (stream) {
    defineProperty(stream, 'cancel', {
      value: function () {
        return PromiseCtor.reject(new TypeErrorCtor('the stream was given to a put: it is locked, and the put goes on'));
      },
      writable: false,
      enumerable: false,
      configurable: false,
    });
  };
  // A get's answer names the stream the host feeds: turned into the
  // ReadableStream it is before the code sees the answer.
  const answered = function (command, resolve) {
    return function (answer) {
      if (command === 'storage.get' && answer !== null && typeof answer === 'object') {
        const ok = answer.ok;
        if (ok !== null && typeof ok === 'object' && ok.body !== null && typeof ok.body === 'object' && typeof ok.body.$stream === 'number') {
          ok.body = nativeCreateStream(ok.body.$stream);
        }
      }
      resolve(answer);
    };
  };
  const invokeThrough = function (binding, command, args, resolve, reject) {
    if (command === 'storage.put' && args !== null && typeof args === 'object' && !Array.isArray(args)) {
      const given = args.body;
      if (given !== undefined) {
        const body = putBody(given);
        if (body !== undefined) args.body = body;
      }
    }
    nativeInvoke(binding, command, args, answered(command, resolve), reject);
  };
  defineProperty(global, '__nativeBindingInvoke', { value: invokeThrough, writable: true, enumerable: false, configurable: true });
  const serve = function (request, runtimeContext) {
    return new PromiseCtor(function (resolve) {
      const fail = function (error) {
        report(error);
        resolve(failure());
      };
      const start = function () {
        let runtimeWaitUntil = null;
        try {
          if (runtimeContext !== null && typeof runtimeContext === 'object' && typeof runtimeContext.waitUntil === 'function') {
            runtimeWaitUntil = runtimeContext.waitUntil;
          }
        } catch (e) {
          runtimeWaitUntil = null;
        }
        const ctx = freeze({
          user: function () {
            return currentUser();
          },
          waitUntil: function (promise) {
            if (!over) {
              track(promise);
            }
            if (runtimeWaitUntil !== null) {
              runtimeWaitUntil(promise);
            }
          },
        });
        outstanding += 1;
        let result;
        try {
          const registered = handler;
          if (registered === null || (typeof registered !== 'object' && typeof registered !== 'function')) {
            throw new ErrorCtor('the Application registered no handler: assign globalThis.default = { fetch(request, ctx) { ... } }');
          }
          const fetch = registered.fetch;
          if (typeof fetch !== 'function') {
            throw new ErrorCtor('the Application registered no fetch function: assign globalThis.default = { fetch(request, ctx) { ... } }');
          }
          result = apply(fetch, registered, [request, ctx]);
        } catch (error) {
          settled();
          fail(error);
          return;
        }
        apply(promiseThen, apply(promiseResolve, PromiseCtor, [result]), [
          function (response) {
            settled();
            if (response instanceof ResponseCtor) {
              if (holdsReaderOn(response)) {
                fail(new ErrorCtor('the response body is a ReadableStream the code still holds a reader on: release the reader before giving the body back'));
                return;
              }
              resolve(response);
            } else {
              fail(new ErrorCtor('the fetch handler did not answer with a Response'));
            }
          },
          function (error) {
            settled();
            fail(error);
          },
        ]);
      };
      if (user !== null) {
        start();
        return;
      }
      nativeInvoke(PLATFORM, 'begin', null, function (answer) {
        if (answer === null || typeof answer !== 'object' || answer.ok === null || typeof answer.ok !== 'object') {
          fail(new ErrorCtor('the platform could not say whose request this is'));
          return;
        }
        const given = answer.ok.user;
        freeze(given.groups);
        user = freeze(given);
        later(start);
      }, fail);
    });
  };
  const platformHandler = freeze({
    fetch: function (request, env, runtimeContext) {
      return serve(request, runtimeContext);
    },
  });
  const install = function () {
    if (installed) {
      return;
    }
    installed = true;
    handler = global.default;
    defineProperty(global, 'default', { value: platformHandler, writable: false, enumerable: false, configurable: false });
  };
  return freeze({ install: install });
})(globalThis);
