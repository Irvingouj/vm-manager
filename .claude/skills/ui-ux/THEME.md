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
| `--on-danger` | `#fff` | `#16140f` | text on a `--danger` fill (the one button of a page that confirms a destructive action) |
| `--danger-soft` / `--danger-soft-ink` | `#fee2e2` / `#991b1b` | `#3b1212` / `#fecaca` | an error notice |
| `--ok-soft` / `--ok-soft-ink` | `#dcfce7` / `#166534` | `#0f2a1a` / `#bbf7d0` | a success notice |
| `--warn-soft` / `--warn-soft-ink` | `#fef9c3` / `#854d0e` | `#2e2607` / `#fef08a` | a warning notice |

Measured (light / dark): `--ink` on `--bg` 16.6 / 15.3; `--ink-2` on `--bg` 6.9 / 8.5; `--ink-3` on `--bg` 5.2 / 6.1; `--accent` on `--bg` 4.7 / 7.3 and on `--surface` 5.1 / 6.7; `--on-accent` on `--accent` 5.2 / 7.1; `--edge` on `--bg` 3.4 / 4.2. Every pair in the two token files is at or above its minimum.

Marks of a state take the ink of their kind on the page or a card: `--ok-soft-ink`, `--warn-soft-ink`, `--danger`, `--ink-2`, `--ink-3`, each at least 3:1 (measured in the token files: `*-mark-on-*`).

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

