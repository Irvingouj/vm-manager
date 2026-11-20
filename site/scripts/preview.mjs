// Serves dist/ on 127.0.0.1 under the base path it was built with, as a
// static host would. For looking at a build, not for production.
//   npm run preview                                  http://127.0.0.1:4321/
//   BASE_PATH=/vm-manager/ PORT=8000 npm run preview http://127.0.0.1:8000/vm-manager/
import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { join, extname, dirname, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const DIST = join(dirname(fileURLToPath(import.meta.url)), "..", "dist");
let BASE = process.env.BASE_PATH || "/";
if (!BASE.startsWith("/")) BASE = "/" + BASE;
if (!BASE.endsWith("/")) BASE += "/";
const PORT = Number(process.env.PORT || 4321);
const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json",
  ".svg": "image/svg+xml",
  ".mp4": "video/mp4",
  ".jpg": "image/jpeg",
};

async function file(path) {
  try {
    const s = await stat(path);
    if (s.isDirectory()) return file(join(path, "index.html"));
    return { path, body: await readFile(path) };
  } catch {
    return null;
  }
}

createServer(async (req, res) => {
  const url = decodeURIComponent(new URL(req.url, "http://x").pathname);
  if (url + "/" === BASE) {
    res.writeHead(301, { location: BASE }).end();
    return;
  }
  const rel = url.startsWith(BASE) ? normalize(url.slice(BASE.length)) : null;
  const found = rel !== null && !rel.startsWith("..") ? await file(join(DIST, rel)) : null;
  if (found && !url.endsWith("/") && found.path.endsWith("index.html") && !url.endsWith(".html")) {
    res.writeHead(301, { location: url + "/" }).end();
    return;
  }
  const hit = found ?? (await file(join(DIST, "404.html")));
  res.writeHead(found ? 200 : 404, { "content-type": TYPES[extname(hit.path)] || "application/octet-stream" });
  res.end(hit.body);
}).listen(PORT, "127.0.0.1", () => console.log(`Serving dist/ at http://127.0.0.1:${PORT}${BASE}`));
