// Builds the website (src/landing.html) and the documentation site (pages.mjs)
// into dist/, as static files served from a base path.
//
//   npm run build                          base path /
//   BASE_PATH=/vm-manager/ npm run build   base path /vm-manager/
//   npm run build -- --base /vm-manager/   the same
//
// The documentation is read from the repository's own documents at build
// time (see pages.mjs). Nothing here needs a secret or the network.

import { readFileSync, writeFileSync, mkdirSync, rmSync, existsSync, readdirSync, copyFileSync } from "node:fs";
import { dirname, join, posix, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Marked } from "marked";
import { groups } from "./pages.mjs";

const SITE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(SITE, "..");
const DIST = join(SITE, "dist");
const GITHUB = "https://github.com/Irvingouj/vm-manager";

// ------------------------------------------------------------------ base path

function baseFromArgs() {
  const i = process.argv.indexOf("--base");
  if (i !== -1 && process.argv[i + 1]) return process.argv[i + 1];
  const eq = process.argv.find((a) => a.startsWith("--base="));
  if (eq) return eq.slice("--base=".length);
  return process.env.BASE_PATH || "/";
}
let BASE = baseFromArgs();
if (!BASE.startsWith("/")) BASE = "/" + BASE;
if (!BASE.endsWith("/")) BASE += "/";
if (/[\s"'<>]/.test(BASE)) {
  console.error(`The base path ${JSON.stringify(BASE)} cannot be used: it must be a URL path such as /vm-manager/.`);
  process.exit(2);
}

// ------------------------------------------------------------------ the latest release

const releases = readdirSync(join(REPO, "docs/releases"))
  .map((f) => /^v(\d+)\.(\d+)\.(\d+)\.md$/.exec(f))
  .filter(Boolean)
  .sort((a, b) => b[1] - a[1] || b[2] - a[2] || b[3] - a[3]);
if (releases.length === 0) {
  console.error("No release notes in docs/releases/.");
  process.exit(1);
}
const VERSION = `${releases[0][1]}.${releases[0][2]}.${releases[0][3]}`;
const RELEASE_FILE = `docs/releases/v${VERSION}.md`;

// ------------------------------------------------------------------ helpers

const esc = (s) =>
  String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

/** GitHub's heading ids: lower case, punctuation removed, spaces as hyphens. */
function slugify(text) {
  return text
    .toLowerCase()
    .trim()
    .replace(/<[^>]*>/g, "")
    .replace(/[^\p{L}\p{N}\s_-]/gu, "")
    .replace(/\s/g, "-");
}

function plain(md) {
  return md.replace(/`([^`]*)`/g, "$1").replace(/\*\*([^*]*)\*\*/g, "$1").replace(/\[([^\]]*)\]\([^)]*\)/g, "$1");
}

const read = (p) => readFileSync(p, "utf8");

/** Cuts a Markdown text at its "## " headings, outside code fences. */
function cut(text) {
  const lines = text.split("\n");
  const out = { preamble: [], sections: new Map() };
  let current = out.preamble;
  let fence = null;
  for (const line of lines) {
    const f = /^\s*(```+|~~~+)/.exec(line);
    if (f) {
      if (fence === null) fence = f[1];
      else if (line.trim().startsWith(fence)) fence = null;
    }
    if (fence === null && line.startsWith("## ")) {
      current = [line];
      out.sections.set(line.trim(), current);
      continue;
    }
    current.push(line);
  }
  return out;
}

// ------------------------------------------------------------------ the pages' Markdown

const pages = groups.flatMap((g) => g.pages.map((p) => ({ ...p, group: g.title })));

