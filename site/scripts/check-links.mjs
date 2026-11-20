// Checks dist/ after a build: every internal link and asset resolves (with its
// #fragment, when it has one), and nothing is loaded from another origin.
//   npm run check                          for a build with the base path /
//   BASE_PATH=/vm-manager/ npm run check   for a build with /vm-manager/
import { readFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const DIST = join(dirname(fileURLToPath(import.meta.url)), "..", "dist");
let BASE = process.env.BASE_PATH || "/";
if (!BASE.startsWith("/")) BASE = "/" + BASE;
if (!BASE.endsWith("/")) BASE += "/";

const files = [];
(function walk(d) {
  for (const f of readdirSync(d)) {
    const p = join(d, f);
    if (statSync(p).isDirectory()) walk(p);
    else if (p.endsWith(".html")) files.push(p);
  }
})(DIST);

const ids = new Map();
const idsOf = (file) => {
  if (!ids.has(file)) ids.set(file, new Set([...readFileSync(file, "utf8").matchAll(/\sid="([^"]+)"/g)].map((m) => m[1])));
  return ids.get(file);
};

const problems = [];
let checked = 0;
for (const file of files) {
  const html = readFileSync(file, "utf8");
  const rel = file.slice(DIST.length);
  // What a browser loads by itself: scripts, styles, icons, media, images, frames.
  for (const m of html.matchAll(/<(script|link|img|video|source|iframe|audio)\b[^>]*?\s(src|href|poster)="([^"]*)"/g)) {
    if (/^(https?:)?\/\//.test(m[3])) problems.push(`${rel}: loads ${m[3]} from another origin`);
  }
  for (const m of html.matchAll(/\s(?:href|src|poster)="([^"]*)"/g)) {
    const url = m[1].replace(/&amp;/g, "&");
    if (/^[a-z][a-z0-9+.-]*:/i.test(url) || url.startsWith("//")) continue;
    checked++;
    const [path, hash] = url.split("#");
    let target;
    if (path === "") target = file;
    else if (!path.startsWith(BASE)) {
      problems.push(`${rel}: ${url} is not under the base path ${BASE}`);
      continue;
    } else {
      target = join(DIST, decodeURIComponent(path.slice(BASE.length)));
      if (path.endsWith("/")) target = join(target, "index.html");
      if (!existsSync(target)) {
        problems.push(`${rel}: ${url} does not exist`);
        continue;
      }
    }
    if (hash && target.endsWith(".html") && !idsOf(target).has(decodeURIComponent(hash))) {
      problems.push(`${rel}: ${url} names no id on its page`);
    }
  }
}

if (problems.length) {
  console.error(problems.join("\n"));
  console.error(`${problems.length} problems in ${files.length} pages.`);
  process.exit(1);
}
console.log(`${files.length} pages, ${checked} internal links and assets: every one resolves; nothing is loaded from another origin.`);
