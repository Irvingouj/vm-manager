# Refactoring UI and Tailwind CSS's defaults: practical UI guidance for restyling our pages

Read on 2026-11-20. For an engineer (human or agent) restyling the program's server-rendered pages (Dashboard, sharing forms, sign-in and sign-out, error and refusal pages, 320 px wide, light and dark) with hand-written CSS custom properties, no framework, no external assets.

## Summary

Adam Wathan and Steve Schoger teach design as a set of tactics, not talent. Their main points are these. Build the feature before the frame around it. Make hierarchy with weight and colour before size, and by de-emphasising what is secondary. Give each view one clear primary action. Take every spacing, size, colour shade and shadow from a small scale fixed up front, never from ad-hoc values. Use fewer borders: spacing, a shadow or a different background separate things just as well. Tint greys towards the brand hue. Offset shadows vertically, as if lit from above. Tailwind CSS (by Wathan's company) encodes the same system as concrete numbers: a 0.25 rem spacing unit, a 13-step type scale with a line height paired to each size, nine font weights, eight radii, seven box shadows, and 26 colour scales of 11 shades (50–950) in OKLCH. Its dark mode follows `prefers-color-scheme` by default. Tailwind is MIT-licensed, so its values can be copied into our own CSS. The concrete tokens are in section 8 and the review checklist is in section 9. The book itself is paid. Only its table of contents and one chapter adapted as a free article were opened, so its chapter bodies are not verified. Each principle below is attributed to the free source where it was actually read.

**How claims are marked.** **[V]** = verified: I opened the primary source and read it there. **[R]** = read from source: a secondary source reports it. **[NV]** = not verified: from memory or inference, not checked this session. **[Rec]** = this document's own recommendation for our pages, not a claim about the sources. Source numbers like (S4) refer to the Sources list.

---

## 1. Process

- **Start with a feature, not a layout.** This is the book's first chapter title, "Start with a feature, not a layout" **[V] (S2)**. What it means (design the actual thing the user does, such as a form or a list, before the nav bar, sidebar and page shell) is my reading of the title. The chapter body was not read **[NV]**.
- **Detail comes later; don't design too much.** These are chapter titles **[V] (S2)**. Their content (work in low fidelity and leave colours, shadows and icons until the structure works) is **[NV]**.
- **Design in greyscale first.** This is widely attributed to the book's "Detail comes later" chapter (hierarchy must work through spacing, contrast and size before colour is added). **[NV]**. It is not in any source I opened.
- **Choose a personality** (typeface, colour, border radius, the tone of the words) and **limit your choices** (pick from fixed scales). Both are chapter titles **[V] (S2)**. Steve Schoger's 2018 Laracon Online deck says to use one consistent corner radius throughout and shows the change from a hard edge to `border-radius: 4px` **[V] (S6)**. The link between small or large radii and a serious or playful personality is **[NV]**.
- **Work by small cosmetic changes, one at a time.** Both the Laravel.io case study (S5) and the Laracon deck (S6) improve an existing UI step by step: shrink a title, move a field, lighten secondary text, re-check. Neither starts the design from scratch **[V] (S5, S6)**.
- **[Rec]** For us: the personality is "quiet internal tool". That means one neutral scale, one primary hue, a small radius (`0.375rem`–`0.5rem`), the system font stack and plain, short wording.

## 2. Hierarchy

- **Size isn't everything; use weight and colour.** Leaning only on font size for hierarchy is a common mistake. Make important text bolder and secondary text a lighter colour **[V] (S4 tip 1)**. Two or three text colours are usually enough, for example dark for primary, mid-grey for secondary and lighter grey for tertiary **[V] (S4)**. Two font weights are usually enough for UI work. Avoid weights under 400 for UI text. To de-emphasise, use a lighter colour or a smaller size, not a thinner weight **[V] (S4)**.
- **De-emphasise to emphasise.** Instead of making the active or important item stronger, make the things around it weaker. In the Laravel.io nav, the inactive items were given a lighter colour rather than recolouring the active one **[V] (S5)**. "De-emphasize to emphasize" is also a book chapter **[V] (S2)**.
- **Don't use grey text on coloured backgrounds.** Grey on white works because it lowers contrast. On a coloured background, lower the contrast with white at reduced opacity, or hand-pick a colour of the background's hue with adjusted saturation and lightness **[V] (S4 tip 2; S6 "Saturate greys when using a colored background")**.
- **Labels are a last resort.** This is a chapter title **[V] (S2)**. Notes from Schoger's CSS Day 2019 talk say most data needs no "Label: value" form. Where a label helps, make it secondary: small, bold or uppercase, in a softer colour **[R] (S7)**. In the Laravel.io sidebar, a "Tags" heading went from 26 px to 12 px, bold, uppercase, letter-spaced and softer in colour, so it labels the content rather than competing with it **[V] (S5)**.
- **Separate visual hierarchy from document hierarchy.** This is a chapter title **[V] (S2)**. In application UIs, choose a heading's size by what matters on the page, not by its tag. A page title can be an `h1` and still be small. The Laravel.io "Forum" title went from 39 px to 24 px, and the case study shows a Baremetrics dashboard whose page title is 16 px next to 42 px figures **[V] (S5)**.
- **Primary, secondary and tertiary actions.** Most pages have one true primary action, a few secondary ones and some rarely used tertiary ones, and their styling should show that order. Not every button needs a background colour **[V] (S4 tip 7)**. The Laracon deck says to give actions a visual hierarchy with one clear primary **[V] (S6)**.
- **Semantics are secondary.** A destructive action is not automatically big and red. Give it the secondary or tertiary treatment unless it *is* the primary action, as in a confirmation dialog **[V] (S4 tip 7; chapter title in S2)**.
- **Balance weight and contrast.** This is a chapter title **[V] (S2)**. The deck's advice to "lighten icons so they don't compete with content" shows the idea: a heavy element such as a solid icon or a bold word gets a softer colour so it does not outweigh the text beside it **[V] (S6)**. The converse is **[NV]**: a light element such as a 1 px border can be made darker or thicker to hold its own.

## 3. Layout and spacing

- **Start with too much white space,** then remove it until the layout feels right **[V] chapter title (S2); [R] the "then remove" method (S7)**. The same notes give 40–48 px as a comfortable input height **[R] (S7)**.
- **Establish a spacing and sizing system.** Use a small fixed scale, not arbitrary pixel values **[V] chapter title (S2)**. In the Laracon deck, paddings of 15, 24 and 28 px are replaced by values taken from one scale **[V] (S6)**.
- **Tailwind's concrete scale.** In v4 every spacing utility is `calc(var(--spacing) * n)` with `--spacing: 0.25rem`, and it accepts any integer n **[V] (S9, S17, S18)**. The curated scale v3 shipped, which is the one to copy, was **[V] (S11)**:

  | step | rem | px | step | rem | px |
  |---|---|---|---|---|---|
  | px | — | 1 | 10 | 2.5 | 40 |
  | 0.5 | 0.125 | 2 | 11 | 2.75 | 44 |
  | 1 | 0.25 | 4 | 12 | 3 | 48 |
  | 1.5 | 0.375 | 6 | 14 | 3.5 | 56 |
  | 2 | 0.5 | 8 | 16 | 4 | 64 |
  | 2.5 | 0.625 | 10 | 20 | 5 | 80 |
  | 3 | 0.75 | 12 | 24 | 6 | 96 |
  | 3.5 | 0.875 | 14 | 28 | 7 | 112 |
  | 4 | 1 | 16 | 32 | 8 | 128 |
  | 5 | 1.25 | 20 | 36–64 | 9–16 (step 1) | 144–256 |
  | 6 | 1.5 | 24 | 72 | 18 | 288 |
  | 7 | 1.75 | 28 | 80 | 20 | 320 |
  | 8 | 2 | 32 | 96 | 24 | 384 |
  | 9 | 2.25 | 36 | | | |

  The px column assumes a 16 px root. Above 12 the steps grow coarser: 14, 16, 20, 24, then every 4 up to 64, then 72, 80 and 96. That is the "no two adjacent values closer than about 25 %" idea in practice **[NV]** as a statement of the book's rule.
- **You don't have to fill the whole screen.** This is a chapter title **[V] (S2)**. Give content a maximum width. Tailwind ships container widths from `--container-3xs: 16rem` to `--container-7xl: 80rem` **[V] (S9)**, and its deprecated `--max-width-prose` is `65ch` **[V] (S9)**. The Laravel.io-era deck "reduce width of panel" step does the same **[V] (S6)**.
- **Grids are overrated.** This is a chapter title **[V] (S2)**. Give elements such as a sidebar or a sign-in card a fixed or maximum width, and let only the rest flex. Avoid percentage columns for everything **[NV]** as the chapter's wording.
- **Relative sizing doesn't scale.** This is a chapter title **[V] (S2)**. Do not derive every size from one ratio, for example `em` padding that grows with font size, or headings shrunk by the same factor on small screens. Large elements usually shrink faster than small ones **[NV]**.
- **Avoid ambiguous spacing.** This is a chapter title **[V] (S2)**. The space between groups must be clearly larger than the space inside a group, so that what belongs together reads as together. In the Laravel.io case study, a search field placed just above "Create Thread" looked as if it belonged to that button, so it was moved to the title bar **[V] (S5)**.
- **[Rec]** For us at 320 px: page padding `1rem`; a form's label-to-input gap `0.375rem`–`0.5rem`, field-to-field gap `1.25rem`–`1.5rem`, section gap `2.5rem`–`3rem`; the sign-in, error and refusal cards `max-width: 28rem` (`--container-md`), the Dashboard `max-width: 64rem`.

## 4. Text

- **Establish a type scale** by hand, not by a strict modular ratio **[V] chapter title (S2)**. The Laracon deck contrasts a "classic typographic scale" with Tailwind's hand-picked sizes and says to use a consistent type scale **[V] (S6)**.
- **Tailwind v4's type scale with paired line heights** **[V] (S9, S10)**:

  | token | size | px | line height (as written) | resulting px |
  |---|---|---|---|---|
  | `--text-xs` | 0.75rem | 12 | `calc(1 / 0.75)` | 16 |
  | `--text-sm` | 0.875rem | 14 | `calc(1.25 / 0.875)` | 20 |
  | `--text-base` | 1rem | 16 | `calc(1.5 / 1)` | 24 |
  | `--text-lg` | 1.125rem | 18 | `calc(1.75 / 1.125)` | 28 |
  | `--text-xl` | 1.25rem | 20 | `calc(1.75 / 1.25)` | 28 |
  | `--text-2xl` | 1.5rem | 24 | `calc(2 / 1.5)` | 32 |
  | `--text-3xl` | 1.875rem | 30 | `calc(2.25 / 1.875)` | 36 |
  | `--text-4xl` | 2.25rem | 36 | `calc(2.5 / 2.25)` | 40 |
  | `--text-5xl` | 3rem | 48 | `1` | 48 |
  | `--text-6xl` | 3.75rem | 60 | `1` | 60 |
  | `--text-7xl` | 4.5rem | 72 | `1` | 72 |
  | `--text-8xl` | 6rem | 96 | `1` | 96 |
  | `--text-9xl` | 8rem | 128 | `1` | 128 |

  The resulting px column is my arithmetic. Note that the line height shrinks as the size grows, from 1.33 to 1.
- **Use good fonts.** This is a chapter title **[V] (S2)**. Schoger's notes call font choice the highest-impact change **[R] (S7)**. We may load no external assets, so use the system stack. Tailwind v4.3.5's `--font-sans` is `-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', 'Noto Sans', Arial, sans-serif, 'Apple Color Emoji', 'Segoe UI Emoji', 'Segoe UI Symbol', 'Noto Color Emoji'`, and `--font-mono` is `ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace` **[V] (S9)**. Earlier v4 releases began the sans stack with `ui-sans-serif, system-ui` **[NV]**.
- **Keep line length in check.** This is a chapter title **[V] (S2)**. The 45–75 characters per line figure is the common typographic rule the book is reported to give, as roughly 20–35 em **[NV]**. Tailwind's `65ch` prose width sits inside it **[V] (S9)**. Only running text needs this. Our pages are mostly short UI text, so it applies to the explanation on error and refusal pages.
- **Line height is proportional.** This is a chapter title **[V] (S2)**. Small text and long lines need taller line height, and large headings need line height near 1. Tailwind's paired values (above) encode the size half of this **[V] (S9)**. The width half (wider columns need more leading) is the book's reported point **[NV]**. Tailwind's named leadings are `tight 1.25`, `snug 1.375`, `normal 1.5`, `relaxed 1.625`, `loose 2` **[V] (S9)**. Preflight sets `line-height: 1.5` on `html` **[V] (S21)**.
- **Baseline, not centre.** This is a chapter title **[V] (S2)**. When text of different sizes sits on one line, such as a title and a count beside it, align it on the baseline (`align-items: baseline`), not on the vertical centre **[NV]** as the chapter's wording.
- **Letter-spacing.** When using all caps, add a little letter-spacing for legibility **[V] (S5)**. Tightening large headings is **[NV]** for the book. Tailwind's tracking tokens are `tighter -0.05em`, `tight -0.025em`, `normal 0em`, `wide 0.025em`, `wider 0.05em`, `widest 0.1em` **[V] (S9)**.
- **Not every link needs a colour.** Where nearly everything is a link, such as a list of threads or applications, use a bolder or darker version of the body text rather than the bright link colour. Keep the coloured link for links inside running text **[V] (S5; S6 "Easy on the link styles")**.
- **Align with readability in mind; align numbers right.** This is a chapter title **[V] (S2)**. In tables, right-align money and other figures so the digits line up, and use tabular figures (`font-variant-numeric: tabular-nums`) **[R] (S7)**. Do not centre long text; left-align it **[NV]**.
- **Font weights.** Tailwind's are `thin 100`, `extralight 200`, `light 300`, `normal 400`, `medium 500`, `semibold 600`, `bold 700`, `extrabold 800`, `black 900` **[V] (S9)**. Wathan and Schoger advise two weights for UI work and none under 400 for small text **[V] (S4)**. **[Rec]** Use 400 and 600, with 500 for button text if needed.

## 5. Colour

- **Ditch hex for HSL.** This is a chapter title **[V] (S2)**. The Laracon deck works in `hsl()` throughout, for example a grey `hsl(0, 0%, 28%)` turned into `hsl(194, 22%, 28%)` by giving it the brand hue **[V] (S6)**. Tailwind v4 has itself moved its palette from `rgb` to `oklch` for a wider gamut **[V] (S18)**. **[Rec]** Write our tokens in `oklch()` (Firefox 128+, which our gate requires, supports it) **[NV]** as to exact browser support. Copying Tailwind's values verbatim keeps them exact.
- **You need more colours than you think.** A real palette has greys (8–10 shades, since almost everything is grey), one or two primary colours (5–10 shades) and accent colours for states such as red, yellow and green, also in several shades. Up to about ten colours of 5–10 shades each is normal for a complex UI. True black looks unnatural, so start from a very dark grey **[V] (S3)**.
- **Define shades up front.** Do not make shades on the fly with `lighten()` or `darken()`. Fix the set up front. Pick the base (`500`, about what works as a button background), then the darkest (`900`, for text) and the lightest (`100`, a tinted background, for example an alert's), then fill in `700`/`300`, and then the rest. Trust your eyes over the maths, and avoid adding new shades later **[V] (S3)**.
- **Don't let lightness kill saturation.** This is a chapter title **[V] (S2)**. As a colour moves toward white or black it looks washed out, so raise the saturation at the extremes. Schoger's talk notes add that when lightening washes a colour out, you can rotate its hue toward the nearest bright hue instead **[R] (S7)**. Which hues count as bright (yellow, cyan, magenta) and which as dark (red, green, blue) is **[NV]**.
- **Greys don't have to be grey.** Add a little of a hue to greys. Use a blue tint for a cool feel and yellow or brown for a warm one, and saturate them more on coloured backgrounds **[V] (S6); [R] temperature detail (S7)**.
- **Accessible doesn't have to mean ugly.** This is a chapter title **[V] (S2)**. Flip the contrast: on a coloured background, use dark text tinted with that colour rather than white or grey **[R] (S7)**. Raise contrast by rotating the hue rather than only by darkening **[R] (S7)**. In the Laravel.io case study, a light green link failed a WCAG contrast check and was replaced by a darker green **[V] (S5)**. The thresholds are WCAG 2.2 1.4.3: text at least 4.5:1, and large text (18 pt, or 14 pt bold) at least 3:1 **[V] (S20)**. 1.4.11 requires at least 3:1 for UI components and graphics against adjacent colours **[V] (S20)**.
- **Don't rely on colour alone.** This is a chapter title **[V] (S2)**. It matches WCAG 1.4.1: colour must not be the only visual means of conveying information **[V] (S20)**. For us, a refusal or error needs a heading and words, perhaps an icon too, and not just a red box.

## 6. Depth

- **Emulate a light source.** This is a chapter title **[V] (S2)**. Light comes from above. Raised elements are lighter on top with a shadow below. Inset elements such as wells and inputs have the shadow on top **[V] (S4 tip 3, which covers inset shadows too)**.
- **Offset your shadows.** To make a shadow noticeable, add a vertical offset rather than a large blur or spread **[V] (S4 tip 3)**. The deck shows `0 0 4px` replaced by `0 2px 4px` **[V] (S6)**.
- **Use shadows to convey elevation, from a small fixed set.** This is a chapter title **[V] (S2)**. Schoger's talk notes speak of five elevation levels: small ones for buttons and inputs, the largest for modals **[R] (S7)**. Tailwind ships seven box shadows (section 8) **[V] (S9)**.
- **Shadows can have two parts.** This is a chapter title **[V] (S2)**. One shadow is large, soft and offset, like direct light. The other is tight and darker, the ambient shadow right under the element. The talk notes describe the large part as more vertically offset, with a negative spread to keep it subtle **[R] (S7)**. That the tight part should fade at higher elevations is **[NV]**. Every Tailwind shadow from `sm` to `xl` is a pair of this kind, with negative spread on both layers **[V] (S9)**.
- **Shadows for interaction.** Raise an element on hover or drag with a larger shadow, and lower it on press **[NV]** for the book. It follows from the elevation model.
- **Even flat designs can have depth.** This is a chapter title **[V] (S2)**. Without shadows, a lighter colour than the background reads as raised and a darker one as inset. A short solid shadow with no blur also works **[NV]**.
- **Overlap elements to create layers.** This is a chapter title **[V] (S2)**. Content is not used here; it is less relevant to our forms.

## 7. Images and finishing touches

- **Supercharge the defaults.** This is a chapter title **[V] (S2)**. Replace bullets with icons, style quotes and links, and give form controls such as checkboxes and radios a custom look in the brand colour **[NV]** as the chapter's list. Focus must stay visible (WCAG 2.4.7) **[V] (S20)**. Tailwind's preflight keeps Firefox's native focus ring (`:-moz-focusring` gets `outline: auto`) **[V] (S21)**. It sets placeholders to `currentcolor` at 50 % via `color-mix` **[V] (S21)**. Since v4, buttons keep the browser's default `cursor: default` **[V] (S14)**.
- **Add colour with accent borders.** A coloured strip along the side of an alert, under the active nav item, or across the top of the whole layout adds colour without any design talent **[V] (S4 tip 6)**. In Laravel.io, the active nav item gets a 2 px bottom border in the brand colour, anchored by a subtle border under the whole nav **[V] (S5)**.
- **Decorate your backgrounds.** This is a chapter title **[V] (S2)**. A subtle page background (slightly off-white) behind white sections helps sections stand out **[V] (S5 "Changing the site background color")**. Patterns and gradients are **[NV]** and need assets we would not ship anyway.
- **Don't overlook empty states.** This is a chapter title **[V] (S2)**. An empty list, such as a Dashboard with no applications yet, is the first thing a new user sees. Give it a clear message and the one action that fills it, and hide controls such as filters and sorting that are useless while it is empty **[NV]** as the chapter's wording.
- **Use fewer borders.** Separate things with a box shadow, two slightly different background colours, or simply more space. Too many borders make a design busy **[V] (S1, S2, S4 tip 4; S6 "Consider using space instead of borders")**. Cards that share one background can share borders and become one list **[V] (S5 "Collapsing borders")**.
- **Think outside the box.** This is a chapter title **[V] (S2)**. Components need not keep their default form. A table cell can hold two lines (name plus secondary detail) or a badge. A set of radio buttons can become selectable cards. A dropdown can have sections and icons **[NV]** as the chapter's examples.
- **Everything has an intended size; don't blow up small icons.** Icons drawn for 16–24 px look chunky at 3–4×. To fill a larger space, put a small icon inside a coloured shape **[V] (S4 tip 5)**. Lighten icons so they don't compete **[V] (S6)**.
- **Text over images, photo quality, user-uploaded content.** These are chapter titles **[V] (S2)**. They do not apply to us (no images).

## 8. Tailwind CSS defaults as a copyable reference

**Version read:** the docs site shows v4.3 (S10), and the source is `packages/tailwindcss/theme.css` at tag `v4.3.5`. That file is byte-identical to `main` as of 2026-11-20 (I diffed them) **[V] (S9)**. **Licence:** MIT, "Copyright (c) Tailwind Labs, Inc." **[V] (S19)**. Copying these values into our CSS is allowed; keeping the notice in a comment is a courtesy under MIT for substantial portions **[NV]** as legal advice.

```css
/* Values copied from Tailwind CSS v4.3.5 theme.css (MIT, Tailwind Labs, Inc.). */
:root {
  --spacing: 0.25rem;                       /* use calc(var(--spacing) * n) */

  --text-xs: 0.75rem;   --text-xs--line-height: calc(1 / 0.75);
  --text-sm: 0.875rem;  --text-sm--line-height: calc(1.25 / 0.875);
  --text-base: 1rem;    --text-base--line-height: calc(1.5 / 1);
  --text-lg: 1.125rem;  --text-lg--line-height: calc(1.75 / 1.125);
  --text-xl: 1.25rem;   --text-xl--line-height: calc(1.75 / 1.25);
  --text-2xl: 1.5rem;   --text-2xl--line-height: calc(2 / 1.5);
  --text-3xl: 1.875rem; --text-3xl--line-height: calc(2.25 / 1.875);
  --text-4xl: 2.25rem;  --text-4xl--line-height: calc(2.5 / 2.25);

  --font-weight-normal: 400; --font-weight-medium: 500;
  --font-weight-semibold: 600; --font-weight-bold: 700;

  --tracking-tight: -0.025em; --tracking-wide: 0.025em; --tracking-wider: 0.05em;
  --leading-tight: 1.25; --leading-snug: 1.375; --leading-normal: 1.5; --leading-relaxed: 1.625;

  --radius-xs: 0.125rem; --radius-sm: 0.25rem; --radius-md: 0.375rem; --radius-lg: 0.5rem;
  --radius-xl: 0.75rem;  --radius-2xl: 1rem;   --radius-3xl: 1.5rem;  --radius-4xl: 2rem;

  --shadow-2xs: 0 1px rgb(0 0 0 / 0.05);
  --shadow-xs: 0 1px 2px 0 rgb(0 0 0 / 0.05);
  --shadow-sm: 0 1px 3px 0 rgb(0 0 0 / 0.1), 0 1px 2px -1px rgb(0 0 0 / 0.1);
  --shadow-md: 0 4px 6px -1px rgb(0 0 0 / 0.1), 0 2px 4px -2px rgb(0 0 0 / 0.1);
  --shadow-lg: 0 10px 15px -3px rgb(0 0 0 / 0.1), 0 4px 6px -4px rgb(0 0 0 / 0.1);
  --shadow-xl: 0 20px 25px -5px rgb(0 0 0 / 0.1), 0 8px 10px -6px rgb(0 0 0 / 0.1);
  --shadow-2xl: 0 25px 50px -12px rgb(0 0 0 / 0.25);
  --inset-shadow-2xs: inset 0 1px rgb(0 0 0 / 0.05);
  --inset-shadow-xs: inset 0 1px 1px rgb(0 0 0 / 0.05);
  --inset-shadow-sm: inset 0 2px 4px rgb(0 0 0 / 0.05);

  --container-xs: 20rem; --container-sm: 24rem; --container-md: 28rem; --container-lg: 32rem;
  --container-xl: 36rem; --container-2xl: 42rem; --container-3xl: 48rem; --container-4xl: 56rem;
  --container-5xl: 64rem; --container-6xl: 72rem; --container-7xl: 80rem;
}
/* Breakpoints (custom properties cannot be used inside @media; write the literal):
   sm 40rem (640px), md 48rem (768px), lg 64rem (1024px), xl 80rem (1280px), 2xl 96rem (1536px) */
```

All values above are **[V] (S9)**. The px equivalents of the breakpoints are my arithmetic at a 16 px root. The `font-weight` tokens 100, 200, 300, 800 and 900, the `--text-5xl`…`9xl` sizes (line height 1), `--tracking-tighter -0.05em`, `--tracking-widest 0.1em` and `--leading-loose 2` also exist and were left out above for brevity **[V] (S9)**. In v4, `shadow-sm` is what v3 called `shadow`, and `rounded-sm` is v3's `rounded` **[V] (S14)**.

**The neutral palettes.** Each has 11 shades (50, 100–900, 950) in OKLCH **[V] (S9, S12)**. The docs do not describe their character **[V] (S12)**, so the following is read off the values in S9. The numbers are **[V]**; the adjectives are my reading.

| palette | hue (OKLCH h) | max chroma (at 500–600) | character |
|---|---|---|---|
| `slate` | ~248–266 | 0.046 | cool, visibly blue |
| `gray` | ~256–265 | 0.034 | cool, slightly blue |
| `zinc` | ~286 | 0.017 | barely cool (violet-blue trace) |
| `neutral` | none | 0 | pure grey |
| `stone` | ~34–106 | 0.013 | warm (brown/yellow trace) |

v4.3 also ships `mauve`, `olive`, `mist` and `taupe` neutrals **[V] (S9, S12)**. The lightness steps line up across the five palettes: 50 ≈ 98.5 %, 100 ≈ 97 %, 200 ≈ 92–93 %, 300 ≈ 87 %, 400 ≈ 70.5 %, 500 ≈ 55 %, 600 ≈ 44 %, 700 ≈ 37 %, 800 ≈ 27–28 %, 900 ≈ 21 %, 950 ≈ 13–14.7 % **[V] (S9)**. The `gray` and `slate` values for copying **[V] (S9)**:

```
gray:  50 oklch(98.5% 0.002 247.839)  100 oklch(96.7% 0.003 264.542)  200 oklch(92.8% 0.006 264.531)
       300 oklch(87.2% 0.01 258.338)  400 oklch(70.7% 0.022 261.325)  500 oklch(55.1% 0.027 264.364)
       600 oklch(44.6% 0.03 256.802)  700 oklch(37.3% 0.034 259.733)  800 oklch(27.8% 0.033 256.848)
       900 oklch(21% 0.034 264.665)   950 oklch(13% 0.028 261.692)
slate: 50 oklch(98.4% 0.003 247.858)  100 oklch(96.8% 0.007 247.896)  200 oklch(92.9% 0.013 255.508)
       300 oklch(86.9% 0.022 252.894) 400 oklch(70.4% 0.04 256.788)   500 oklch(55.4% 0.046 257.417)
       600 oklch(44.6% 0.043 257.281) 700 oklch(37.2% 0.044 257.287) 800 oklch(27.9% 0.041 260.031)
       900 oklch(20.8% 0.042 265.755) 950 oklch(12.9% 0.042 264.695)
blue (a primary): 50 oklch(97% 0.014 254.604) 100 oklch(93.2% 0.032 255.585) 500 oklch(62.3% 0.214 259.815)
       600 oklch(54.6% 0.245 262.881) 700 oklch(48.8% 0.243 264.376) 900 oklch(37.9% 0.146 265.522)
red (danger): 50 oklch(97.1% 0.013 17.38) 100 oklch(93.6% 0.032 17.717) 600 oklch(57.7% 0.245 27.325)
       700 oklch(50.5% 0.213 27.518) 800 oklch(44.4% 0.177 26.899) 950 oklch(25.8% 0.092 26.042)
```

**How shades are used, light and dark.** Tailwind's own docs show a card with `bg-white` / `dark:bg-gray-800`, a heading with `text-gray-900` / `dark:text-white`, body text with `text-gray-500` / `dark:text-gray-400`, a hairline `ring-gray-900/5`, and a page background `bg-white` / `dark:bg-black` **[V] (S13, S12)**. The colours page also shows `border-pink-300` on `bg-pink-100`, and `text-gray-700` beside `text-gray-950` emphasis **[V] (S12)**. Generalised as a **[Rec]** mapping for our tokens:

| role | light | dark |
|---|---|---|
| page background | gray-50 (or white) | gray-950 |
| surface (card) | white | gray-900 / gray-800 |
| border / hairline | gray-200, or gray-950 at 5–10 % | white at 10 % |
| primary text | gray-900 | white / gray-50 |
| secondary text | gray-600 (gray-500 only at ≥ 4.5:1) | gray-400 |
| primary button | blue-600 bg, white text | blue-500 bg, white text |
| tinted alert | red-50 bg, red-800 text, red-600 accent border | red-950 bg, red-200 text, red-500 border |

**Dark mode approach.** Tailwind's `dark` variant uses `prefers-color-scheme` by default. It can be switched to a class or a data attribute with `@custom-variant dark (&:where(.dark, .dark *));`. Its three-way light/dark/system example stores the choice in `localStorage`, notes that it could also be stored server-side with the class rendered on the server, and runs an inline script in `<head>` to avoid a flash **[V] (S13)**. **[Rec]** We have no JS and no toggle, so use `@media (prefers-color-scheme: dark)` to redefine the colour custom properties, and add `color-scheme: light dark` so native controls follow **[NV]** for `color-scheme` behaviour details.

**Focus ring convention.** In v4, `ring` is `box-shadow` `0 0 0 1px` and its colour defaults to `currentColor` (it was 3px `blue-500` in v3). For a v3-style ring, use `ring-3` and name a colour **[V] (S14, S15)**. The docs' focus example on outlines is `outline-offset-2 outline-sky-500 focus:outline-2` **[V] (S16)**. Use `outline-hidden`, not `outline-none`, when hiding an outline: the old `outline-none` kept an invisible outline for forced-colours mode **[V] (S14)**. Border and divide colours default to `currentColor` in v4 (they were `gray-200` in v3) **[V] (S14)**. **[Rec]** For us: `:focus-visible { outline: 2px solid var(--color-focus); outline-offset: 2px; }`, with the colour at ≥ 3:1 against both the surface and the page (WCAG 1.4.11) **[V] for the threshold (S20)**. Use `outline` rather than `box-shadow` so the ring survives forced-colours mode **[NV]**.

**Breakpoints and 320 px.** WCAG 2.2 1.4.10 Reflow requires content to work at a width of 320 CSS px without two-dimensional scrolling **[V] (S20)**. Tailwind's smallest breakpoint is 40rem (640 px), so its design is mobile-first: the base styles are the narrow ones and `@media (min-width: 40rem)` adds the wide layout **[V] values (S9); [NV] that this is "Tailwind's stated philosophy" (the responsive docs page was not opened)**.

## 9. Review checklist

1. Does every view have exactly one primary action, styled as the only filled button?
2. Are secondary actions outlined or plain, and tertiary ones styled as links?
3. Is a destructive action (revoke a share, sign out) red only where it *is* the primary action, as in its own confirmation page?
4. Can the page's hierarchy be read in greyscale (with colour removed)?
5. Is hierarchy made mostly by weight and colour, with at most 3 text colours and 2–3 weights, none under 400?
6. Is secondary text de-emphasised (lighter colour) rather than primary text made bigger?
7. Are labels secondary (smaller, softer), and has every "Label: value" pair been checked for whether the label is needed?
8. Is the `h1`'s size chosen by what matters on the page, not by its tag?
9. Does every margin, padding, gap and width come from the spacing scale (`calc(var(--spacing) * n)` on the curated steps)? Are there no stray values such as `15px` or `13px`?
10. Is the gap between groups clearly larger than the gap within a group (label to its input < field to field < section to section)?
11. Does content have a `max-width` (forms and cards ~28rem, Dashboard ~64rem) instead of stretching to the window?
12. Does every page work at 320 px wide with no horizontal scroll, no clipped text and touch targets of at least ~40 px?
13. Does every font size come from the type scale, each with its paired line height?
14. Is running text (error explanations) limited to about 65ch?
15. Are uppercase labels letter-spaced (`0.05em`) and large headings set with line height near 1.25 or less?
16. Are mixed-size items on one line aligned to the baseline?
17. Are numbers and dates in tables right-aligned, with `tabular-nums`?
18. In the application list, are names styled as bold dark text rather than bright link colour? Are links inside sentences still recognisable as links, by colour and underline?
19. Do all colours come from the token palette, with no computed `lighten()`/`darken()` shades and no new one-off hex values?
20. Are the greys tinted (gray, slate, zinc or stone) consistently, with one neutral family chosen for the whole product?
21. Does all text meet 4.5:1 (3:1 for large text), and do borders of controls and the focus ring meet 3:1, in both light and dark mode?
22. On any coloured background, is secondary text a tint of that colour or translucent white, not grey?
23. Is meaning never carried by colour alone (errors and refusals have words, a heading and possibly an icon)?
24. Is `:focus-visible` clearly visible on every link, button and input, using `outline` with an offset, in both themes?
25. Are borders used only where spacing, a background change or a shadow would not do? Are there no double borders where cards meet?
26. Do shadows come from the fixed set only, offset downward (light from above), with elevation matching importance?
27. Is one border radius used throughout (plus perhaps one larger one for cards)?
28. Do the empty Dashboard and empty share list have a message and the single action that fills them, without useless controls?
29. Do error and refusal pages say what happened, why, and what to do next, with one way onward (one primary link)?
30. Does dark mode redefine only the colour custom properties under `prefers-color-scheme: dark`, with `color-scheme` set so native form controls match?

## Sources

All read on 2026-11-20.

- S1. Refactoring UI home page, https://www.refactoringui.com/ (WebFetch summary).
- S2. Refactoring UI book page with the full table of contents, https://refactoringui.com/book (raw HTML text).
- S3. "Building Your Color Palette", adapted from the book, https://www.refactoringui.com/previews/building-your-color-palette (raw HTML text).
- S4. Adam Wathan & Steve Schoger, "7 Practical Tips for Cheating at Design", Medium, 20 Feb 2018, https://medium.com/refactoring-ui/7-practical-tips-for-cheating-at-design-40c736799886. Medium returned HTTP 403, so it was read in full from the Wayback Machine snapshot of 2026-11-15: http://web.archive.org/web/20261115233253/https://medium.com/refactoring-ui/7-practical-tips-for-cheating-at-design-40c736799886
- S5. Refactoring UI, "Redesigning Laravel.io", Medium, 4 Oct 2017, https://medium.com/refactoring-ui/redesigning-laravel-io-c47ac495dff0. Read in full from the publication's RSS feed, https://medium.com/feed/refactoring-ui
- S6. Steve Schoger, "The Little Details of UI Design", Laracon Online, 7 Feb 2018, slides with transcript text, https://speakerdeck.com/steveschoger/the-little-details-of-ui-design
- S7. Notes on Steve Schoger's CSS Day 2019 talk "Practical Solutions to Common UI Design Problems", gist by ynotdraw (secondary), https://gist.github.com/ynotdraw/9351627d7509cc35813eeac4245cab3b
- S8. Bramus Van Damme, "Steve Schoger | Refactoring UI — Tips and tricks…", 18 Jun 2019 (secondary, confirms the CSS Day 2019 talk), https://www.bram.us/2019/06/18/steve-schoger-refactoring-ui-tips-and-tricks-to-make-a-site-visually-more-interesting
- S9. Tailwind CSS `theme.css` at tag v4.3.5 (identical to `main`), https://raw.githubusercontent.com/tailwindlabs/tailwindcss/v4.3.5/packages/tailwindcss/theme.css
- S10. Tailwind CSS docs, "Theme variables", v4.3, https://tailwindcss.com/docs/theme
- S11. Tailwind CSS v3.4.17 `stubs/config.full.js` (the v3 spacing scale), https://raw.githubusercontent.com/tailwindlabs/tailwindcss/v3.4.17/stubs/config.full.js
- S12. Tailwind CSS docs, "Colors", https://tailwindcss.com/docs/colors
- S13. Tailwind CSS docs, "Dark mode", https://tailwindcss.com/docs/dark-mode
- S14. Tailwind CSS docs, "Upgrade guide" (v3 → v4 changes), https://tailwindcss.com/docs/upgrade-guide
- S15. Tailwind CSS docs, "box-shadow" (ring utilities), https://tailwindcss.com/docs/box-shadow (first 100,000 characters read)
- S16. Tailwind CSS docs, "outline-width", https://tailwindcss.com/docs/outline-width
- S17. Tailwind CSS docs, "padding", https://tailwindcss.com/docs/padding
- S18. Adam Wathan, "Tailwind CSS v4.0", 22 Jan 2025, https://tailwindcss.com/blog/tailwindcss-v4
- S19. Tailwind CSS LICENSE (MIT), https://raw.githubusercontent.com/tailwindlabs/tailwindcss/main/LICENSE
- S20. W3C, Web Content Accessibility Guidelines (WCAG) 2.2, https://www.w3.org/TR/WCAG22/ (1.4.1, 1.4.3, 1.4.10, 1.4.11, 2.4.7)
- S21. Tailwind CSS `preflight.css` at tag v4.3.5, https://raw.githubusercontent.com/tailwindlabs/tailwindcss/v4.3.5/packages/tailwindcss/preflight.css

## What could not be verified

- **The book's chapter bodies.** The book is a paid PDF. Only its table of contents (S2) and one adapted chapter (S3, colour palettes) were read. Specific numbers often quoted from it are **[NV]**: 45–75 characters (20–35 em) per line, the "no two adjacent values closer than ~25 %" spacing rule, the greyscale-first advice, and the content of "Baseline, not center", "Relative sizing doesn't scale", "Empty states" and "Think outside the box". The principles are stated here by chapter title and from the free articles.
- **Steve Schoger's quick tips on Twitter/X** (the Twitter Moment linked from S1) were not fetched. X is not readable without signing in.
- **The CSS Day 2019 talk video** was not watched. What it says is known only from secondary notes (S7, S8): five elevation levels, two-part shadows, flipping contrast, rotating hue, warm and cool greys, 40–48 px inputs, `tnum`.
- **Tailwind `ring-offset` defaults** and the responsive-design docs page were not read (S15 was cut off at 100,000 characters).
- **Exact `oklch()` and `color-scheme` support in Firefox 128,** and whether `outline` is more robust than `box-shadow` in forced-colours mode, were not checked.
