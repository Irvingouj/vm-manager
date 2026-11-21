# HCI and usability principles for the platform's pages

Date: 2026-11-20. Scope: the platform's own web pages: the Dashboard, the sign-in and sign-out pages, the form that shares an Application with a person or Group (makes them a Contributor, or lets them use it), and the pages that say a request was refused. The audience is an engineer, usually a Coding agent, who has to build these pages and then check them without a usability lab.

## Summary

The pages should follow a small number of rules that have held up for thirty years and have numbers attached. Show what is going on, in the reader's words and the product's glossary. Let people undo or back out. Prefer showing over making people remember. Say what went wrong and what to do next, next to the thing that went wrong. Meet WCAG 2.2 AA, which turns much of this into measurable thresholds: 4.5:1 text contrast (3:1 for large text and for the edges of controls), pointer targets at least 24 by 24 CSS px, a visible focus indicator, no sideways scrolling at 320 CSS px, nothing lost at 200 % text size, no colour-only meaning, and sign-in that does not test memory. The "laws" (Fitts, Hick–Hyman, Miller/Cowan, response-time limits, Jakob's, Tesler's) are useful as directions, not as formulas to apply: make the main action big and near, keep choices few where speed matters, never make people carry information from one screen to another, answer within about 0.1 s to 1 s or show progress, and look like other sites. Most of this can be checked by an agent without users: a heuristic pass against the ten heuristics, a cognitive walkthrough with its four questions, a computed contrast check, a keyboard-only walk, and screenshots at several widths in light and dark. A 36-question checklist closes the document.

### How each claim is marked

