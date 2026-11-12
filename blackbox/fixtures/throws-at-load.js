// An Application whose top level throws while loading, before it registers
// a handler. The message holds a marker, and the throw comes from a function
// whose name the tests look for in the stack.

function failWhileLoadingInTheNamedFunction() {
  throw new Error("the secret message SECRET-LOAD-c3d9");
}

failWhileLoadingInTheNamedFunction();

globalThis.default = {
  fetch() {
    return new Response("unreachable");
  },
};