/** Each part becomes { md, from }: `from` is the repository path its links are relative to. */
function partsOf(page) {
  return page.parts.map((part) => {
    if (part.md) return { md: part.md, from: null };
    if (part.file) return { md: read(join(SITE, part.file)).replaceAll("{{version}}", VERSION), from: null };
    if (part.release) return { md: read(join(REPO, RELEASE_FILE)), from: RELEASE_FILE };
    if (part.sdk) return { md: sdkMarkdown(part.sdk), from: part.sdk };
    const text = read(join(REPO, part.repo));
    if (part.preamble) return { md: cut(text).preamble.join("\n"), from: part.repo };
    if (part.sections) {
      const { sections } = cut(text);
      const md = part.sections
        .map((h) => {
          const s = sections.get(h);
          if (!s) throw new Error(`${part.repo} has no section "${h}" (pages.mjs, page "${page.title}")`);
          return s.join("\n");
        })
        .join("\n\n");
      return { md, from: part.repo };
    }
    return { md: text, from: part.repo };
  });
}

function sdkMarkdown(path) {
  const text = read(join(REPO, path));
  const chunks = [{ name: "At a glance", lines: [] }];
  for (const line of text.split("\n")) {
    const m = /^\/\/ -{5,} (.+)$/.exec(line);
    if (m) chunks.push({ name: m[1].charAt(0).toUpperCase() + m[1].slice(1), lines: [] });
    else chunks[chunks.length - 1].lines.push(line);
  }
  return chunks
    .map((c) => `## ${c.name}\n\n\`\`\`ts\n${c.lines.join("\n").trim()}\n\`\`\``)
    .join("\n\n");
}

const pageUrl = (slug) => (slug ? `docs/${slug}/` : "docs/");

