# The theme

One identity for the website, the documentation and the product's pages: **a quiet tool on warm paper**. Warm neutrals instead of blue-grey, one vermilion accent spent sparingly, a serif only for the page's title, a plain system sans for everything a person works with. It should feel like a well-set document, not a control panel.

The values below are the source. A page that needs a value not listed adds it here first. The pairs' contrast is measured with `scripts/contrast.py --tokens tokens-light.txt` and `tokens-dark.txt` (kept beside this file; both must exit 0 after any change of colour).

## Colour

| Token | Light | Dark | Used for |
|---|---|---|---|
| `--bg` | `#f6f3ec` | `#12110e` | the page |
| `--surface` | `#fffdf9` | `#1c1a16` | cards, fields, the header when it sits on the page |
| `--surface-2` | `#efeadf` | `#27251f` | insets: identifiers, inline code, table heads, quiet buttons' hover |
| `--ink` | `#16140f` | `#ece7dc` | text, names, headings |
| `--ink-2` | `#57534e` | `#b5ad9c` | secondary text: descriptions, labels of data, help text |
| `--ink-3` | `#6b655c` | `#9a9282` | tertiary: identifiers, timestamps, counts |
| `--line` | `#d6cfbf` | `#34312a` | hairlines that only decorate (not the edge of a control) |
| `--edge` | `#8a8377` | `#7d766a` | the edge of a field or a secondary button (3:1 against the page) |
| `--accent` | `#c2410c` | `#ff7a3d` | the primary button, links, the focus ring, the current item |
| `--accent-strong` | `#9a3412` | `#ff9a66` | the primary button pressed or hovered |
| `--on-accent` | `#ffffff` | `#16140f` | text on the accent |
| `--accent-soft` / `--accent-soft-ink` | `#ffedd5` / `#9a3412` | `#3a1d0e` / `#ffb38a` | a notice that points at something to do |
| `--danger` | `#b91c1c` | `#fca5a5` | error text, a destructive action's text |
| `--danger-soft` / `--danger-soft-ink` | `#fee2e2` / `#991b1b` | `#3b1212` / `#fecaca` | an error notice |
| `--ok-soft` / `--ok-soft-ink` | `#dcfce7` / `#166534` | `#0f2a1a` / `#bbf7d0` | a success notice |
| `--warn-soft` / `--warn-soft-ink` | `#fef9c3` / `#854d0e` | `#2e2607` / `#fef08a` | a warning notice |

Measured (light / dark): `--ink` on `--bg` 16.6 / 15.3; `--ink-2` on `--bg` 6.9 / 8.5; `--ink-3` on `--bg` 5.2 / 6.1; `--accent` on `--bg` 4.7 / 7.3 and on `--surface` 5.1 / 6.7; `--on-accent` on `--accent` 5.2 / 7.1; `--edge` on `--bg` 3.4 / 4.2. Every pair in the two token files is at or above its minimum.

Rules of use: dark is not the light theme inverted: surfaces get lighter as they come forward, shadows almost disappear and a hairline takes their place. Never `--ink-3` for anything a person must read to act. Status colours always come with words and a mark (an inline `<svg>` or a glyph), never alone. `:root{color-scheme:light dark}` so the browser's own controls and scrollbars follow.

## Type

- Sans (everything): `system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif`.
- Serif (the page's `h1` only): `"Iowan Old Style", "Palatino Linotype", Palatino, "Book Antiqua", Georgia, serif`.
- Mono (identifiers, commands): `ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace`.
- Two weights: 400 and 600. The serif title is 600 with `letter-spacing: -0.01em`.

| Step | Size / line height | Used for |
|---|---|---|
| `--text-xs` | 0.75rem / 1rem | overlines (uppercase, `letter-spacing: 0.06em`, 600, `--ink-3`), counts |
| `--text-sm` | 0.875rem / 1.25rem | help text, identifiers, table cells, secondary buttons |
| `--text-base` | 1rem / 1.5rem | body, fields, primary buttons |
| `--text-lg` | 1.125rem / 1.75rem | a card's title (an Application's name) |
| `--text-xl` | 1.25rem / 1.75rem | section headings |
| `--text-2xl` | 1.5rem / 2rem | the page title at 320 px |
| `--text-3xl` | 1.875rem / 2.25rem | the page title from 40rem up |

