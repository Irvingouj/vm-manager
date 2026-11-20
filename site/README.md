# The website and the documentation site

`site/` builds two things into `site/dist/`, as static files any static host can serve: the landing page of Vibe Manager (`/`) and the documentation beside it (`/docs/`). It has its own `package.json` and lock file and is **not** part of the repository's root npm workspace. It needs Node.js 22 or later; its one dependency is `marked`, the Markdown parser.

## Build

```sh
cd site
npm ci
npm run build                          # the base path /
BASE_PATH=/vm-manager/ npm run build   # the base path /vm-manager/ (or: npm run build -- --base /vm-manager/)
npm run check                          # every internal link and asset resolves; nothing from another origin
```

`npm run check` takes the same `BASE_PATH` as the build it checks.

## Preview

```sh
npm run preview                                    # http://127.0.0.1:4321/
BASE_PATH=/vm-manager/ PORT=8000 npm run preview   # http://127.0.0.1:8000/vm-manager/
```

It serves `dist/` under the base path the build was made with, as a static host would.

## The base path

Every link and asset is written with the base path given at build time, `/` without one. GitHub Pages serves this repository at `https://irvingouj.github.io/vm-manager/`, so the base path there is `/vm-manager/`; on a domain of its own it is `/`.

## Where the content comes from

- The landing page is `src/landing.html`, written from `docs/vision.md` and `README.md`. Its lists of what is new, what works and what is missing are taken from the latest release notes at build time (each bullet's bold lead and first sentence).
- The documentation is read from the repository's own documents at build time, as `pages.mjs` lists them: `docs/getting-started.md`, `deploy/README.md` (cut into pages by its sections), the latest `docs/releases/v*.md`, `CONTEXT.md` and `sdk/index.d.ts`. Links between them are rewritten to the site's pages; a section that is renamed in a document makes the build fail and name it.
- `content/*.md` are the few words of the site's own that join those pages.
- `content/commands.md` is the help of every command of the tool, made by `scripts/capture-commands.sh <path to a built vibe>`. Run it again when a release changes the tool's commands.
- The search index (`dist/docs/search-index.json`) is built with the site and searched in the browser (`assets/search.js`); there is no search service.

## The videos

The videos are made by the repository's `media/` folder (ticket #74) and copied in before a build; they are not committed (`.gitignore`):

- `site/media/launch.mp4`, with its poster `site/media/launch-poster.jpg`: the short launch video, shown right after the hero;
- `site/media/demo.mp4`, with its poster `site/media/demo-poster.jpg`: the demo, in its own section "See it work", and linked from the footer.

Each is shown only when its `.mp4` is there at build time; otherwise the build leaves its section out. A poster is a still of 1920×1080, and is optional.

## Publishing

Publishing is not set up yet. The build's output, `dist/`, is plain static files: any static host can serve it, from the base path it was built with.
