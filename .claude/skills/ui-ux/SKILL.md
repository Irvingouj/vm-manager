---
name: ui-ux
description: UI/UX for Vibe Manager. Designs, restyles and reviews the platform's server-rendered pages (Dashboard, sharing forms, sign-in, sign-out, refusal and error pages, the local run's pages, the tool's browser pages) and the website, from one shared theme, using Refactoring UI tactics, Tailwind's scales and HCI principles with checkable numbers. Use when changing or reviewing anything a person sees in a browser: a page's HTML or CSS, the theme, colours, spacing, typography, forms, error messages, empty states, dark mode, the 320 px layout, accessibility, or when someone says the UI looks bad.
---

# UI/UX

A page is done when a person can tell where they are, what they can do and what just happened, at 320 px and on a wide screen, in light and dark, with a keyboard, and it looks like one product. Looks are checked by looking: screenshots are read, not assumed.

## The method, in order

1. **Look first.** Screenshot every page you will touch at 320, 768 and 1280 px, light and dark (`scripts/shots.sh`, or the black-box harness's browser for pages behind a sign-in), and read the pictures. Keep them: they are the "before".
2. **Evaluate.** Walk [CHECKLIST.md](CHECKLIST.md) page by page and write down each problem with the rule it breaks. Do a cognitive walkthrough of each task (share an Application, sign out, recover from a refusal): will the person know what to do, see the control, connect it to their goal, and see that it worked?
3. **Tokens before pages.** All values come from the one theme ([THEME.md](THEME.md)): spacing, type sizes with their line heights, two weights, the neutral and accent shades, radii, shadows, focus ring. No ad-hoc pixel value, no colour outside the theme. If a value is missing, add it to the theme, not to a page.
4. **Structure in greyscale.** Get hierarchy right with spacing, weight and the neutral shades. Colour comes last and is spent on the one primary action, links, and status.
5. **Components, then pages.** Page shell, header, card, buttons (primary, secondary, quiet, destructive), fields with labels above them, notices, lists, identifiers, empty states. Each page is built from these and nothing else.
6. **Verify by looking and measuring.** New screenshots at the three widths, light and dark, read and compared with the "before"; every text and control pair through `scripts/contrast.py`; a keyboard-only walk (visible focus everywhere, order follows the page); 320 px with no sideways scroll; text at 200 %; `prefers-reduced-motion`. Then the tests that hold the pages.
7. **Report** what changed, with the before and after pictures' paths, the contrast table, and what was not checked.

## Rules that do not bend

- **One primary action per view**, styled as such; everything else is secondary or quiet. A destructive action is not automatically loud: it is loud only where it is the primary action.
- **Hierarchy by weight and colour before size.** De-emphasise what is secondary (identifiers, timestamps, labels) instead of enlarging what matters. Two weights, three text colours.
- **Space groups things.** The gap between groups is clearly larger than the gap inside one. Prefer spacing, a background or a shadow to a border.
- **Text**: body 16 px or more, line length 45 to 75 characters (`max-width` in `ch` or `rem`), line height from the scale, numbers and identifiers in tabular or monospace figures and never louder than names.
- **Contrast** (WCAG 2.2 AA): text 4.5:1, large text and the edges of controls and focus rings 3:1, in both schemes. Measured, not guessed.
- **Targets** at least 24 by 24 CSS px, 44 px tall for the main buttons and fields; never two closer than 8 px.
- **Focus is always visible**: an `outline` (it survives forced colours), offset from the control, 3:1 against what is around it. Never `outline: none` without a replacement.
- **Never colour alone**: a status has words, and an icon or a shape, as well as its colour. Links inside text are underlined.
- **Forms**: a visible label above each field (a placeholder is not a label), help text under the label, the error next to its field in words that say what happened and what to do, the button named by its verb and object ("Share hello"), nothing disabled without saying why.
- **Messages** say what happened, why, and what the person can do next, in the vocabulary of `CONTEXT.md` (User, Application, Contributor, Group, Dashboard, Version, Platform administrator). No blame, no jargon, no bare codes.
- **Empty states** say what would be here and how to get the first one.
- **Motion** is optional decoration: under 200 ms, none under `prefers-reduced-motion`, and nothing moves on its own.

## What this product's pages may and may not do

- **No external anything.** The pages' `content-security-policy` is `default-src 'none'` with inline `<style>` allowed: no web font, no image file, no `url(...)` in CSS, no script. Use the system font stack, CSS gradients, borders, shadows and inline `<svg>`. Changing a policy is a contract change (`blackbox/CONTRACT.md`), not a styling change.
- **One theme, written once.** The CSS of the platform's pages comes from one module of the program; a page family does not carry its own copy. The website (`site/assets/style.css`) uses the same identity (THEME.md) so the site, the documentation and the product read as one thing.
- **The black-box tests hold the words and the structure.** `blackbox/tests/dashboard_*.rs`, `browser_*.rs`, `oidc_*.rs`, `local_dev*.rs` assert on text, on `data-vm-*` attributes, on forms and their fields, and that no page is wider than 320 CSS px. A restyle keeps them green without editing them. A change of wording or structure goes through the contract and its tests first, as `AGENTS.md` says.
- **Everything that is not the platform's own text is escaped**, and long names, addresses and identifiers must wrap (`overflow-wrap: anywhere`) instead of widening the page.
- **Rust rules still apply** to the code that writes the pages (`AGENTS.md`): no panic, no `unwrap`, minimal `clone`.

## Where the reasons are

- `docs/research/refactoring-ui-and-tailwind-defaults.md`: Adam Wathan and Steve Schoger's tactics and Tailwind's exact scales, each claim marked by how it is known.
- `docs/research/hci-principles-for-the-platforms-pages.md`: Nielsen's heuristics, Norman's principles, Fitts, Hick and the other laws, Gestalt grouping, WCAG 2.2 criteria with numbers, forms and error research.
- [CHECKLIST.md](CHECKLIST.md): the review questions from both, in the order to ask them.
- [THEME.md](THEME.md): the identity and the tokens, with the measured contrast of each pair.

## Scripts

- `scripts/contrast.py '#fg' '#bg' …` or `--tokens file`: WCAG contrast ratios; exits 1 when a listed pair is under its minimum.
- `scripts/shots.sh <name> <url>…`: Firefox screenshots at 320, 768 and 1280 px, light and dark, under `~/snap/firefox/common/ui-shots/`.
