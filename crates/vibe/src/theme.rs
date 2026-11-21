//! The one theme of every page of the platform's own (CONTRACT.md, "How
//! the pages look"): the Dashboard's, the sign-in address's, the pages an
//! Application's address answers in the platform's name, the local run's,
//! and the page the tool itself serves on the loopback address while it
//! signs in. One identity with the website ("a quiet tool on warm paper",
//! `.claude/skills/ui-ux/THEME.md`, the source of every value here).
//!
//! [`STYLE`] is the stylesheet, written once and never built per request.
//! It names no address (no `url(`, `@import` or `@font-face`): the fonts
//! are the reader's own, and the marks are inline `<svg>` ([`LOGO`] and the
//! notices' [`Mark`]s), so a page whose policy is `default-src 'none';
//! style-src 'unsafe-inline'` loads nothing. [`page`] writes the shell every
//! page shares: `<!doctype html>`, `lang="en"`, the viewport and
//! `color-scheme` metas, the one `<style>`, the slim header and the main
//! column or the one centred card.
//!
//! Nothing here escapes: whatever a caller passes in is HTML already, with
//! everything that is not the platform's own text escaped by the caller.

/// The stylesheet: the tokens of THEME.md for light and for dark, the type
/// scale, the shell and the components (card, buttons, fields, notices,
/// rows of people, identifiers, pills, empty states).
pub const STYLE: &str = r#":root{color-scheme:light dark;
--bg:#f6f3ec;--surface:#fffdf9;--surface-2:#efeadf;--ink:#16140f;--ink-2:#57534e;--ink-3:#6b655c;--line:#d6cfbf;--edge:#8a8377;
--accent:#c2410c;--accent-strong:#9a3412;--on-accent:#fff;--accent-soft:#ffedd5;--accent-soft-ink:#9a3412;
--danger:#b91c1c;--danger-soft:#fee2e2;--danger-soft-ink:#991b1b;--ok-soft:#dcfce7;--ok-soft-ink:#166534;--warn-soft:#fef9c3;--warn-soft-ink:#854d0e;
--shadow-1:0 1px 2px rgb(22 20 15/.06),0 1px 1px rgb(22 20 15/.04);--shadow-2:0 8px 24px -6px rgb(22 20 15/.14),0 2px 6px -2px rgb(22 20 15/.08);--card-edge:transparent;
--sans:system-ui,-apple-system,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif;
--serif:"Iowan Old Style","Palatino Linotype",Palatino,"Book Antiqua",Georgia,serif;
--mono:ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace}
@media (prefers-color-scheme:dark){:root{
--bg:#12110e;--surface:#1c1a16;--surface-2:#27251f;--ink:#ece7dc;--ink-2:#b5ad9c;--ink-3:#9a9282;--line:#34312a;--edge:#7d766a;
--accent:#ff7a3d;--accent-strong:#ff9a66;--on-accent:#16140f;--accent-soft:#3a1d0e;--accent-soft-ink:#ffb38a;
--danger:#fca5a5;--danger-soft:#3b1212;--danger-soft-ink:#fecaca;--ok-soft:#0f2a1a;--ok-soft-ink:#bbf7d0;--warn-soft:#2e2607;--warn-soft-ink:#fef08a;
--shadow-1:none;--shadow-2:none;--card-edge:var(--line)}}
*,*::before,*::after{box-sizing:border-box}
html{-webkit-text-size-adjust:100%;text-size-adjust:100%}
body{margin:0;min-height:100vh;background:var(--bg);color:var(--ink);font:400 1rem/1.5rem var(--sans);overflow-wrap:anywhere}
h1,h2,h3,h4,p,ul,ol,form,pre,table{margin:0}
h1{font:600 1.5rem/2rem var(--serif);letter-spacing:-.01em}
h2{font-size:1.25rem;line-height:1.75rem;font-weight:600}
h3{font-size:1.125rem;line-height:1.75rem;font-weight:600}
h4{font-size:1rem;line-height:1.5rem;font-weight:600}
b,strong{font-weight:600}
a{color:var(--accent);text-decoration-thickness:.08em;text-underline-offset:.18em}
a:hover{color:var(--accent-strong)}
:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
code,pre{font-family:var(--mono);font-size:.875em}
code{background:var(--surface-2);padding:.125rem .375rem;border-radius:.375rem}
.over{font-size:.75rem;line-height:1rem;font-weight:600;letter-spacing:.06em;text-transform:uppercase;color:var(--ink-3)}
.soft{color:var(--ink-2)}
.small{font-size:.875rem;line-height:1.25rem}
.vh{position:absolute;width:1px;height:1px;margin:-1px;padding:0;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap;border:0}
.i{width:1.25rem;height:1.25rem;flex:none;fill:none;stroke:currentColor;stroke-width:1.5;stroke-linecap:round;stroke-linejoin:round}
.small .i,.pill .i,.btn.compact .i{width:1rem;height:1rem}
.top{border-bottom:1px solid var(--line)}
.top-in{max-width:56rem;margin:0 auto;padding:.5rem 1rem;display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:.25rem 1rem;min-height:3.75rem}
.brand{display:inline-flex;align-items:center;gap:.5rem;min-height:2.75rem;color:var(--ink);text-decoration:none;font:600 1.125rem/1.75rem var(--serif);letter-spacing:-.01em;white-space:nowrap}
a.brand:hover{color:var(--ink)}
.logo{width:1rem;height:1rem;flex:none;color:var(--accent)}
.place{font:600 .75rem/1rem var(--sans);letter-spacing:.06em;text-transform:uppercase;color:var(--ink-3);padding-left:.5rem;border-left:1px solid var(--line)}
.place.host{font:400 .875rem/1.25rem var(--mono);letter-spacing:0;text-transform:none;white-space:normal;overflow-wrap:anywhere}
.who{display:flex;align-items:center;gap:.5rem 1rem;margin-left:auto;min-width:0}
.who>.btn{flex:none;white-space:nowrap}
.person{display:flex;flex-direction:column;align-items:flex-end;text-align:right;min-width:0;line-height:1.25rem}
.person .name{font-weight:600;font-size:.875rem}
code.id{background:none;padding:0;border-radius:0;font-size:.875rem;line-height:1.25rem;color:var(--ink-3)}
.person code.id{font-size:.75rem;line-height:1rem}
code.id.as-name{font-size:1em;line-height:inherit;font-weight:600;color:var(--ink);overflow-wrap:anywhere}
.person code.id.as-name{font-size:.875rem;line-height:1.25rem;word-break:break-all}
.column{max-width:56rem;margin:0 auto;padding:2rem 1rem 4rem}
.lone{max-width:32rem;margin:0 auto;padding:clamp(1.5rem,10vh,6rem) 1rem 4rem}
.card{background:var(--surface);border:1px solid var(--card-edge);border-radius:.625rem;box-shadow:var(--shadow-1);padding:1rem}
.lone>.card{box-shadow:var(--shadow-2);padding:1.5rem 1.25rem}
.lone>.card>*+*,.lone form>*+*{margin-top:1rem}
.lone h1+*{margin-top:.75rem}
.lone .aside{margin-top:1.5rem;padding-top:1rem;border-top:1px solid var(--line)}
.state{display:flex;align-items:flex-start;gap:.625rem}
.state>.i{width:1.75rem;height:1.75rem;margin-top:.125rem;stroke-width:1.75}
.state.ok>.i{color:var(--ok-soft-ink)}
.state.err>.i{color:var(--danger)}
.state.warn>.i{color:var(--warn-soft-ink)}
.state.plain>.i{color:var(--ink-2)}
.section{margin-top:2.5rem}
.section>h2+*{margin-top:.75rem}
.lead{color:var(--ink-2);max-width:65ch}
.said code{font-size:1em}
.actions{display:flex;flex-wrap:wrap;align-items:center;gap:.5rem .75rem}
.lone .actions{margin-top:1.5rem}
.btn,button{display:inline-flex;align-items:center;justify-content:center;gap:.5rem;min-height:2.75rem;padding:.5rem 1rem;border:1px solid var(--edge);border-radius:.375rem;background:var(--surface);color:var(--ink);font:600 1rem/1.5rem var(--sans);text-decoration:none;text-align:center;cursor:pointer;transition:background-color .12s,border-color .12s,color .12s}
.btn:hover,button:hover{border-color:var(--ink);color:var(--ink)}
.primary,button.primary{background:var(--accent);border-color:var(--accent);color:var(--on-accent)}
.primary:hover,button.primary:hover{background:var(--accent-strong);border-color:var(--accent-strong);color:var(--on-accent)}
.quiet,button.quiet{background:transparent;border-color:transparent;color:var(--accent);padding-inline:.625rem}
.quiet:hover,button.quiet:hover{background:var(--surface-2);border-color:transparent;color:var(--accent-strong);text-decoration:underline}
.quiet.danger,button.quiet.danger{color:var(--danger)}
.quiet.danger:hover,button.quiet.danger:hover{color:var(--danger)}
.compact{min-height:2.25rem;padding-block:.375rem}
.field+.field{margin-top:1.25rem}
label{display:block;font-weight:600;font-size:1rem;line-height:1.5rem}
.help{margin-top:.25rem;font-size:.875rem;line-height:1.25rem;color:var(--ink-2);max-width:65ch}
.control{position:relative;margin-top:.5rem;width:100%;max-width:24rem}
input[type=text],select{display:block;width:100%;min-height:2.75rem;padding:.5rem .75rem;border:1px solid var(--edge);border-radius:.375rem;background:var(--surface);color:var(--ink);font:400 1rem/1.5rem var(--sans)}
select{-webkit-appearance:none;appearance:none;padding-right:2.5rem;cursor:pointer}
.control .chevron{position:absolute;right:.75rem;top:50%;width:1rem;height:1rem;margin-top:-.5rem;color:var(--ink-2);pointer-events:none}
input.code{font-family:var(--mono);letter-spacing:.08em;text-transform:uppercase}
input[aria-invalid=true]{border-color:var(--danger);border-width:2px}
.form-actions{margin-top:1.5rem}
.notice{display:flex;align-items:flex-start;gap:.75rem;padding:.75rem 1rem;border-radius:.375rem;background:var(--accent-soft);color:var(--accent-soft-ink);max-width:65ch}
.field .notice{margin-top:.5rem}
.notice>.ico{flex:none;width:1.25rem;height:1.25rem;margin-top:.125rem}
.notice code{background:none;padding:0;font-size:1em;color:inherit}
.notice strong{color:inherit}
.notice.ok{background:var(--ok-soft);color:var(--ok-soft-ink)}
.notice.error{background:var(--danger-soft);color:var(--danger-soft-ink)}
.notice.warn{background:var(--warn-soft);color:var(--warn-soft-ink)}
.notice.plain{background:var(--surface-2);color:var(--ink)}
.notice a{color:inherit}
.pill{display:inline-flex;align-items:center;gap:.25rem;padding:0 .5rem;border-radius:999px;background:var(--surface-2);color:var(--ink-2);font-size:.75rem;line-height:1.5rem;font-weight:600;white-space:nowrap}
.pill.on{background:var(--accent-soft);color:var(--accent-soft-ink)}
.pill.warn{background:var(--warn-soft);color:var(--warn-soft-ink)}
.pill.err{background:var(--danger-soft);color:var(--danger-soft-ink)}
.pill.ver{font-family:var(--mono);font-weight:400}
.apps{list-style:none;padding:0;display:grid;gap:.75rem}
.group+.group{margin-top:2.5rem}
.group>h2{margin-bottom:.75rem}
.app.card{display:flex;flex-wrap:wrap;align-items:center;gap:.75rem 1.5rem;padding:1rem 1.25rem}
.app .text{flex:1 1 16rem;min-width:0}
.app .text>*+*{margin-top:.25rem}
.app-head{display:flex;flex-wrap:wrap;align-items:center;gap:.25rem .75rem}
.app-head h1{flex:0 1 auto}
.app .desc{color:var(--ink-2)}
a.name{display:inline-block;min-width:1.5rem;color:var(--accent);text-decoration:none}
a.name:hover{text-decoration:underline}
.whole>*+*{margin-top:.75rem}
.whole>.app-head+.desc{margin-top:.5rem}
.whole>.actions{margin-top:1.5rem}
.whole>.section{margin-top:2.5rem}
.address{font-size:.875rem;line-height:1.25rem}
.lockline{display:inline-flex;align-items:flex-start;gap:.5rem;color:var(--ink-2);max-width:65ch}
.lockline>.i{margin-top:.125rem}
.sub{display:flex;flex-wrap:wrap;align-items:baseline;justify-content:space-between;gap:.25rem 1rem;margin-top:1.5rem}
h2+.sub{margin-top:1rem}
.sub+.rows{margin-top:.5rem}
.rows{list-style:none;padding:0;border-top:1px solid var(--line)}
.rows>li{display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:.5rem 1rem;padding:.75rem 0;border-bottom:1px solid var(--line)}
.rows .who-is{display:flex;align-items:flex-start;gap:.75rem;min-width:0;flex:1 1 16rem}
.rows .who-is .lines{min-width:0;overflow-wrap:anywhere}
.rows .who-is>.i{margin-top:.125rem;color:var(--ink-3)}
.rows .who-is .lines>*{display:block}
.rows .who-is .name{font-weight:600}
.rows .who-is .more{font-size:.875rem;line-height:1.25rem;color:var(--ink-2)}
.rows .who-is .more code.id{margin-left:.5rem}
.rows .who-is .lines>.unseen{display:flex;align-items:flex-start;gap:.25rem;color:var(--warn-soft-ink);font-size:.875rem;line-height:1.25rem}
.rows form{flex:none;margin-left:auto}
.after{margin-top:.75rem}
.share{margin-top:1.5rem}
table.res{width:100%;border-collapse:collapse;font-size:.875rem;line-height:1.25rem}
.res th{overflow-wrap:normal;white-space:nowrap;text-align:left;font-size:.75rem;line-height:1rem;font-weight:600;letter-spacing:.06em;text-transform:uppercase;color:var(--ink-3);padding:0 .75rem .5rem 0}
.res td{padding:.625rem .75rem .625rem 0;border-top:1px solid var(--line);vertical-align:top}
.res th:last-child,.res td:last-child{padding-right:0}
.res .it{display:flex;align-items:flex-start;gap:.5rem}
.res .it>.i{margin-top:0}
.res .it code{background:none;padding:0;font-size:1em;color:var(--ink);font-weight:600}
.res .ask{margin-top:.25rem;color:var(--ink-2)}
.res .lv{white-space:nowrap}
.res .enough .it>.i{color:var(--ok-soft-ink)}
.res .lower .it>.i,.res .not-registered .it>.i{color:var(--warn-soft-ink)}
.res .none .it>.i{color:var(--danger)}
.res .unknown .it>.i{color:var(--ink-3)}
.empty{padding:2rem 1.25rem;text-align:center;border:1px dashed var(--edge);border-radius:.625rem;color:var(--ink-2)}
.empty>*+*{margin-top:.5rem}
.empty .first{color:var(--ink);font-weight:600}
.column>h1.page-title{margin-bottom:2rem}
.crumbs{margin:-1rem 0 1rem}
.back{margin-left:-.625rem}
.column>.notice{margin-bottom:1.5rem;max-width:none}
.column>h1+.notice{margin-top:1rem}
.pages .next{margin-left:auto}
.pages{display:flex;flex-wrap:wrap;justify-content:space-between;gap:.75rem;margin-top:2rem}
.users{list-style:none;padding:0;display:grid;gap:.75rem}
.users .card{padding:1rem 1.25rem}
.users form{display:flex;flex-wrap:wrap;align-items:center;gap:.75rem 1.5rem}
.users .text{flex:1 1 16rem;min-width:0}
.users .text>*+*{margin-top:.25rem}
.users .user-head{display:flex;flex-wrap:wrap;align-items:center;gap:.25rem .75rem}
.users .name{font-weight:600;font-size:1.125rem;line-height:1.75rem}
.users .ident{font-size:.875rem;line-height:1.25rem;color:var(--ink-2)}
.users .ident code.id{margin-left:.5rem}
.tags{list-style:none;padding:0;display:flex;flex-wrap:wrap;align-items:center;gap:.375rem .5rem;font-size:.875rem;line-height:1.25rem;color:var(--ink-2)}
.tags>li{display:inline-flex;align-items:center;gap:.25rem}
.tags .i{width:1rem;height:1rem;color:var(--ink-3)}
.tags code{background:none;padding:0;font-size:1em;color:var(--ink)}
.facts{display:grid;grid-template-columns:auto 1fr;gap:.25rem .75rem;margin:0;font-size:.875rem;line-height:1.25rem}
.facts dt{color:var(--ink-2)}
.facts dd{margin:0}
.facts dd code{background:none;padding:0;font-size:1em}
.column>.lead+*,.column>.notice+.lead{margin-top:1.5rem}
.column>h1+.lead,.column>h1+.facts{margin-top:.75rem}
.column>.facts+*{margin-top:1.5rem}
.column>pre.message+*{margin-top:1rem}
.signed{display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:.5rem 1rem}
.signed p{display:flex;flex-wrap:wrap;align-items:center;gap:.25rem .5rem;min-width:0}
.signed .i{width:1rem;height:1rem;color:var(--ink-3)}
.signed p>code.as-name{flex:1 1 0;min-width:0}
.signed .btn{margin-right:-.625rem}
pre.message{white-space:pre-wrap;padding:.75rem 1rem;border-radius:.375rem;background:var(--surface-2);color:var(--ink);font-size:.875rem;line-height:1.25rem}
details>summary{cursor:pointer;color:var(--accent);min-height:1.5rem}
details[open]>summary{margin-bottom:.5rem}
@media (max-width:39.99rem){
.res thead{position:absolute;width:1px;height:1px;margin:-1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap}
.res tr{display:flex;flex-wrap:wrap;column-gap:1rem;border-top:1px solid var(--line);padding:.625rem 0 .625rem 1.75rem}
.res td{border:0;padding:0}
.res td:first-child{flex:1 0 100%;margin-left:-1.75rem}
.res td.lv{color:var(--ink)}
.res td.lv::before{content:attr(data-label) " ";color:var(--ink-2)}
.rows .who-is{flex-basis:auto}
}
@media (min-width:40rem){
h1{font-size:1.875rem;line-height:2.25rem}
.state>.i{width:2rem;height:2rem}
.top-in{padding-inline:1.5rem}
.column{padding:2.5rem 1.5rem 4rem}
.card{padding:1.25rem}
.app.card,.users .card{padding:1.25rem 1.5rem}
.lone>.card{padding:2rem}
}
@media (min-width:64rem){.top-in{padding-inline:2rem}.column{padding-inline:2rem}}
@media (prefers-reduced-motion:reduce){*,*::before,*::after{transition:none!important;animation:none!important}}
"#;

