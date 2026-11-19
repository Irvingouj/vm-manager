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
//   which logs it and answers the User with the platform's fixed 500.
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
