export { Refused } from "./refused.js";

/** Returns the manifest unchanged. `vm check` and `vm deploy` read it from manifest.ts as a plain literal. */
export function defineManifest(manifest) {
  return manifest;
}

/** Registers the Application's handlers. The platform calls `fetch(request, ctx)` for every request. */
export function defineApp(manifest, handlers) {
  if (!handlers || typeof handlers.fetch !== "function") {
    throw new TypeError("defineApp(manifest, { fetch }) needs a fetch(request, ctx) function.");
  }
  return { manifest, fetch: handlers.fetch };
}