/// The product's mark: the small vermilion triangle of the website.
pub const LOGO: &str = "<svg class=\"logo\" viewBox=\"0 0 16 16\" aria-hidden=\"true\" focusable=\"false\"><path d=\"M1 2h14L8 15z\" fill=\"currentColor\"/></svg>";

/// The chevron drawn over a `<select>` (its own arrow is turned off so that
/// it looks the same in every browser).
pub const CHEVRON: &str = "<svg class=\"chevron\" viewBox=\"0 0 16 16\" aria-hidden=\"true\" focusable=\"false\"><path d=\"M4 6l4 4 4-4\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>";

/// The mark a notice carries beside its words, so that its kind is never
/// said by colour alone.
#[derive(Clone, Copy)]
pub enum Mark {
    /// Something was done.
    Ok,
    /// Something was refused or went wrong.
    Error,
    /// Something to be careful about.
    Warn,
    /// Something to know.
    Info,
}

impl Mark {
    /// The notice's class and its inline `<svg>`.
    pub fn parts(self) -> (&'static str, &'static str) {
        match self {
            Self::Ok => (
                "ok",
                "<svg class=\"ico\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"10\" cy=\"10\" r=\"8.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><path d=\"M6.5 10.25l2.25 2.25 4.75-5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>",
            ),
            Self::Error => (
                "error",
                "<svg class=\"ico\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"10\" cy=\"10\" r=\"8.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><path d=\"M7.25 7.25l5.5 5.5M12.75 7.25l-5.5 5.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\"/></svg>",
            ),
            Self::Warn => (
                "warn",
                "<svg class=\"ico\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><path d=\"M10 2.75l8 14.5H2z\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linejoin=\"round\"/><path d=\"M10 8v4.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\"/><circle cx=\"10\" cy=\"14.75\" r=\"1\" fill=\"currentColor\"/></svg>",
            ),
            Self::Info => (
                "plain",
                "<svg class=\"ico\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"10\" cy=\"10\" r=\"8.25\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><path d=\"M10 9v5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\"/><circle cx=\"10\" cy=\"6.25\" r=\"1\" fill=\"currentColor\"/></svg>",
            ),
        }
    }
}