- **Shell.** A slim header on `--bg` with a hairline under it: the product's mark (the small vermilion triangle of the website, as inline `<svg>`) and name on the left, then the place after a hairline: an overline on the Dashboard, the local run and the tool's own page ("Dashboard", "Local run", "The tool"), and on the sign-in address's pages the installation's domain in mono `--text-sm` `--ink-3`, never upper case (one word of a place would be wrong on most of them; which installation it is is right on all). The whole is a link home on the Dashboard only, since the other pages promise their links. The person on the right as name (600) with the identifier beneath it in `--ink-3` mono `--text-xs` (with no name the identifier is the name: `code.id.as-name`, 600 in `--ink`, still mono), then "Sign out" as a quiet button. The header never grows taller than it must at 320 px: the identifier wraps.
- **Card.** `--surface`, radius 0.625rem, `--shadow-1` (a hairline in dark), padding 1rem (1.25rem from 40rem). Title in `--text-lg` 600, description in `--ink-2`. Data about the card as a pill with its mark, not as a sentence.
- **A card of the Dashboard's list** holds what is needed to choose and open an Application and nothing more (the product owner's rule 3): its name in `--accent` 600 (the link to its own page, underlined on hover), beside the name at most one pill of its state (a dashed circle and "Not deployed"; a cross on `--danger-soft` and "Missing access" when a Resource it uses will refuse the person; a triangle on `--warn-soft` and "Not set up" when one is not registered; three dots and "Access unknown"), the description under the name, and "Open" with the mark of an arrow leaving a box as the outlined button at the right end of the card (under the text at 320 px), the Application's name hidden beside the word for a reader who hears it. Contributors, the address, the Version, the Resources and whom to ask live on the Application's own page.
- **Buttons.** Primary: `--accent` fill, `--on-accent` text, 600. Secondary: `--surface` fill, `--edge` border, `--ink` text. Quiet: no fill or border, `--accent` text, underlined on hover, `--surface-2` on hover when it is a button. Destructive: quiet with `--danger` text ("Remove"), a `--danger` fill only on a page whose one job is to confirm it. A link that goes somewhere is an `<a>`; a thing that changes something is a `<button>` in a `<form>`.
- **Fields.** Label above (600, `--text-base`: what a person reads to act is 16 px or more, CONTRACT.md "How the pages look"), help under the label in `--ink-2`, the control full width up to 24rem with an `--edge` border on `--surface` (a `<select>` has its own arrow turned off and an inline `<svg>` chevron over it, never a gradient or a picture), the error under the control in `--danger` with a mark, and the field's border in `--danger` too.
- **Notices.** A soft fill of its kind, its ink, radius 0.375rem, a mark at the left, the words: what happened, then what to do. No left-border-only stripes as the only signal.
- **Lists of people or things.** Rows with the mark of the kind at the left in `--ink-3` (one person, a Group of two people, everyone as a globe; its name hidden beside it for a reader who hears it), the name (600), the email and the identifier beneath it (`--text-sm`, the identifier mono `--ink-3`), the row's action at the right end as a quiet or destructive button, beside the text when both fit, else on its own line under it (from 40rem up the text keeps at least 16rem beside the action and wraps; under 40rem the action stays beside only text that fits on its lines unwrapped), the text wrapping anywhere so nothing ever lies under the button. A person known only by an identifier has it as their name (`code.id.as-name`: the name's weight and colour, mono); the identifier is small and soft only where a name stands before it. Hairlines between rows, none around the list. A list in two parts (the Contributors, then whom it is shared with) has an overline over each part instead of a sentence joining them.
- **A table of standings** (the Resources an Application uses): an overline head (Resource, Needs, You hold), one row each with a hairline above it; the first cell the mark of the standing in its colour (a check in `--ok-soft-ink`, a triangle in `--warn-soft-ink`, a cross in `--danger`, a question mark in `--warn-soft-ink` for one not registered, three dots in `--ink-3`), its words hidden beside it, and the name (mono 600), with whom to ask under it in `--ink-2`; then the two levels. Under 40rem the head is hidden from sight and each level carries its label before it (`data-label`).
- **Identifiers.** Always mono, small, `--ink-3`, never before the name, wrapped with `overflow-wrap: anywhere`. Where the identifier is all that is known of a person it is their name instead: mono, at the name's size, weight and `--ink` (`code.id.as-name`), so a person with no name never looks lesser than a Group.
- **A choice among cards** (the test Users): one button on every card, the chosen card included since the contract promises one for each; the chosen card is said once, by a "Chosen" pill with a check mark beside the name, and its button says what it does there ("Go on as this User"), not the choosing words of the others.
- **Empty states.** Inside the place where the things would be: one line saying what would be here, one saying how to get the first, and the command or action that does it.
- **One-thing pages.** Title (serif), one or two sentences, the one action, and a quiet way out ("Your Applications" after a back arrow, "Sign out"). The title of a state carries its mark before it, 1.75rem (2rem from 40rem) in the state's colour: a check in `--ok-soft-ink` (done), a cross in a circle in `--danger` (refused, failed), a clock in `--warn-soft-ink` (later), a lock or a question mark in `--ink-2` (not yours, not here). The sentence under it says what the title does not: the name it is about, the reason, the next step; never the title again. An error or refusal says who you are signed in as only if that explains it, as a row of its own at the foot of the card (a person's mark, the name, the identifier, and "Sign out" at the right). What only some want to know (why a sign-in may be over) is behind a `<details>`.
- **Marks.** Inline `<svg>` drawn on a 20 by 20 grid with a 1.5 stroke in the current colour, `aria-hidden`, 1.25rem (1rem in small text and pills): an arrow leaving a box (open), an arrow to the left (back), a person, two people (a Group), a globe (everyone), a lock, a dashed circle (nothing there yet), a check, a triangle, a cross, a question mark, three dots, a clock, a terminal window (the tool), a door with an arrow (sign out). A mark that carries meaning always has words for a reader who hears it, visible beside it or hidden (`.vh`); a colour always rides with a mark. Only marks people already know.
- **The pages of Platform administrators** (`/admin`). The way in is one quiet link in the Dashboard's header, before the person: a shield and "Administration" (the word hidden from sight under 40rem, kept for a reader who hears it). Under the header each of their pages has one `nav.sections`: five links, each a mark and a word (a window: Applications; lines of a list: Operation log; a cylinder: Resources; a circle with an arrow leaving it: Outbound; two stacked boxes: Installation), the page's own with `aria-current="page"`, `--ink` and a 2 px `--accent` underline, the rest `--ink-2`, 8 px apart. Under 40rem the five stay on one row: the page's own keeps its word in `--text-sm`, the others show their mark alone (the word hidden from sight, kept as the link's name), each at least 28 px wide.
- **Usage** is two numbers in tabular figures (600, `--ink`), each after its mark in `--ink-3` (a pulse: requests; a person: Users); the period ("in the last 30 days") is said once, in a legend over a list or in the line itself on one Application's page.
- **The Operation log.** The filters are one card of fields in a grid (`minmax(min(18rem, 100%), 1fr)`: two columns at most), the times `datetime-local` held to the second (the fraction cut, so that the field's segments fit it at 320 px) and styled as text fields with 4 px of padding at each side (the browser's field draws its own segments and calendar mark); under 40rem their letter spacing is `-0.03em`, so that a 12-hour time's "AM" or "PM" keeps a gap of some pixels before the calendar mark at 320 px, and the form loses its card (no padding, fill or edge) so that the fields take the column's whole width, the error of a field under it in `--danger` with a triangle (`aria-describedby` from the field). Each entry is a row with a hairline under it: the outcome as a named mark (`role="img"`, `aria-label` the outcome's word; a check in `--ok-soft-ink`, a cross in `--danger`, a triangle in `--warn-soft-ink`, three dots in `--ink-3`), the time to the second in tabular figures (UTC said once, in the overline over the rows), then the User, the Application and Version, the Resource (each a mark and a link that narrows to it, 24 px high), the operation in mono, and a refusal's reason or a failure's code in mono in the outcome's colour; one step away in a `<details>`: the statement in a `pre.statement` (`--surface-2`, broken where the line ends) and the facts.
- **Disabling.** On an Application's page a Platform administrator has a strip under its actions: its usage, a quiet link to its Operation log, and "Disable" (a circle with a bar, quiet, `--danger` text) leading to the page that confirms it; when it is disabled, the primary "Enable <name>" (a triangle pointing right) in its place. The page that confirms it is a one-thing page: the title "Disable <name>?" after the same mark in `--warn-soft-ink`, what it does in a few words, its usage, the one `--danger` fill button (`--on-danger` text) and the quiet "Keep it serving" beside it.
- **A Resource** is a card: the cylinder and its name (mono 600), its description in `--ink-2`, its Grants as a row of a Group's mark, the Group (600) and the level in a small pill (`.lv-pill`, never broken); whose it is, its kind, who registered it and the link to its Operation log behind "Details".
