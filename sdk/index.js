// @vm/sdk: what Application code imports (blackbox/CONTRACT.md, "The SDK").
// The Trusted context itself is made by the runtime (runtime.js), which the
// bundle's entry puts around the Application.

export { OperationFailed, Refused } from "./errors.js";

/**
 * The Manifest. Its argument must be a plain literal: the platform reads it
 * from manifest.ts without running any code. At run time it is the value
 * itself.
 */
export function defineManifest(manifest) {
  return manifest;
}

/** The Application: its Manifest and its `fetch(request, ctx)`. */
export function defineApp(manifest, handlers) {
  return Object.freeze({
    manifest,
    fetch(request, ctx) {
      return handlers.fetch(request, ctx);
    },
  });
}