/// A mark drawn beside words or in their place (rule 2 of the product
/// owner's: an icon before a word). Each is an inline `<svg>` hidden from
/// a screen reader: whoever uses one gives it words, visible or hidden
/// ([`icon`]), so that nothing is said by the drawing alone.
#[derive(Clone, Copy)]
pub enum Icon {
    /// An arrow leaving a box: open it (in its own place).
    Open,
    /// An arrow to the left: back.
    Back,
    /// An arrow to the right: on.
    Forward,
    /// One person.
    Person,
    /// A Group of people.
    Group,
    /// Everyone: a globe.
    Everyone,
    /// A lock: not for you.
    Lock,
    /// A dashed circle: nothing there yet.
    Empty,
    /// A check: enough, done.
    Check,
    /// A triangle with a stroke: careful.
    Warn,
    /// A cross: refused.
    Cross,
    /// A question mark in a circle: not known here.
    Unknown,
    /// Three dots: cannot be told yet.
    Pending,
    /// A clock: later.
    Later,
    /// A window of a terminal: the command-line tool.
    Terminal,
    /// A window: an Application.
    Window,
    /// A door with an arrow leaving it: sign out.
    Leave,
}

impl Icon {
    /// The drawing, 20 by 20, in the current colour.
    pub fn svg(self) -> &'static str {
        match self {
            Self::Open => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><path d=\"M11.5 3.5h5v5M16.5 3.5L9 11M14.5 11.5v4a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1h4\"/></svg>"
            }
            Self::Back => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><path d=\"M16 10H4.5M9 5l-5 5 5 5\"/></svg>"
            }
            Self::Forward => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><path d=\"M4 10h11.5M11 5l5 5-5 5\"/></svg>"
            }
            Self::Person => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"10\" cy=\"6.5\" r=\"3.25\"/><path d=\"M3.5 17c.8-3.3 3.4-5 6.5-5s5.7 1.7 6.5 5\"/></svg>"
            }
            Self::Group => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"7.5\" cy=\"7\" r=\"2.75\"/><path d=\"M2 16.5c.6-2.8 2.7-4.25 5.5-4.25s4.9 1.45 5.5 4.25\"/><circle cx=\"14\" cy=\"6\" r=\"2.25\"/><path d=\"M14.5 10.75c2 .25 3.25 1.5 3.75 3.75\"/></svg>"
            }
            Self::Everyone => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"10\" cy=\"10\" r=\"7.25\"/><path d=\"M2.75 10h14.5M10 2.75c2 2.1 3 4.5 3 7.25s-1 5.15-3 7.25c-2-2.1-3-4.5-3-7.25s1-5.15 3-7.25z\"/></svg>"
            }
            Self::Lock => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><rect x=\"4\" y=\"9\" width=\"12\" height=\"8.5\" rx=\"1.5\"/><path d=\"M7 9V6.5a3 3 0 0 1 6 0V9\"/></svg>"
            }
            Self::Empty => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"10\" cy=\"10\" r=\"6.75\" stroke-dasharray=\"2.6 2.7\"/></svg>"
            }
            Self::Check => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"10\" cy=\"10\" r=\"7.75\"/><path d=\"M6.5 10.25l2.25 2.25 4.75-5\"/></svg>"
            }
            Self::Warn => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><path d=\"M10 2.75l8 14.5H2z\"/><path d=\"M10 8v4.25\"/><circle cx=\"10\" cy=\"14.75\" r=\".6\"/></svg>"
            }
            Self::Cross => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"10\" cy=\"10\" r=\"7.75\"/><path d=\"M7.25 7.25l5.5 5.5M12.75 7.25l-5.5 5.5\"/></svg>"
            }
            Self::Unknown => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"10\" cy=\"10\" r=\"7.75\"/><path d=\"M7.75 7.75a2.25 2.25 0 1 1 3.1 2.1c-.5.25-.85.7-.85 1.25v.4\"/><circle cx=\"10\" cy=\"14\" r=\".6\"/></svg>"
            }
            Self::Pending => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"10\" cy=\"10\" r=\"7.75\"/><circle cx=\"6.5\" cy=\"10\" r=\".6\"/><circle cx=\"10\" cy=\"10\" r=\".6\"/><circle cx=\"13.5\" cy=\"10\" r=\".6\"/></svg>"
            }
            Self::Later => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><circle cx=\"10\" cy=\"10\" r=\"7.75\"/><path d=\"M10 6v4.25l2.75 1.75\"/></svg>"
            }
            Self::Terminal => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><rect x=\"2.75\" y=\"3.75\" width=\"14.5\" height=\"12.5\" rx=\"1.5\"/><path d=\"M6 8l2.5 2L6 12M10.5 12.5H14\"/></svg>"
            }
            Self::Window => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><rect x=\"2.75\" y=\"3.75\" width=\"14.5\" height=\"12.5\" rx=\"1.5\"/><path d=\"M2.75 7.75h14.5\"/></svg>"
            }
            Self::Leave => {
                "<svg class=\"i\" viewBox=\"0 0 20 20\" aria-hidden=\"true\" focusable=\"false\"><path d=\"M8 3.5H4.5a1 1 0 0 0-1 1v11a1 1 0 0 0 1 1H8M12.5 6.5L16 10l-3.5 3.5M16 10H7.5\"/></svg>"
            }
        }
    }
}