Text blocks are at most `65ch` wide. Numbers that are compared sit in a column with `font-variant-numeric: tabular-nums`.

## Space, size, shape, depth

- Spacing scale (rem): 0.125, 0.25, 0.375, 0.5, 0.75, 1, 1.25, 1.5, 2, 2.5, 3, 4, 6. Inside a group: 0.25 to 0.75. Between fields: 1.25. Between cards: 1. Between sections: 2.5 to 3. Page padding: 1 at 320 px, 1.5 from 40rem, 2 from 64rem.
- Widths: the Dashboard's column `max-width: 56rem`; a page that asks one thing (sign-in, a refusal, a confirmation, the test-User page) is a single card of `max-width: 30rem`, centred, a third of the way down, not a sheet of text in the corner.
- Controls: primary buttons and fields at least 2.75rem tall (44 px); secondary and quiet ones at least 2.25rem; 0.5rem or more between any two.
- Radii: 0.375rem controls, 0.625rem cards, 999px pills (marks, badges). One radius per kind, everywhere.
- Shadows (light): `--shadow-1: 0 1px 2px rgb(22 20 15 / 0.06), 0 1px 1px rgb(22 20 15 / 0.04)` for cards; `--shadow-2: 0 8px 24px -6px rgb(22 20 15 / 0.14), 0 2px 6px -2px rgb(22 20 15 / 0.08)` for the single card of a one-thing page. In dark: no shadow, `1px solid var(--line)` instead.
- Focus: `outline: 2px solid var(--accent); outline-offset: 2px` on `:focus-visible`, on every interactive thing, both schemes.
- Motion: colour and shadow transitions of 120ms on buttons and links only; none under `prefers-reduced-motion: reduce`.

## Components

- **Shell.** A slim header on `--bg` with a hairline under it: the product's mark (the small vermilion triangle of the website, as inline `<svg>`) and name on the left, the person on the right as name (600) with the identifier beneath or beside it in `--ink-3` mono `--text-sm`, then "Sign out" as a quiet button. The header never grows taller than it must at 320 px: the identifier wraps or is cut with the full value in `title`.
- **Card.** `--surface`, radius 0.625rem, `--shadow-1` (a hairline in dark), padding 1.25rem (1rem at 320 px). Title in `--text-lg` 600, description in `--ink-2`, then one row of actions: the primary on the left, quiet links after it. Data about the card (Version, counts) as small `--ink-3` text or pills, not as sentences.
- **Buttons.** Primary: `--accent` fill, `--on-accent` text, 600. Secondary: `--surface` fill, `--edge` border, `--ink` text. Quiet: no fill or border, `--accent` text, underlined on hover, `--surface-2` on hover when it is a button. Destructive: quiet with `--danger` text ("Remove"), a `--danger` fill only on a page whose one job is to confirm it. A link that goes somewhere is an `<a>`; a thing that changes something is a `<button>` in a `<form>`.
- **Fields.** Label above (600, `--text-sm`), help under the label in `--ink-2`, the control full width up to 24rem with an `--edge` border on `--surface`, the error under the control in `--danger` with a mark, and the field's border in `--danger` too.
- **Notices.** A soft fill of its kind, its ink, radius 0.375rem, a mark at the left, the words: what happened, then what to do. No left-border-only stripes as the only signal.
- **Lists of people or things.** Rows with the name (600) and the identifier beneath or after it (mono, `--text-sm`, `--ink-3`), the row's action at the right end as a quiet or destructive button; hairlines between rows, none around the list.
- **Identifiers.** Always mono, small, `--ink-3`, never before the name, wrapped with `overflow-wrap: anywhere`.
- **Empty states.** Inside the place where the things would be: one line saying what would be here, one saying how to get the first, and the command or action that does it.
- **One-thing pages.** Title (serif), one or two sentences, the one action, and a quiet way out ("Back to the Dashboard", "Sign out"). An error or refusal says who you are signed in as only if that explains it.