- **[verified]**: I opened the primary or authoritative source in this session and read it there (via a fetch that returned the page's text or a model summary of it).
- **[read from source]**: reported by a secondary source I opened (for example lawsofux.com, an NN/g article quoting an older paper, or search-result excerpts), not by the original.
- **[not verified]**: from my own knowledge or a search snippet; not confirmed against a source in this session.
- **[recommendation]**: this document's own application of the sources to this product; not a claim about the sources.

Fetches went through a tool that returns a model's summary of each page, not the raw page. A summary can be wrong in detail; where a number matters (WCAG thresholds), the number was read in the WCAG text itself.

---

## 1. Nielsen's ten usability heuristics

The ten heuristics were published by Jakob Nielsen in 1994; NN/g's page was last reviewed in January 2024 [verified]. Each is stated in my words, then applied to the four kinds of page. The applications are [recommendation].

| # | Heuristic (Nielsen's name) | What it asks |
|---|---|---|
| 1 | Visibility of system status | Tell people what the system is doing, promptly [verified]. |
| 2 | Match between the system and the real world | Use the reader's words and concepts, in a natural order [verified]. |
| 3 | User control and freedom | Give a clearly marked way out of an unwanted state [verified]. |
| 4 | Consistency and standards | Same words and behaviour throughout; follow platform conventions [verified]. |
| 5 | Error prevention | Remove error-prone conditions, or confirm before risky actions [verified]. |
| 6 | Recognition rather than recall | Keep options and information visible [verified]. |
| 7 | Flexibility and efficiency of use | Shortcuts for experts that do not get in novices' way [verified]. |
| 8 | Aesthetic and minimalist design | Leave out what is irrelevant or rarely needed [verified]. |
| 9 | Help users recognize, diagnose and recover from errors | Plain-language error, the precise problem, a suggested fix [verified]. |
| 10 | Help and documentation | Task-focused, searchable, short, ideally in context [verified]. |

**Applied** [recommendation]:

1. **Visibility of status.**
   - *Dashboard*: each Application shows its state (serving which Version, disabled or not) and when that last changed; the signed-in User's name is always visible; loading is distinguished from "no Applications" (see §7).
   - *Sharing form*: after submit, say what changed ("Dana's Group `facilities` can now contribute to room-issues") on the page the person lands on; the button shows that it is working if the reply takes more than about a second.
   - *Sign-in/out*: say who is signed in, and after sign-out say plainly that they are signed out and what happens to other tabs.
   - *Refusal pages*: say that the request was refused (not "error"), by whom (the platform), and for which Application.
2. **Match the real world.** Use the glossary's words: User, Application, Contributor, Group, Platform administrator, Dashboard, Version. Never "worker", "deployment", "owner", "role" or HTTP status words in the page body. A refusal says "You are not in a Group that can use this Application", not "403 Forbidden".
3. **Control and freedom.** Every page has a way back to the Dashboard. The sharing form has Cancel. Removing a Contributor is undoable or confirmed (see §6). Sign-out is reachable from every signed-in page.
4. **Consistency.** One name per thing across pages; the same header on every page; the same control looks the same everywhere (WCAG 3.2.3 and 3.2.4, §5).
5. **Error prevention.** In the sharing form, choose a person or Group from the directory instead of typing an exact identifier; refuse a duplicate share before submit; do not offer actions the User may not take, *but* say why (see disabled buttons, §6).
6. **Recognition over recall.** The sharing form shows who already has access; refusal pages name the Application and the missing Group so the person does not have to remember what they tried; the Dashboard lists Applications rather than asking for a name.
7. **Flexibility.** Stable, bookmarkable URLs for each Application; a filter box on the Dashboard once the list is long; keyboard operation throughout.
8. **Minimalist design.** One primary action per page. A refusal page carries the reason, the next step and a way back, nothing else.
9. **Recover from errors.** A refusal page states what happened, why, and what to do: whom to ask (the Contributors or a Platform administrator), with the exact Group name to ask for. Form errors follow §6.
10. **Help.** A short line of help text where a decision is made (what a Contributor can do, before sharing); a link to the documentation site in the footer.

## 2. Don Norman's principles

- **Affordances and signifiers.** Norman took "affordance" from J. J. Gibson, for whom it meant the actions an environment makes possible whether or not anyone can perceive them; Norman's 1988 use was about *perceived* affordances, and in his 2008 *Interactions* column he argued designers should talk about **signifiers**: any perceivable cue, deliberate or accidental, that tells people what action is possible [verified]. The 2013 revised edition of *The Design of Everyday Things* added signifiers to the book [read from source]. *Consequence*: on a web page nothing has a physical affordance; whether something is clickable is entirely a matter of signifiers: underlined link text, button shapes with a fill or border, a pointer cursor, a focus ring [recommendation]. NN/g adds that buttons should look raised and fields recessed, and that hidden gestures cause mistakes [verified].
- **Mapping.** The relation between a control and its effect should be evident from layout. *Consequence*: a "Remove" control sits on the row of the person it removes, not in a toolbar above a list [recommendation]. (Norman's definition of mapping was not opened in this session: [not verified].)
- **Feedback.** Every action gets an immediate, informative response; see the response-time limits in §3 [recommendation, built on the verified limits].
- **Constraints.** Limit what can be entered to what is valid. NN/g lists constraints, suggestions, good defaults and forgiving formatting as the defences against slips [verified].
- **Conceptual models.** The person's model of how the system works must match the system. NN/g: mistakes come from an incorrect mental model; prevent them by researching expectations, following conventions, using clear signifiers, and previewing the result before committing [verified]. *Consequence*: the Dashboard should teach the model in its structure: "Applications you can contribute to" and "Applications shared with you" are two visibly separate groups, because that is the distinction the platform actually makes [recommendation].
- **Gulfs of execution and evaluation.** Coined by Hutchins, Hollan and Norman (1986). The gulf of evaluation is the effort to understand the system's current state (perceive the indicator, then interpret it); the gulf of execution is the effort to work out and perform the action that reaches a goal. NN/g advises fixing evaluation first, because a plan made on a misread state fails; labels should describe the current state unambiguously; familiar controls narrow both gulfs [verified]. *Consequence*: a toggle labelled "Shared" must make it obvious whether it shows the current state or the action; prefer a sentence ("Shared with 3 Groups") plus a separate button ("Share with someone") [recommendation].
- **Slips and mistakes.** A *slip* is doing something other than what one intended (often a similar action, on autopilot); a *mistake* is forming the wrong goal, usually from a wrong mental model [verified]. Slips are prevented by constraints, defaults, suggestions and forgiving input; mistakes by a correct model, conventions, and previews [verified].
- **Designing for error.** NN/g: offer undo wherever possible; confirm only actions with serious consequences, especially irreversible ones; word the confirmation specifically and label its buttons with outcomes ("Remove Contributor", "Keep"); do not default to the destructive choice; do not confirm routine actions, because frequent confirmations train people to click through [verified].

## 3. Quantitative laws

Each entry: the original source, how sure I am of it, and the practical consequence for these pages.

- **Fitts's law.** Time to hit a target grows with the logarithm of distance over target width: T = a + b·log2(2D/W). Original: P. M. Fitts, "The information capacity of the human motor system in controlling the amplitude of movement", *Journal of Experimental Psychology* 47(6), 381–391, 1954 [read from source: NN/g's article by Raluca Budiu, 2022; the APA record would not load]. NN/g's consequences [verified]: bigger targets are faster and produce fewer errors; make the whole row or the icon-plus-label clickable; put related controls near each other (Submit right after the last field); with a mouse, screen edges stop the pointer so edge targets are effectively infinite, but this does not hold on touch screens; invisible padding helps only if people know it is there; crowded small targets cause wrong hits. *For us*: the Dashboard row is one large link to the Application; the primary button sits directly under the form [recommendation].
- **Hick–Hyman law.** Choice reaction time grows roughly with the logarithm of the number of equally likely alternatives. Original: W. E. Hick, "On the rate of gain of information", *Quarterly Journal of Experimental Psychology* 4(1), 11–26, 1952 (DOI 10.1080/17470215208416600) [read from source: search results agree on this citation; the publisher page returned 403]; Hyman (1953) extended it to unequal probabilities [read from source]. The law describes simple choice-reaction tasks, not reading a menu; lawsofux.com warns against simplifying until options become abstract [read from source]. *For us*: few actions per page, one visually primary; long lists (Groups to share with) are searched, not scanned [recommendation].
- **Miller and Cowan: working-memory limits.** Miller's 1956 paper (*Psychological Review* 63, 81–97) is about two different limits: absolute judgment of one-dimensional stimuli (about 5–7 categories) and immediate memory span (about seven items, varying with material); he stresses chunking and calls the coincidence of the "sevens" possibly "a pernicious, Pythagorean coincidence" [verified]. Cowan (2001, *Behavioral and Brain Sciences* 24, 87–114) argues that when rehearsal and chunking are controlled, the central capacity is about four chunks [read from source: abstract via search results; the PubMed page needed cookies]. **Common misuse**: "menus must have at most seven items." Nielsen calls this a misconception, because people *recognise* menu items on screen rather than recall them [verified]. *The correct consequence*: never require people to *carry* information across screens or steps (a Group name from a refusal page to a request form, a code from one tab to another); put it on screen or prefill it [recommendation].
- **Power law of practice.** Time per trial falls as a power function of the number of trials. Source: A. Newell and P. Rosenbloom, "Mechanisms of skill acquisition and the law of practice", in J. R. Anderson (ed.), *Cognitive Skills and Their Acquisition*, Erlbaum, 1981 [read from source: bibliographic records and later reviews; full text not opened]. Its status as a law has been questioned as partly an artifact of averaging (Heathcote, Brown and Mewhort, 2000) [read from source]. *For us*: people get fast at whatever layout they meet repeatedly, so do not move things between releases without cause; keep the Dashboard's order stable [recommendation].
- **Jakob's law.** People spend most of their time on other sites, so they expect yours to work like those [verified: NN/g video by Nielsen, 2017]. *For us*: sign-out in the top-right user area, links that look like links, a standard table or list on the Dashboard, the browser Back button working [recommendation].
- **Tesler's law (conservation of complexity).** Every task has an irreducible complexity that someone must bear, the system or the user; attributed to Larry Tesler at Xerox PARC in the mid-1980s, with his own statement in an interview in Dan Saffer's *Designing for Interaction* [read from source: lawsofux.com; no primary source opened]. *For us*: the platform, not the person, should work out who may be shared with, which Group is missing, and whom to ask [recommendation].
- **Response-time limits and the Doherty threshold.** Nielsen (1993, from *Usability Engineering*, citing Miller 1968 and Card, Robertson and Mackinlay 1991): about **0.1 s** feels instantaneous; about **1 s** keeps the flow of thought (people notice the delay); about **10 s** is the limit of attention, beyond which show a percent-done indicator; for 2–10 s a lighter busy indicator is enough [verified]. The "Doherty threshold" is attributed to W. J. Doherty and A. J. Thadani (Thadhani), "The Economic Value of Rapid Response Time", IBM, 1982 (form GE20-0752-00), usually quoted as 400 ms [read from source: lawsofux.com and search excerpts of IBM's and the Computer History Museum's pages; the IBM page failed to load]. *For us*: page responses under 1 s on the Dashboard; any submit that can exceed ~1 s shows it is working and disables a second submit; nothing should take 10 s without progress [recommendation].
- **Aesthetic–usability effect.** People rate attractive interfaces as easier to use. Original: M. Kurosu and K. Kashimura, Hitachi, CHI 1995 (26 ATM layouts, 252 participants) [read from source: NN/g, Kate Moran, 2024; the ACM page returned 403]. NN/g's caveat: it masks small problems only, and it hides problems in testing, so watch behaviour rather than comments [verified]. *For us*: polish matters, but a screenshot that looks good is not evidence that the page works; walk the task (§11) [recommendation].
- **Serial position effect.** Items at the start and end of a list are remembered better than those in the middle; term attributed to Ebbinghaus [read from source: lawsofux.com; no primary opened]. *For us*: put the most-used item first in the header, sign-out last [recommendation].
- **Von Restorff (isolation) effect.** The one item that differs from the rest is remembered; Hedwig von Restorff, 1933 [read from source: lawsofux.com]. Caveats there: emphasise sparingly, do not rely on colour alone, be careful with motion [read from source]. *For us*: one primary button per page; refusal pages emphasise only the next step [recommendation].
- **Goal-gradient effect.** Effort increases as the goal nears; Clark Hull (1932, rats in an alley 1934); revisited for customers by Kivetz, Urminsky and Zheng, "The Goal-Gradient Hypothesis Resurrected", *Journal of Marketing Research* 43(1), 2006 [read from source: lawsofux.com names the paper but not the year; journal page returned 403; year from the DOI 10.1509/jmkr.43.1.39, not verified]. *For us*: little application, since our flows are one page; if a multi-step flow appears, show step "2 of 3" [recommendation].
- **Postel's law (robustness principle).** "Be conservative in what you do, be liberal in what you accept from others": J. Postel, RFC 761 (TCP), January 1980, section 2.10 [verified]. It was written for protocol implementations. *For input*: accept what people naturally type (leading/trailing spaces, a pasted email address with a display name, case differences in Group names) and normalise it; NN/g calls this forgiving formatting [verified for NN/g]. Do not apply it to anything with security meaning: an identity or a Grant is matched exactly [recommendation].

## 4. Gestalt principles

Gestalt psychology comes from Wertheimer, Koffka and Köhler in the early twentieth century (Wertheimer's grouping paper is usually dated 1923) [read from source: IxDF; 1923 date not verified]. Definitions below are from NN/g for proximity, similarity and common region [verified] and IxDF for the rest [read from source]. Consequences are [recommendation] unless marked.

- **Proximity.** Things close together are seen as a group, and proximity can beat colour and shape. NN/g: use space to tie a label to its field and a heading to its section, separate unrelated buttons, and recheck groupings at every breakpoint because stacking can pull related items apart [verified]. *Rule*: space inside a group is clearly smaller than space between groups (e.g. 4–8 px between a label and its field, 24 px or more between fields, more between sections); a field's error message sits with its field.
- **Similarity.** Things that look alike are seen as related. NN/g: one link colour reserved for clickable text, a distinct style for the primary action, consistent icons for like functions [verified]. *Rule*: never colour non-clickable text in the link colour; all Application rows on the Dashboard share one template.
- **Common region.** Things inside a shared boundary (border, background, card) are grouped, and a region overrides proximity and similarity. NN/g warns that decorative boxes add clutter and full-width colour bands make "false floors" [verified]. *Rule*: use a border or background only where space alone fails (a card per Application on narrow screens, the error summary box); no boxes inside boxes.
- **Continuity.** The eye follows a line or path. *Rule*: align labels, fields and buttons on one left edge so the eye runs straight down a form; align table columns.
- **Closure.** People complete incomplete shapes. *Rule*: a light rule or partial border is enough to suggest a group; it also means a clipped card at the edge of a scroll area signals more content (NN/g recommends cut-off columns to signal horizontal scrolling in mobile tables [verified]).
- **Figure/ground.** People separate a foreground from its background. *Rule*: dialogs and menus need a clearly separated layer (scrim, shadow or border with 3:1 contrast); in dark themes elevation is shown by lighter surfaces (§10).
- **Common fate.** Things that move or change together are grouped. *Rule*: when a row is removed or updated, animate (if at all) only that row, so the change is attributed to it; honour reduced motion (§5).

## 5. Accessibility requirements with numbers (WCAG 2.2)

WCAG 2.2 became a W3C Recommendation on 5 October 2023; the version I read is dated 12 December 2024 [verified for 12 December 2024; the 2023 date is not verified]. Target: **Level AA**. Each line: number, name, level, requirement [verified, from the WCAG 2.2 text and the named Understanding documents], then what it means here [recommendation].

| SC | Name (level) | Requirement | For these pages |
|---|---|---|---|
| 1.3.1 | Info and Relationships (A) | Structure conveyed visually is in the markup. | Real `<h1>`–`<h3>`, `<table>` with headers, `<label for>`, `<fieldset>/<legend>` for radio groups. |
| 1.4.1 | Use of Color (A) | Colour is not the only way information is conveyed. | Status has a word or icon; links in text are underlined. |
| 1.4.3 | Contrast (Minimum) (AA) | Text 4.5:1; large text 3:1. Large = at least 18 pt, or 14 pt bold (about 24 px / 18.66 px bold) [px conversion not verified]. | Compute every text/background pair in both themes. (#767676 on white is 4.54:1, computed in this session.) |
| 1.4.6 | Contrast (Enhanced) (AAA) | 7:1, large 4.5:1. | Optional target for body text. |
| 1.4.4 | Resize Text (AA) | Text resizes to 200 % without loss of content or function. | Use `rem`; no fixed-height boxes that clip text. |
| 1.4.10 | Reflow (AA) | No two-dimensional scrolling at 320 CSS px wide (= 1280 px at 400 %), or 256 px high for horizontal text; data tables and similar are excepted, but only the table itself, ideally in its own scroll container. | Dashboard table becomes a list or scrolls inside its own box; nothing else scrolls sideways. |
| 1.4.11 | Non-text Contrast (AA) | 3:1 for the visual parts needed to identify UI components and states, and for meaningful graphics. | Field borders, checkbox outlines, focus rings, status icons at 3:1 against neighbours. |
| 1.4.12 | Text Spacing (AA) | No loss when line height is 1.5×, paragraph spacing 2×, letter spacing 0.12×, word spacing 0.16× the font size. | Test with a bookmarklet/user stylesheet; buttons must grow, not clip. |
| 1.4.13 | Content on Hover or Focus (AA) | Hover/focus popups are dismissible (e.g. Escape), hoverable, persistent. | Avoid tooltips for anything essential. |
| 2.1.1 | Keyboard (A) | All functionality by keyboard. | Every action reachable by Tab and operable by Enter/Space. |
| 2.3.3 | Animation from Interactions (AAA) | Motion triggered by interaction can be turned off unless essential. | Honour `prefers-reduced-motion: reduce` (MDN: values `no-preference`/`reduce`, widely supported since January 2020 [verified]); MDN does not itself name the SC [verified]. |
| 2.4.2 | Page Titled (A) | Titles describe topic or purpose. | See §8. |
| 2.4.6 | Headings and Labels (AA) | Headings and labels describe topic or purpose. | "Share room-issues", not "Form". |
| 2.4.7 | Focus Visible (AA) | A keyboard focus indicator is visible. | Never `outline: none` without a replacement. |
| 2.4.11 | Focus Not Obscured (Minimum) (AA) | The focused element is not entirely hidden by author content. | Sticky headers must not cover focused fields; use `scroll-padding-top`. |
| 2.4.13 | Focus Appearance (AAA) | Indicator area at least that of a 2 CSS px perimeter, and 3:1 change between focused and unfocused pixels. Distinct from 1.4.11, which measures contrast against neighbours. | Recommended: a solid 2 px outline with offset, in a colour 3:1 against both the control and the page. |
| 2.5.8 | Target Size (Minimum) (AA) | Pointer targets at least 24×24 CSS px, unless a 24 px circle centred on each small target touches no other target or circle; exceptions for inline links in text, browser-native controls, equivalent controls, essential cases. | All buttons and icon buttons ≥ 24×24; aim higher (below). |
| 2.5.5 | Target Size (Enhanced) (AAA) | 44×44 CSS px. The Understanding document encourages 44 for important controls; 24 was chosen as a threshold workable for all pointers. | Primary buttons and row actions ≥ 44 px tall. Apple's HIG: hit region at least 44×44 pt (60×60 in visionOS) [verified]; Apple points are not CSS pixels [read from source: W3C task-force note]. |
| 3.1.1 | Language of Page (A) | Page language is programmatically set. | `<html lang="en">`. |
| 3.2.3 | Consistent Navigation (AA) | Repeated navigation in the same relative order. | One header component on every page. |
| 3.2.4 | Consistent Identification (AA) | Same function, same identification. | "Sign out" everywhere, never "Log out" on one page. |
| 3.3.1 | Error Identification (A) | The field in error is identified and the error described in text. | §6. |
| 3.3.2 | Labels or Instructions (A) | Labels or instructions where input is needed. | Visible label on every field. |
| 3.3.3 | Error Suggestion (AA) | When a fix is known, suggest it (unless that harms security). | "No Group called 'facilites'. Did you mean 'facilities'?" |
| 3.3.7 | Redundant Entry (A) | Information already entered in the same process is filled in or selectable. | Keep form values after an error; carry the Application into the share form. |
| 3.3.8 | Accessible Authentication (Minimum) (AA) | No cognitive function test (remembering a password, transcribing a code, puzzles) in an authentication step unless there is an alternative, a mechanism to help (password manager, paste), object recognition, or personal content. The Understanding document says not to block paste, to let a one-time code be pasted whole, and to keep recovery free of such tests. | Sign-in goes to the identity provider; our own pages must not block paste or autofill; if any code is ever shown, it is pasteable. |
| 3.3.9 | Accessible Authentication (Enhanced) (AAA) | As 3.3.8 without the object-recognition and personal-content exceptions. | Met if sign-in is the identity provider's redirect. |
| 4.1.2 | Name, Role, Value (A) | Every control has a programmatic name and role; states are exposed and changes notified. | Native `<button>`, `<a>`, `<input>`; `aria-expanded`, `aria-current="page"`, `aria-invalid`, `aria-describedby` for errors. |

## 6. Forms and errors

- **Label placement: above the field.** Penzo's 2006 eye-tracking study (UXmatters) found that labels above fields needed very short label-to-field saccades (about 50 ms) and attracted few fixations; right-aligned labels to the left almost halved fixations and completion time compared with left-aligned labels to the left [verified, as summarised from the article]. GOV.UK puts labels above inputs, short, in sentence case, without a trailing colon [verified]. *Rule*: top-aligned labels; they also survive narrow screens and 200 % zoom without layout changes [recommendation].
- **Placeholder text is not a label.** NN/g (Sherwin, 2014): it disappears when typing starts, so people forget what was asked and cannot check their answers; it is mistaken for a prefilled value; it is usually low-contrast; some screen readers skip it [verified]. GOV.UK: do not use placeholder text in place of a label, nor for hints [verified]. *Rule*: visible label plus, if needed, a hint below the label, linked by `aria-describedby` [recommendation].
- **Inline validation timing.** NN/g: validate a field when the person leaves it, not while they are still typing [verified]. Baymard (2024): validate on blur (or once a fixed-length value is complete); once an error is shown, recheck on every keystroke so it disappears as soon as the input is valid; positive confirmation can help [verified]. *Rule*: on blur first, then live until fixed; always revalidate on the server and redisplay [recommendation].
- **Error message wording.** GOV.UK: say what went wrong and how to fix it, in plain words and positive language; avoid "invalid", "forbidden", "illegal", "you forgot", "please", "sorry", "oops" and jargon; use an instruction for an empty field ("Enter a name or Group") and a description for a broken rule ("Name must be 35 characters or less"); never "An error occurred"; keep what the person typed [verified]. NN/g: explicit, polite, precise, constructive [verified].
- **Placement.** Next to the field (GOV.UK: after the label and hint, red, with a red border on the field), and in an error summary at the top with the same wording, each entry linking to its field [verified for GOV.UK; linking not checked in this session]. NN/g: the summary must not be the only indicator; add an icon so the error does not depend on colour [verified]. GOV.UK also uses a visually hidden "Error:" prefix for screen readers [verified]. Prefixing the page `<title>` with "Error:" is a GOV.UK practice I did not confirm in this session [not verified].
- **Not an error message**: GOV.UK says lack of permission or ineligibility is a separate page that explains what to do next, not a form error [verified]. *This is exactly our refusal page* [recommendation].
- **Required versus optional.** GOV.UK: mark optional fields with "(optional)" in the label; do not mark required fields with asterisks; ask only what you need [verified for the first two]. *Rule*: forms here are short; every field is required unless it says "(optional)" [recommendation].
- **Button labels.** GOV.UK: sentence case, describe the action, start with a verb ("Sign in", "Save and continue", "Sign out") [verified]. Apple: start with a verb, but uses title-style capitalisation [verified]; this product follows GOV.UK sentence case [recommendation]. NN/g: dialog buttons named for outcomes ("Delete file"/"Keep file"), not Yes/No [verified]. *Rule*: "Share", "Remove access", "Sign out", never "Submit" or "OK" [recommendation].
- **Destructive actions.** NN/g: prefer undo; confirm only serious, irreversible actions; specific wording; no default on the destructive choice [verified]. GOV.UK: warning (red) buttons are for serious, hard-to-undo actions, rarely needed, used for the final confirmation step, and never rely on colour alone [verified]. Apple: never make a destructive button the primary (default) button [verified]. *Rule*: removing a Contributor gets an "Undo" in the confirmation message for some seconds, or a confirmation page naming the person and the Application; disabling an Application (Platform administrator) is confirmed with its name [recommendation].
- **Disabled buttons.** GOV.UK: avoid them; they have poor contrast and confuse people [verified]. *Rule*: keep the button enabled and explain on submit, or replace the control with a sentence saying why the action is not available and whom to ask [recommendation].
- **Double submission.** GOV.UK: people double-click (habit, slow feedback, tremor); prevent it on the client and also guard on the server [verified]. Apple: show an in-button activity indicator for slow actions [verified].
- **Autocomplete and spellcheck.** GOV.UK: set `autocomplete` where a purpose exists (WCAG 1.3.5) and `spellcheck="false"` for names, email addresses and identifiers [verified].

## 7. Content and microcopy

- **Plain language.** GOV.UK requires plain English, explaining any technical term on first use [verified]; GOV.UK targets a reading age of 9 [read from source: search excerpt of the "Writing for GOV.UK" page, which now redirects]. A 25-word sentence limit is often attributed to GOV.UK [not verified].
- **Front-loading.** NN/g (Pernice, 2017): readers scan in an F-shape when nothing guides them; the first words of headings, list items and links get the most fixations, so put the information-carrying words first and use descriptive link text, never "click here" [verified]. *Rule*: "room-issues: shared with you", not "This Application has been shared with you: room-issues" [recommendation].
- **Sentence case.** GOV.UK: sentence case for headings and page titles [verified]. NN/g's command-link article also defaults to sentence case [verified].
- **Words.** GOV.UK: say "select", not "click", because not everyone clicks [verified]. Consistent vocabulary is heuristic 4 and WCAG 3.2.4 [verified]. *Rule*: only the glossary's terms, with its "avoid" words banned in UI text: Application (not app, service, project); Version (not release, build, deployment); Contributor (not owner, maintainer); Group (not team, role); Platform administrator (not admin, IT); Dashboard (not portal, console); User for the person using an Application [recommendation, from `CONTEXT.md`].
- **Empty states.** NN/g (Kaplan, 2021): say the state (finished, no results) and never show "no records" while still loading; teach what the area is for; give a direct action to fill it [verified]. *Rule*: a new employee's Dashboard says "No Applications yet. Applications shared with you, and the ones you build, appear here." plus how to start one [recommendation].
- **Status and timestamps.** GOV.UK: "5:30pm", "midday", "midnight", no ordinals in dates, "to" for ranges [verified]. *Rule*: show relative time for recent events with the absolute time available (in a `<time datetime>` element and visible on focus/hover is not enough on touch, so show it in text on detail pages); state the time zone where it matters [recommendation; no source opened for relative timestamps].

## 8. Navigation and orientation

Every page should answer three questions: where am I, what can I do here, where can I go [recommendation; the three questions are a common framing whose origin I did not open: not verified].

- **Page titles** (WCAG 2.4.2 [verified]): specific part first, product last, e.g. "Share room-issues – Vibe Manager"; on a refusal, "Not allowed: room-issues – Vibe Manager" [recommendation; front-loading from NN/g, verified].
- **Persistent header.** Product name linking to the Dashboard, the signed-in person's name, "Sign out"; identical on every page (WCAG 3.2.3) [recommendation].
- **Breadcrumbs.** NN/g (2018): show hierarchy, not history; the current page is not a link; they help on deep sites and are not useful on flat ones (one or two levels) [verified]. *Rule*: our hierarchy is two levels (Dashboard → Application), so a back link "← Dashboard" or a header is enough; add breadcrumbs only if the depth grows [recommendation].
- **Current and visited state.** Mark the current page in navigation with `aria-current="page"` and a visible style [recommendation]. NN/g (2004): distinguish visited from unvisited links; it is a convention most users understand [verified].
- **Links versus buttons.** NN/g (Nielsen, 2007): links navigate, buttons act (change data or state); commands as links only for minor, secondary actions, worded as verbs [verified]. Links in running text are underlined (WCAG 1.4.1) [recommendation]. *Rule*: "Open room-issues" is a link; "Share", "Remove access", "Sign out" are buttons (sign-out changes state, so a POST form button) [recommendation].
- **Refusal pages** keep the header and give at least one way onward (Dashboard) and the specific next step [recommendation].

## 9. Responsive layout and input

- **Touch targets.** WCAG 2.5.8 floor 24×24 CSS px; 44 px recommended for important controls (2.5.5, Apple 44×44 pt) [verified]. Space between adjacent small targets [verified, NN/g Fitts].
- **Hover independence.** Nothing essential only on hover: touch has no hover and WCAG 1.4.13 constrains hover content [verified for 1.4.13]. NN/g also warns against hover-only error alerts [verified]. Use `@media (hover: hover)` only for enhancements [recommendation; media feature not opened: not verified].
- **Thumb reach.** Hoober (2013, 1,333 observations, 780 touching the screen): 49 % one-handed, 36 % cradled, 15 % two-handed; grips change often [verified]. In 2017 Hoober retracted his reach drawings: people touch the centre of the screen more, faster and more accurately [verified]. *Rule*: put primary actions in the content flow near the centre-bottom of what they act on; keep destructive actions away from frequently used ones; do not rely on a "thumb zone" diagram [recommendation].
- **Tables on narrow screens.** WCAG 1.4.10 allows a data table to scroll sideways inside itself [verified]. NN/g (Schade, 2017): sticky headers and first column, legible columns, show which columns matter, signal horizontal scroll by cutting off a column [verified]. *Rule*: the Dashboard table becomes one card per Application below a breakpoint (name as the link, state and last change beneath); keep the table for wide screens [recommendation].
- **Viewport.** The `meta viewport` must allow zoom (WCAG Reflow Understanding refers to that test rule) [verified].

## 10. Dark mode and colour

- **`prefers-color-scheme`**: `light` or `dark`; widely available since January 2020; pair with the `color-scheme` property or `<meta name="color-scheme">` so form controls and scrollbars follow [verified, MDN]. *Rule*: follow the system setting by default; an override, if offered, is remembered [recommendation].
- **Contrast in dark themes.** Same WCAG ratios apply in both themes [recommendation; WCAG is theme-agnostic]. Material (M2): base surface #121212 rather than black; dark surfaces should reach at least 15.8:1 against white body text so that text still passes 4.5:1 on the lightest elevated surface; elevation shown by lighter overlays [read from source: search excerpts of the M2 page, which did not render for me]. The Material codelab: #121212 base, desaturated (lighter) accent colours because saturated ones "vibrate" on dark, text at 87 % / 60 % / 38 % white for high/medium/disabled emphasis, avoid pure white text because it can appear to bleed [verified]. Computed in this session: white on #121212 is 18.73:1 and white on #000000 is 21:1; #DEDEDE on #121212 is 13.92:1.
- **Avoid pure extremes.** Use near-black (around #121212) and off-white text (around 87 % white) in dark mode; in light mode near-black text on white is fine [recommendation from the Material codelab]. Note that 38 %-opacity "disabled" text fails 4.5:1; WCAG exempts inactive components from 1.4.3, but GOV.UK's advice to avoid disabled controls applies [recommendation; the exemption is not verified in this session].
- **Not colour alone** (WCAG 1.4.1) [verified]. Status colours carry a word and an icon: "Serving Version 4" with a check, "Disabled" with a stop icon, "Refused" with a text reason. Red/green pairs must also differ in lightness. Status icons meet 3:1 (1.4.11) [recommendation].
- **Check both themes separately**: every colour pair is computed twice [recommendation].

## 11. Evaluation an agent can run without users

- **Heuristic evaluation.** NN/g (Moran and Gordon, 2023): 3–5 evaluators working independently, each 1–2 hours, two passes (learn the product, then note violations of each heuristic), consolidate afterwards [verified]. Severity ratings are commonly 0–4 (cosmetic to catastrophe) from Nielsen [not verified: the page I tried returned 404]. *Agent version*: run separate passes (or separate agents with no shared notes) over screenshots and the DOM, one per page, logging each finding as heuristic, page, element, severity [recommendation].
- **Cognitive walkthrough.** Lewis, Polson, Wharton and Rieman introduced it at CHI 1990; the streamlined practitioner's version is Wharton et al. 1994 in *Usability Inspection Methods* (Nielsen and Mack, eds.) [read from source: NN/g, Flaherty, 2022]. The four questions, asked at each step of a task for a first-time user [verified]:
  1. Will the person try to achieve the right result?
  2. Will they notice that the correct action is available?
  3. Will they associate that action with the result they want?
  4. After acting, will they see progress toward the goal?
  Any "no" fails the step [verified]. *Tasks for this product*: open an Application from the Dashboard; share an Application with a Group; remove a Contributor; recover from a refusal (find whom to ask, for which Group); sign out [recommendation].
- **Accessibility check, by script and by hand** [recommendation]:
  1. Compute contrast for every text and UI-component colour pair in both themes with the WCAG relative-luminance formula (as done in §10); text ≥ 4.5:1 (large ≥ 3:1); borders, icons, focus rings ≥ 3:1.
  2. Run an automated checker (for example axe-core) on each page; it finds only part of the issues, so do the rest by hand [not verified that any particular share is automatable].
  3. Keyboard-only walk: Tab through each page; every control reachable in a sensible order, focus always visible and never hidden under a sticky header, Enter/Space operate it, Escape closes popups, no trap.
  4. Reflow: viewport 320 CSS px wide; no horizontal page scroll except inside a table container.
  5. Zoom 200 % (text) and 400 % (page) on 1280 px; nothing clipped or overlapping; apply the 1.4.12 spacing overrides and look again.
  6. Measure target sizes from the DOM (`getBoundingClientRect`) ≥ 24×24, primary ≥ 44 high.
  7. Check `lang`, `<title>`, one `<h1>`, labelled fields, `aria-describedby` on errors, `prefers-reduced-motion` honoured.
- **Screenshot review.** Capture each page in each state (empty, normal, long list, error, refusal, signed out) at widths 320, 390, 768, 1280 and 1920 px, in light and dark, and compare: grouping by spacing (§4), one primary action, nothing clipped, status readable without colour [recommendation]. The aesthetic–usability effect cuts both ways here: a clean screenshot is not evidence the task works, so pair it with the walkthrough [recommendation, from §3].

## 12. Checklist

Each question should get "yes" for every page it applies to. All are [recommendation], derived from the sections cited.

**Orientation and navigation**
1. Does the `<title>` start with the page-specific part (e.g. "Share room-issues – …")? (§8, WCAG 2.4.2)
2. Is there exactly one `<h1>`, and does it say what the page is for? (2.4.6)
3. Is the same header, in the same order, on every page, with the signed-in name and "Sign out"? (3.2.3)
4. Can every page get back to the Dashboard in one action? (heuristic 3)
5. Is navigation done with links and actions with buttons, and is sign-out a POST button? (§8)
6. Are links in running text underlined and visited links distinguishable? (1.4.1, §8)

**Status and feedback**
7. Does every Application on the Dashboard show its state in words, not colour alone? (1.4.1, heuristic 1)
8. Is loading visibly different from "no Applications"? (§7)
9. Does the empty Dashboard say what will appear there and how to start? (§7)
10. Does every action show a result on the next page, naming what changed? (heuristic 1)
11. Does anything that can take over ~1 s show it is working and block a second submit? (§3, §6)
12. Are times shown in a consistent format, with the absolute time available in text? (§7)

**Words**
13. Are only glossary terms used, and none of their "avoid" words? (§7)
14. Is every heading, title and button in sentence case? (§7)
15. Does every button start with a verb that names its result, with no "Submit", "OK" or "Yes/No"? (§6)
16. Are headings and link texts front-loaded and free of "click here"? (§7)

**Forms**
17. Does every field have a visible label above it, with no placeholder used as a label? (§6, 3.3.2)
18. Are optional fields marked "(optional)", with no asterisks? (§6)
19. Is a field validated only after the person leaves it, then live until fixed? (§6)
20. Does each error say what happened and how to fix it, without "invalid", "please" or "sorry"? (§6, 3.3.1, 3.3.3)
21. Is each error shown next to its field and in a summary at the top that links to it? (§6)
22. Are entered values kept after an error? (3.3.7)
23. Does the sharing form choose people and Groups from a list rather than typing exact identifiers, and show who already has access? (heuristics 5 and 6)
24. Are there no disabled buttons, or does each one explain why it is unavailable? (§6)
25. Is every destructive action undoable or confirmed with a specific message, and never the default button? (§2, §6)

**Refusals and sign-in**
26. Does each refusal page say it was refused, why in plain words, and the next step (whom to ask, which Group to ask for)? (heuristic 9, §6)
27. Is no HTTP status code or internal identifier shown as the main message? (heuristic 2)
28. Does sign-in avoid blocking paste and autofill, and avoid any memory test of our own? (3.3.8)
29. Does sign-out confirm in words that the person is signed out? (heuristic 1)

**Accessibility numbers**
30. Is every text pair ≥ 4.5:1 (large ≥ 3:1) in both light and dark, by computation? (1.4.3)
31. Are field borders, icons and focus rings ≥ 3:1 against their neighbours? (1.4.11)
32. Is every pointer target ≥ 24×24 CSS px, and primary actions ≥ 44 px high? (2.5.8, 2.5.5)
33. Is focus always visible (2 px outline, 3:1) and never hidden under a sticky element? (2.4.7, 2.4.11, 2.4.13)
34. Can every task be done by keyboard alone? (2.1.1)
35. At 320 CSS px wide, does nothing but a table scroll sideways, and at 200 % text nothing clip? (1.4.10, 1.4.4, 1.4.12)
36. Do `lang`, accessible names, roles and states (`aria-current`, `aria-invalid`) exist on every page and control? (3.1.1, 4.1.2)
37. Is motion removed or reduced under `prefers-reduced-motion: reduce`, and does the page follow `prefers-color-scheme`? (§5, §10)

---

## Sources

All read 2026-11-20. "Opened" means the fetch returned the page's content; "search only" means the claim rests on search-result excerpts.

NN/g
- Nielsen, J. "10 Usability Heuristics for User Interface Design". https://www.nngroup.com/articles/ten-usability-heuristics/ (opened)
- Nielsen, J. "Response Times: The 3 Important Limits". https://www.nngroup.com/articles/response-times-3-important-limits/ (opened)
- Whitenton, K. "The Two UX Gulfs: Evaluation and Execution" (2018). https://www.nngroup.com/articles/two-ux-gulfs-evaluation-execution/ (opened)
- Laubheimer, P. "Preventing User Errors: Avoiding Unconscious Slips" (2015). https://www.nngroup.com/articles/slips/ (opened)
- Laubheimer, P. "Preventing User Errors: Avoiding Conscious Mistakes" (2015). https://www.nngroup.com/articles/user-mistakes/ (opened)
- Nielsen, J. "Confirmation Dialogs Can Prevent User Errors" (2018). https://www.nngroup.com/articles/confirmation-dialog/ (opened)
- Sherwin, K. "Placeholders in Form Fields Are Harmful" (2014). https://www.nngroup.com/articles/form-design-placeholders/ (opened)
- Krause, R. "10 Design Guidelines for Reporting Errors in Forms" (2019). https://www.nngroup.com/articles/errors-forms-design-guidelines/ (opened)
- Budiu, R. "Fitts's Law and Its Applications in UX" (2022). https://www.nngroup.com/articles/fitts-law/ (opened)
- Nielsen, J. "Short-Term Memory and Web Usability" (2009). https://www.nngroup.com/articles/short-term-memory-and-web-usability/ (opened)
- Nielsen, J. "Jakob's Law of Internet User Experience" (video, 2017). https://www.nngroup.com/videos/jakobs-law-internet-ux/ (opened)
- Moran, K. "The Aesthetic-Usability Effect" (2024). https://www.nngroup.com/articles/aesthetic-usability-effect/ (opened)
- Harley, A. "Proximity Principle in Visual Design" (2020). https://www.nngroup.com/articles/gestalt-proximity/ (opened)
- Harley, A. "The Principle of Common Region" (2020). https://www.nngroup.com/articles/common-region/ (opened)
- Harley, A. "Similarity Principle in Visual Design" (2020). https://www.nngroup.com/articles/gestalt-similarity/ (opened)
- Flaherty, K. "Evaluate Interface Learnability with Cognitive Walkthroughs" (2022). https://www.nngroup.com/articles/cognitive-walkthroughs/ (opened)
- Moran, K. and Gordon, K. "How to Conduct a Heuristic Evaluation" (2023). https://www.nngroup.com/articles/how-to-conduct-a-heuristic-evaluation/ (opened)
- Kaplan, K. "Designing Empty States in Complex Applications" (2021). https://www.nngroup.com/articles/empty-state-interface-design/ (opened)
- Laubheimer, P. "Breadcrumbs: 11 Design Guidelines" (2018). https://www.nngroup.com/articles/breadcrumbs/ (opened)
- Nielsen, J. "Change the Color of Visited Links" (2004). https://www.nngroup.com/articles/change-the-color-of-visited-links/ (opened)
- Nielsen, J. "Command Links" (2007). https://www.nngroup.com/articles/command-links/ (opened)
- Schade, A. "Mobile Tables" (2017). https://www.nngroup.com/articles/mobile-tables/ (opened)
- Pernice, K. "F-Shaped Pattern of Reading on the Web" (2017). https://www.nngroup.com/articles/f-shaped-pattern-reading-web-content/ (opened)

W3C and MDN
- W3C. Web Content Accessibility Guidelines (WCAG) 2.2, Recommendation, version of 12 December 2024. https://www.w3.org/TR/WCAG22/ (opened, first ~200,000 of ~270,000 characters)
- W3C. Understanding SC 2.5.8 Target Size (Minimum). https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html (opened)
- W3C. Understanding SC 3.3.8 Accessible Authentication (Minimum). https://www.w3.org/WAI/WCAG22/Understanding/accessible-authentication-minimum.html (opened)
- W3C. Understanding SC 1.4.10 Reflow. https://www.w3.org/WAI/WCAG22/Understanding/reflow.html (opened)
- W3C. Understanding SC 2.4.13 Focus Appearance. https://www.w3.org/WAI/WCAG22/Understanding/focus-appearance.html (opened)
- W3C. Understanding SC 1.4.13 Content on Hover or Focus. https://www.w3.org/WAI/WCAG22/Understanding/content-on-hover-or-focus.html (opened)
- MDN. prefers-reduced-motion. https://developer.mozilla.org/en-US/docs/Web/CSS/@media/prefers-reduced-motion (opened)
- MDN. prefers-color-scheme. https://developer.mozilla.org/en-US/docs/Web/CSS/@media/prefers-color-scheme (opened)

GOV.UK
- GOV.UK Design System. Error message. https://design-system.service.gov.uk/components/error-message/ (opened)
- GOV.UK Design System. Button. https://design-system.service.gov.uk/components/button/ (opened)
- GOV.UK Design System. Question pages. https://design-system.service.gov.uk/patterns/question-pages/ (opened)
- GOV.UK Design System. Text input. https://design-system.service.gov.uk/components/text-input/ (opened)
- GOV.UK. A to Z style guide. https://guidance.publishing.service.gov.uk/writing-to-gov-uk-standards/style-guides/a-to-z-style-guide/ (opened)
- GOV.UK. Writing for GOV.UK (reading age 9). https://www.gov.uk/guidance/content-design/writing-for-gov-uk (redirects; search only)

Apple, Google
- Apple. Human Interface Guidelines: Buttons (change log to 16 December 2025). https://developer.apple.com/design/human-interface-guidelines/buttons.md (opened)
- Google. Codelab "Design a dark theme with Material and Figma". https://codelabs.developers.google.com/codelabs/design-material-darktheme/ (opened)
- Google. Material Design 2: Dark theme. https://m2.material.io/design/color/dark-theme (page did not render; search only)

Original papers and authors' pages
- Norman, D. "Signifiers, not affordances", *Interactions* 15(6), 2008. https://jnd.org/signifiers-not-affordances/ (opened)
- Miller, G. A. "The magical number seven, plus or minus two", *Psychological Review* 63, 81–97, 1956. https://psychclassics.yorku.ca/Miller/ (opened)
- Cowan, N. "The magical number 4 in short-term memory", *Behavioral and Brain Sciences* 24, 87–114, 2001. https://pubmed.ncbi.nlm.nih.gov/11515286/ (cookie wall; search only)
- Fitts, P. M. 1954, *Journal of Experimental Psychology* 47(6), 381–391. https://psycnet.apa.org/record/1955-02911-001 (did not load; citation via NN/g)
- Hick, W. E. "On the rate of gain of information", *QJEP* 4(1), 11–26, 1952. https://journals.sagepub.com/doi/10.1080/17470215208416600 (403; search only)
- Kurosu, M. and Kashimura, K. "Apparent usability vs. inherent usability", CHI 1995. https://dl.acm.org/doi/10.1145/223355.223680 (403; via NN/g)
- Kivetz, R., Urminsky, O. and Zheng, Y. "The Goal-Gradient Hypothesis Resurrected", *JMR* 43(1), 2006. https://journals.sagepub.com/doi/10.1509/jmkr.43.1.39 (403)
- Newell, A. and Rosenbloom, P. 1981. https://kilthub.cmu.edu/articles/journal_contribution/Mechanisms_of_skill_acquisition_and_the_law_of_practice/6607196 (search only)
- Doherty, W. J. and Thadani, A. J. "The Economic Value of Rapid Response Time", IBM, 1982. https://www.vm.ibm.com/devpages/jelliott/evrrt1.html (TLS error; search only)
- Postel, J. RFC 761, Transmission Control Protocol, January 1980, §2.10. https://www.rfc-editor.org/rfc/rfc761 (opened)
- Penzo, M. "Label Placement in Forms", UXmatters, 2006. https://www.uxmatters.com/mt/archives/2006/07/label-placement-in-forms.php (opened)
- Hoober, S. "How Do Users Really Hold Mobile Devices?", UXmatters, 2013. https://www.uxmatters.com/mt/archives/2013/02/how-do-users-really-hold-mobile-devices.php (opened)
- Scott, E. "Inline form validation", Baymard Institute, 2024. https://baymard.com/blog/inline-form-validation (opened)

Pointers (secondary)
- lawsofux.com: Hick's law, Doherty threshold, Tesler's law, goal-gradient effect, serial position effect, Von Restorff effect. https://lawsofux.com/ (opened, those six pages)
- IxDF. Gestalt principles. https://ixdf.org/literature/topics/gestalt-principles (opened)

## What could not be verified

- **Original papers behind most "laws"**: Fitts 1954, Hick 1952, Hyman 1953, Cowan 2001, Newell and Rosenbloom 1981, Kurosu and Kashimura 1995, Kivetz et al. 2006, Doherty and Thadani 1982, von Restorff 1933, Hull 1932, and Tesler's own statement. Publisher pages refused automated fetches (403, cookie walls, a TLS error); their content here comes from NN/g, lawsofux.com or search excerpts. Only Miller 1956, Norman 2008 and Postel 1980 were read at their source.
- **Material's dark-theme page** (#121212 and the 15.8:1 target) would not render; the figures are from search excerpts. The 4.5:1-at-24dp rule is likewise search-only. My own computation confirms white on #121212 is 18.73:1.
- **Apple's accessibility page** did not render; only the Buttons page (44×44 pt) was read.
- **GOV.UK's reading age of 9** and the often-quoted **25-word sentence** limit: the first is search-only, the second not found.
- **Nielsen's 0–4 severity scale**: the NN/g page returned 404.
- **Norman's definitions of mapping, feedback, constraints and conceptual models** were not read in his own text in this session; the claims rest on NN/g articles that cite him.
- **WCAG 2.2's first publication date** (5 October 2023), the px equivalent of "large text", the exemption of inactive components from 1.4.3, and 2.4.4/2.4.8 were not read in this session; the rest of the WCAG table was.
- **The "where am I / what can I do / where can I go" framing** and the `hover` media feature were not traced to a source.
- Every fetch returned a model's summary of the page, not the raw text, so wording-level details (exact quotes) may differ from the originals.