/// `icon` and its words for a reader who cannot see it, hidden from sight
/// (`words`, HTML, escaped already; empty for a mark whose words are
/// visible beside it).
pub fn icon(icon: Icon, words: &str) -> String {
    if words.is_empty() {
        icon.svg().to_owned()
    } else {
        format!("{}<span class=\"vh\">{words}</span>", icon.svg())
    }
}

/// A page's one `<h1>` with the mark of its state before it (`mark`'s
/// colour and drawing), so that the state is seen before it is read:
/// `words` (HTML) are the title.
pub fn state_title(mark: Mark, icon: Icon, words: &str) -> String {
    let (class, _) = mark.parts();
    format!(
        "<h1 class=\"state {}\">{}<span>{words}</span></h1>",
        if class == "error" { "err" } else { class },
        icon.svg()
    )
}

/// Which family of pages a page is of: what its header says.
#[derive(Clone, Copy)]
pub enum Place<'a> {
    /// The Dashboard: the mark is a link to its list, and the header's
    /// right holds whom it is for (`who`, HTML).
    Dashboard { who: &'a str },
    /// The sign-in address, and the pages an Application's address answers
    /// in the platform's name: the mark and the name, then which
    /// installation it is (`installation`, its domain, HTML escaped
    /// already) when it is known. No word of a place: the person may be
    /// signing out, or refused an Application, not signing in.
    SignIn { installation: Option<&'a str> },
    /// The local run (`vibe dev`).
    LocalRun,
    /// The page the tool serves on the loopback address while it signs in.
    Tool,
}

/// How a page's content is laid out.
#[derive(Clone, Copy)]
pub enum Layout {
    /// A column for lists and forms, as wide as reads well.
    Column,
    /// One card, centred: a page that asks or says one thing.
    Card,
}

/// A whole page: `title` is the `<title>` (HTML, escaped already), `main`
/// the content (HTML, everything not the platform's own escaped already).
pub fn page(title: &str, place: Place<'_>, layout: Layout, main: &str) -> String {
    let (brand, right) = match place {
        Place::Dashboard { who } => (
            format!(
                "<a class=\"brand\" href=\"/\">{LOGO}<span>Vibe Manager</span><span class=\"place\">Dashboard</span></a>"
            ),
            who,
        ),
        Place::SignIn { installation } => (
            brand_text(installation.map(|at| format!("<span class=\"place host\">{at}</span>"))),
            "",
        ),
        Place::LocalRun => (
            brand_text(Some("<span class=\"place\">Local run</span>".to_owned())),
            "",
        ),
        Place::Tool => (
            brand_text(Some("<span class=\"place\">The tool</span>".to_owned())),
            "",
        ),
    };
    let (open, close) = match layout {
        Layout::Column => ("<main class=\"column\">", "</main>"),
        Layout::Card => ("<main class=\"lone\"><div class=\"card\">", "</div></main>"),
    };
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"light dark\">\
         <title>{title}</title><style>{STYLE}</style></head>\n\
         <body><header class=\"top\"><div class=\"top-in\">{brand}{right}</div></header>\n\
         {open}{main}{close}</body></html>\n"
    )
}