// Every page's headings, for links that name a heading of another page.
const anchors = new Map(); // id -> page slug (first page that has it)
for (const page of pages) {
  page.md = partsOf(page);
  for (const part of page.md) {
    for (const m of part.md.matchAll(/^#{2,4} (.+)$/gm)) {
      const id = slugify(plain(m[1]));
      if (!anchors.has(id)) anchors.set(id, page.slug);
    }
  }
}

// Repository documents that are pages of this site.
const repoPages = new Map([
  ["docs/getting-started.md", "getting-started"],
  ["deploy/README.md", "admin/install"],
  [RELEASE_FILE, "releases"],
  ["CONTEXT.md", "glossary"],
  ["sdk/index.d.ts", "builders/sdk"],
]);

function rewriteHref(href, from, pageSlug) {
  if (/^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("//")) return href;
  const [path, hash] = href.split("#");
  if (!path) {
    // A heading of the same document: it may now be on another page.
    const target = anchors.get(hash);
    if (target !== undefined && target !== pageSlug) return BASE + pageUrl(target) + "#" + hash;
    return "#" + hash;
  }
  if (from === null) {
    // This site's own words: a path from /docs/.
    return BASE + "docs/" + path + (hash ? "#" + hash : "");
  }
  const repoPath = posix.normalize(posix.join(posix.dirname(from), path)).replace(/\/$/, "");
  if (repoPages.has(repoPath)) {
    let slug = repoPages.get(repoPath);
    if (hash && anchors.has(hash)) slug = anchors.get(hash);
    return BASE + pageUrl(slug) + (hash ? "#" + hash : "");
  }
  const kind = existsSync(join(REPO, repoPath)) && !repoPath.includes(".") ? "tree" : "blob";
  return `${GITHUB}/${kind}/main/${repoPath}${hash ? "#" + hash : ""}`;
}

// ------------------------------------------------------------------ rendering

function render(page) {
  const toc = [];
  const used = new Map();
  let html = "";
  for (const part of page.md) {
    const marked = new Marked({
      gfm: true,
      walkTokens(token) {
        if (token.type === "link") token.href = rewriteHref(token.href, part.from, page.slug);
      },
      renderer: {
        heading({ tokens, depth, text }) {
          if (depth === 1) return ""; // the page's own title is its h1
          let id = slugify(plain(text));
          const n = used.get(id) ?? 0;
          used.set(id, n + 1);
          if (n > 0) id = `${id}-${n}`;
          const inner = this.parser.parseInline(tokens);
          if (depth <= 3) toc.push({ id, depth, html: inner });
          return `<h${depth} id="${esc(id)}"><a class="anchor" href="#${esc(id)}" aria-hidden="true" tabindex="-1">#</a>${inner}</h${depth}>\n`;
        },
        code({ text, lang }) {
          const l = (lang || "").split(/\s/)[0];
          const label = l && l !== "text" ? `<span class="lang">${esc(l)}</span>` : "";
          return `<div class="code">${label}<pre tabindex="0"><code${l ? ` class="language-${esc(l)}"` : ""}>${esc(text)}</code></pre></div>\n`;
        },
      },
    });
    html += marked.parse(part.md).replace(/<table>/g, '<div class="table"><table>').replace(/<\/table>/g, '</table></div>');
  }
  return { html, toc };
}

/** Search entries: one per heading, with its text, from the rendered HTML. */
function searchEntries(page, html) {
  const entries = [];
  const parts = html.split(/(?=<h[234] id=")/);
  for (const chunk of parts) {
    const m = /^<h[234] id="([^"]+)">(.*?)<\/h[234]>/s.exec(chunk);
    const id = m ? m[1] : "";
    const title = m ? m[2].replace(/<a class="anchor"[^>]*>#<\/a>/, "").replace(/<[^>]+>/g, "") : page.title;
    const body = (m ? chunk.slice(m[0].length) : chunk)
      .replace(/<[^>]+>/g, " ")
      .replace(/&lt;/g, "<")
      .replace(/&gt;/g, ">")
      .replace(/&quot;/g, '"')
      .replace(/&#39;/g, "'")
      .replace(/&amp;/g, "&")
      .replace(/\s+/g, " ")
      .trim();
    if (!body && !m) continue;
    entries.push({ p: page.title, h: title === page.title ? "" : decode(title), u: pageUrl(page.slug) + (id ? "#" + id : ""), t: body.slice(0, 1200) });
  }
  return entries;
}
const decode = (s) => s.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, "&");

// ------------------------------------------------------------------ the shell

const layout = read(join(SITE, "src/layout.html"));

function fill(template, values) {
  return template.replace(/\{\{(\w+)\}\}/g, (_, k) => {
    if (!(k in values)) throw new Error(`no value for {{${k}}}`);
    return values[k];
  });
}

function sidebar(current) {
  return groups
    .map(
      (g) =>
        `<p class="nav-group">${esc(g.title)}</p><ul>` +
        g.pages
          .map(
            (p) =>
              `<li><a href="${BASE}${pageUrl(p.slug)}"${p.slug === current ? ' aria-current="page"' : ""}>${esc(p.title)}</a></li>`,
          )
          .join("") +
        "</ul>",
    )
    .join("\n");
}

function tocHtml(toc) {
  if (toc.length < 2) return "";
  return (
    '<nav class="toc" aria-label="On this page"><p class="toc-title">On this page</p><ul>' +
    toc.map((t) => `<li class="d${t.depth}"><a href="#${esc(t.id)}">${t.html.replace(/<a [^>]*>|<\/a>/g, "")}</a></li>`).join("") +
    "</ul></nav>"
  );
}

// ------------------------------------------------------------------ write

rmSync(DIST, { recursive: true, force: true });
mkdirSync(DIST, { recursive: true });

const write = (rel, content) => {
  const p = join(DIST, rel);
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, content);
};

const index = [];
pages.forEach((page, i) => {
  const { html, toc } = render(page);
  index.push(...searchEntries(page, html));
  const prev = pages[i - 1];
  const next = pages[i + 1];
  const pager =
    `<nav class="pager" aria-label="Previous and next">` +
    (prev ? `<a class="prev" href="${BASE}${pageUrl(prev.slug)}"><span>Previous</span>${esc(prev.title)}</a>` : "<span></span>") +
    (next ? `<a class="next" href="${BASE}${pageUrl(next.slug)}"><span>Next</span>${esc(next.title)}</a>` : "<span></span>") +
    `</nav>`;
  const sources = [...new Set(page.parts.map((p) => p.repo || p.sdk || (p.release ? RELEASE_FILE : null)).filter(Boolean))];
  const source = sources.length
    ? `<p class="source">From the repository: ${sources
        .map((s) => `<a href="${GITHUB}/blob/main/${s}"><code>${esc(s)}</code></a>`)
        .join(", ")}</p>`
    : "";
  write(
    join(pageUrl(page.slug), "index.html"),
    fill(layout, {
      base: BASE,
      title: esc(page.title === "Overview" ? "Documentation" : page.title),
      group: esc(page.group),
      h1: esc(page.title),
      sidebar: sidebar(page.slug),
      toc: tocHtml(toc),
      content: html,
      pager,
      source,
      version: VERSION,
      github: GITHUB,
    }),
  );
});
write("docs/search-index.json", JSON.stringify(index));

// The landing page's lists of the release, taken from its notes: each bullet's
// bold lead and first sentence, as the notes write them.
function releaseLists() {
  const { sections } = cut(read(join(REPO, RELEASE_FILE)));
  const bullets = (prefix) => {
    const key = [...sections.keys()].find((k) => k.startsWith(prefix));
    if (!key) return [];
    return sections
      .get(key)
      .filter((l) => l.startsWith("- "))
      .map((l) => {
        const text = l.slice(2);
        const lead = /^(\*\*[^*]+\*\*)\s*(.*)$/.exec(text);
        const first = (s) => (/^(.*?[.!?])(\s|$)/.exec(s) || [s, s])[1];
        // A lead that ends its sentence ("**Installing.** ...") keeps the sentence after it.
        const md = lead ? (/[.!?]\*\*$/.test(lead[1]) ? `${lead[1]} ${first(lead[2])}` : first(text)) : first(text);
        return `<li>${new Marked().parseInline(md)}</li>`;
      });
  };
  const col = (title, cls, items) =>
    items.length ? `      <div class="col">\n        <h3>${title}</h3>\n        <ul class="${cls}">\n${items.join("\n")}\n        </ul>\n      </div>` : "";
  const newer = [...sections.keys()].find((k) => k.startsWith("## What is new"));
  return (
    `    <div class="cols ${newer ? "three" : "two"}">\n` +
    (newer ? col(esc(newer.slice(3).replace(/^What is new/, "New")), "ticks", bullets("## What is new")) : "") +
    col("Works", "ticks", bullets("## What works")) +
    col("Not yet", "dashes", bullets("## What is missing")) +
    `\n    </div>\n    <p class="more"><a href="${BASE}docs/releases/">Every line of the release notes →</a></p>`
  );
}

// The landing page, and the videos that are there at build time.
function media(name, template) {
  if (!existsSync(join(SITE, `media/${name}.mp4`))) return "";
  mkdirSync(join(DIST, "media"), { recursive: true });
  copyFileSync(join(SITE, `media/${name}.mp4`), join(DIST, `media/${name}.mp4`));
  let poster = "";
  if (existsSync(join(SITE, `media/${name}-poster.jpg`))) {
    copyFileSync(join(SITE, `media/${name}-poster.jpg`), join(DIST, `media/${name}-poster.jpg`));
    poster = ` poster="${BASE}media/${name}-poster.jpg"`;
  }
  return read(join(SITE, template)).replace("{{poster}}", poster);
}
const launch = media("launch", "src/launch.html");
const demo = media("demo", "src/video.html");
let landing = read(join(SITE, "src/landing.html"));
landing = landing
  .replace("{{launch}}", launch)
  .replace("{{video}}", demo)
  .replace("{{videolink}}", demo ? read(join(SITE, "src/videolink.html")) : "")
  .replace("{{releaselists}}", releaseLists());
write("index.html", fill(landing, { base: BASE, version: VERSION, github: GITHUB }));
const hasVideo = Boolean(demo);

// Static assets.
mkdirSync(join(DIST, "assets"), { recursive: true });
for (const f of readdirSync(join(SITE, "assets"))) copyFileSync(join(SITE, "assets", f), join(DIST, "assets", f));
write("404.html", fill(read(join(SITE, "src/404.html")), { base: BASE }));
write(".nojekyll", "");

console.log(
  `Built ${pages.length} documentation pages and the landing page into dist/ (base path ${BASE}, release v${VERSION}, ${
    launch ? "with" : "without"
  } the launch video, ${hasVideo ? "with" : "without"} the demo video, ${index.length} search entries).`,
);
