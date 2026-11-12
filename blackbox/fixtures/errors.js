// An Application that fails in the ways the program answers with its fixed
// 500. Each error's message holds a marker the tests look for, and is
// thrown from a function whose name the tests look for in the stack: neither
// may reach the User, both must reach the `uncaught` record.
//
// Routes:
//   /ok            200 "ok"
//   /throw         fetch throws synchronously
//   /async-throw   async fetch throws after an await
//   /reject        fetch returns a rejected promise
//   /throw-string  fetch throws a string, not an Error
//   /not-response  fetch answers a plain object
//   /string        fetch answers a string
//   /nothing       fetch answers undefined

function failInsideTheNamedFunctionForTheStack(marker) {
  throw new Error(`the secret message ${marker}`);
}

globalThis.default = {
  fetch(request, ctx) {
    const path = new URL(request.url).pathname;
    switch (path) {
      case "/ok":
        return new Response("ok");
      case "/throw":
        failInsideTheNamedFunctionForTheStack("SECRET-SYNC-7f3a");
        break;
      case "/async-throw":
        return (async () => {
          await null;
          failInsideTheNamedFunctionForTheStack("SECRET-ASYNC-91c2");
        })();
      case "/reject":
        return Promise.reject(new Error("the secret message SECRET-REJECT-4d0e"));
      case "/throw-string":
        throw "the secret message SECRET-STRING-55ab";
      case "/not-response":
        return { body: "SECRET-OBJECT-2b8f", status: 200 };
      case "/string":
        return "SECRET-TEXT-a17c";
      case "/nothing":
        return undefined;
    }
    return new Response("no such route", { status: 404 });
  },
};
