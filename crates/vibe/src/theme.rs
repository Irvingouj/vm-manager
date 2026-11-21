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
h1,h2,h3,h4,p,ul,ol,form,pre{margin:0}
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
.top{border-bottom:1px solid var(--line)}
.top-in{max-width:56rem;margin:0 auto;padding:.5rem 1rem;display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:.25rem 1rem;min-height:3.75rem}
.brand{display:inline-flex;align-items:center;gap:.5rem;min-height:2.75rem;color:var(--ink);text-decoration:none;font:600 1.125rem/1.75rem var(--serif);letter-spacing:-.01em;white-space:nowrap}
a.brand:hover{color:var(--ink)}
.logo{width:1rem;height:1rem;flex:none;color:var(--accent)}
.place{font:600 .75rem/1rem var(--sans);letter-spacing:.06em;text-transform:uppercase;color:var(--ink-3);padding-left:.5rem;border-left:1px solid var(--line)}
.who{display:flex;align-items:center;gap:.5rem 1rem;margin-left:auto;min-width:0}
.person{display:flex;flex-direction:column;align-items:flex-end;text-align:right;min-width:0;line-height:1.25rem}
.person .name{font-weight:600;font-size:.875rem}
code.id{background:none;padding:0;border-radius:0;font-size:.875rem;line-height:1.25rem;color:var(--ink-3)}
.person code.id{font-size:.75rem;line-height:1rem}
.column{max-width:56rem;margin:0 auto;padding:2rem 1rem 4rem}
.lone{max-width:32rem;margin:0 auto;padding:clamp(1.5rem,10vh,6rem) 1rem 4rem}
.card{background:var(--surface);border:1px solid var(--card-edge);border-radius:.625rem;box-shadow:var(--shadow-1);padding:1rem}
.lone>.card{box-shadow:var(--shadow-2);padding:1.5rem 1.25rem}
.lone>.card>*+*,.lone form>*+*{margin-top:1rem}
.lone h1+*{margin-top:.75rem}
.lone .aside{margin-top:1.5rem;padding-top:1rem;border-top:1px solid var(--line)}
.section{margin-top:3rem}
.section>h2+*{margin-top:1rem}
.lead{color:var(--ink-2);max-width:65ch}
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
.apps{list-style:none;padding:0;display:grid;gap:1rem}
.apps+h2,.group+.group{margin-top:2.5rem}
.group>h2{margin-bottom:.75rem}
.app>*+*{margin-top:.75rem}
.app-head{display:flex;flex-wrap:wrap;align-items:center;gap:.25rem .75rem}
.app-head h1{flex:0 1 auto}
.app .desc{color:var(--ink-2)}
.app .actions{margin-top:1rem;column-gap:1.5rem}
.app .actions .quiet{padding-inline:0}
.app .actions .quiet:hover{background:transparent}
.meta{padding-top:.75rem;border-top:1px solid var(--line)}
.meta+.meta{border-top:0;padding-top:0}
.meta>h2,.meta>h3,.meta>h4{margin-bottom:.375rem}
.inline-people{list-style:none;padding:0;display:flex;flex-wrap:wrap;gap:.25rem 1.25rem}
.inline-people li{font-size:.875rem;line-height:1.25rem}
.whole{display:block}
.whole>*+*{margin-top:1rem}
.whole .meta{margin-top:2.5rem;padding-top:0;border-top:0}
.whole .meta>h2{margin-bottom:.75rem}
.section>p+.rows{margin-top:.75rem}
.whole .inline-people li{font-size:1rem;line-height:1.5rem}
.rows{list-style:none;padding:0;border-top:1px solid var(--line)}
.rows>li{display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:.5rem 1rem;padding:.75rem 0;border-bottom:1px solid var(--line)}
.rows .who-is{min-width:0;flex:1 1 0}
.rows form{flex:none}
.rows .who-is>*{display:block}
.rows .who-is .name{font-weight:600}
.resources{list-style:none;padding:0;display:grid;gap:.5rem}
.resources li{display:flex;flex-wrap:wrap;align-items:baseline;gap:.25rem .5rem;font-size:.875rem;line-height:1.25rem}
.resources .words{flex:1 1 16rem;color:var(--ink-2)}
.st{display:inline-flex;align-items:center;gap:.25rem;padding:0 .5rem;border-radius:999px;font-size:.75rem;line-height:1.5rem;font-weight:600;white-space:nowrap;background:var(--surface-2);color:var(--ink)}
.enough .st{background:var(--ok-soft);color:var(--ok-soft-ink)}
.lower .st,.not-registered .st{background:var(--warn-soft);color:var(--warn-soft-ink)}
.none .st{background:var(--danger-soft);color:var(--danger-soft-ink)}
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
.users{list-style:none;padding:0;display:grid;gap:1rem}
.users .user-head{display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:.25rem .75rem}
.users .ident>*{display:block}
.users .facts{margin-top:.75rem}
.users .name{font-weight:600;font-size:1.125rem;line-height:1.75rem}
.users .chosen{border-color:var(--accent)}
.facts{display:grid;grid-template-columns:auto 1fr;gap:.25rem .75rem;margin:0;font-size:.875rem;line-height:1.25rem}
.facts dt{color:var(--ink-2)}
.facts dd{margin:0}
.users .form-actions{margin-top:1rem}
.facts-line{font-size:.875rem;line-height:1.25rem;color:var(--ink-2)}
.column>.lead+*,.column>.notice+.lead{margin-top:1.5rem}
.column>.lead{margin-bottom:1.5rem}
.column>h1+.lead{margin-top:.5rem}
pre.message{white-space:pre-wrap;padding:.75rem 1rem;border-radius:.375rem;background:var(--surface-2);color:var(--ink);font-size:.875rem;line-height:1.25rem}
@media (min-width:40rem){
h1{font-size:1.875rem;line-height:2.25rem}
.top-in{padding-inline:1.5rem}
.column{padding:2.5rem 1.5rem 4rem}
.card{padding:1.25rem}
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

/// Which family of pages a page is of: what its header says.
#[derive(Clone, Copy)]
pub enum Place<'a> {
    /// The Dashboard: the mark is a link to its list, and the header's
    /// right holds whom it is for (`who`, HTML).
    Dashboard { who: &'a str },
    /// The sign-in address, and the pages an Application's address answers
    /// in the platform's name.
    SignIn,
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
        Place::SignIn => (brand_text("Sign-in"), ""),
        Place::LocalRun => (brand_text("Local run"), ""),
        Place::Tool => (brand_text("The tool"), ""),
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
/// or of the local run promises its links, and this is none of them).
fn brand_text(place: &str) -> String {
    format!(
        "<span class=\"brand\">{LOGO}<span>Vibe Manager</span><span class=\"place\">{place}</span></span>"
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
        let html = page("T", Place::SignIn, Layout::Card, "<h1>T</h1>");
        assert_eq!(html.matches("<style>").count(), 1);
        assert!(html.contains("<html lang=\"en\">"));
        assert!(html.contains("<meta name=\"color-scheme\" content=\"light dark\">"));
        assert!(html.contains("<meta name=\"viewport\""));
        assert_eq!(html.matches("<h1").count(), 1);
        assert!(
            !html.contains("<a "),
            "a page of the sign-in address links nowhere by itself"
        );
    }
}