/// The header's mark and name with no link (a page of the sign-in address
/// or of the local run promises its links, and this is none of them), and
/// `place` (HTML) after them.
fn brand_text(place: Option<String>) -> String {
    format!(
        "<span class=\"brand\">{LOGO}<span>Vibe Manager</span>{}</span>",
        place.unwrap_or_default()
    )
}

/// A notice: its mark and `words` (HTML), in a `<p>` with `attributes`
/// (HTML, e.g. `role="status" data-vm-notice="shared"`).
pub fn notice(mark: Mark, attributes: &str, words: &str) -> String {
    let (class, svg) = mark.parts();
    format!("<p class=\"notice {class}\" {attributes}>{svg}<span>{words}</span></p>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_style_names_no_address() {
        let lower = STYLE.to_ascii_lowercase();
        for named in ["url(", "@import", "@font-face", "gradient("] {
            assert!(!lower.contains(named), "the style holds {named}");
        }
    }

    #[test]
    fn a_page_has_one_style_the_metas_lang_and_its_content() {
        let html = page(
            "T",
            Place::SignIn {
                installation: Some("vm.test"),
            },
            Layout::Card,
            "<h1>T</h1>",
        );
        assert_eq!(html.matches("<style>").count(), 1);
        assert!(html.contains("<html lang=\"en\">"));
        assert!(html.contains("<meta name=\"color-scheme\" content=\"light dark\">"));
        assert!(html.contains("<meta name=\"viewport\""));
        assert_eq!(html.matches("<h1").count(), 1);
        assert!(
            !html.contains("<a "),
            "a page of the sign-in address links nowhere by itself"
        );
        assert!(html.contains("vm.test"), "it says which installation it is");
    }

    #[test]
    fn a_state_title_is_one_h1_with_its_mark() {
        let title = state_title(Mark::Error, Icon::Cross, "Not changed");
        assert!(title.starts_with("<h1 class=\"state err\"><svg"));
        assert!(title.ends_with("<span>Not changed</span></h1>"));
        assert!(icon(Icon::Lock, "Locked").contains("<span class=\"vh\">Locked</span>"));
        assert_eq!(icon(Icon::Lock, ""), Icon::Lock.svg());
    }
}
