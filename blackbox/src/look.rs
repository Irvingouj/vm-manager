//! How a page of the platform's own looks, measured in the harness's real
//! browser (CONTRACT.md, "How the pages look"): what a script in the page
//! reads of the computed styles and the boxes ([`measure`]), the focus the
//! Tab key brings to each thing it reaches ([`walk_focus`]), and the
//! arithmetic the contract names on what was read: WCAG 2's relative
//! luminance and contrast ratio ([`Colour`], [`contrast`]), colours with
//! an alpha drawn over what is behind them ([`Colour::over`]), and the
//! directives of a `content-security-policy` ([`directives`]).
//!
//! The page's script only reads: it reports each colour as the browser
//! computes it (`rgb(...)`, `rgba(...)`, or whatever else the browser
//! writes) and also as a 1x1 canvas paints it (four bytes), so that a
//! colour the browser computes in another space (`oklch(...)`,
//! `color(...)`) is read all the same. Nothing of the page is changed but
//! for an element of its own added and removed at once to read the
//! `Canvas` colour.

use std::fmt;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::firefox::{Firefox, TAB};

/// A colour in sRGB, its channels 0 to 255 and its alpha 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colour {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Colour {
    pub const WHITE: Self = Self::rgb(255.0, 255.0, 255.0);
    pub const BLACK: Self = Self::rgb(0.0, 0.0, 0.0);

    pub const fn rgb(r: f64, g: f64, b: f64) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    /// `rgb(r, g, b)`, `rgba(r, g, b, a)` (commas or spaces, an alpha as a
    /// number or a percentage, `/` before it), `transparent`, or
    /// `#rrggbb`; `None` for anything else.
    pub fn parse(css: &str) -> Option<Self> {
        let css = css.trim().to_ascii_lowercase();
        if css == "transparent" {
            return Some(Self {
                a: 0.0,
                ..Self::BLACK
            });
        }
        if let Some(hex) = css.strip_prefix('#') {
            let byte = |at: usize| {
                hex.get(at..at + 2)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                    .map(f64::from)
            };
            if hex.len() != 6 {
                return None;
            }
            return Some(Self::rgb(byte(0)?, byte(2)?, byte(4)?));
        }
        let inner = css
            .strip_prefix("rgba(")
            .or_else(|| css.strip_prefix("rgb("))?
            .strip_suffix(')')?;
        let parts: Vec<&str> = inner
            .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .collect();
        let number = |text: &str| -> Option<f64> {
            match text.strip_suffix('%') {
                Some(percent) => percent.parse::<f64>().ok().map(|p| p / 100.0),
                None => text.parse::<f64>().ok(),
            }
        };
        let channel = |text: &str| -> Option<f64> {
            match text.strip_suffix('%') {
                Some(percent) => percent.parse::<f64>().ok().map(|p| p * 2.55),
                None => text.parse::<f64>().ok(),
            }
        };
        match parts.as_slice() {
            [r, g, b] => Some(Self::rgb(channel(r)?, channel(g)?, channel(b)?)),
            [r, g, b, a] => Some(Self {
                r: channel(r)?,
                g: channel(g)?,
                b: channel(b)?,
                a: number(a)?,
            }),
            _ => None,
        }
    }

    /// A colour as the page's script reports it: `{"css": "<computed>",
    /// "bytes": [r, g, b, a]}`; the computed text when it can be read,
    /// else the bytes a canvas painted.
    pub fn of(reported: &Value) -> Option<Self> {
        if let Some(parsed) = reported["css"].as_str().and_then(Self::parse) {
            return Some(parsed);
        }
        let bytes = reported["bytes"].as_array()?;
        let byte = |at: usize| bytes.get(at).and_then(Value::as_f64);
        Some(Self {
            r: byte(0)?,
            g: byte(1)?,
            b: byte(2)?,
            a: byte(3)? / 255.0,
        })
    }

    /// This colour drawn over `below` (which is opaque, or is itself the
    /// result of such drawing): "source over" compositing.
    pub fn over(self, below: Self) -> Self {
        let a = self.a + below.a * (1.0 - self.a);
        if a <= 0.0 {
            return Self { a: 0.0, ..below };
        }
        let mix = |top: f64, bottom: f64| (top * self.a + bottom * below.a * (1.0 - self.a)) / a;
        Self {
            r: mix(self.r, below.r),
            g: mix(self.g, below.g),
            b: mix(self.b, below.b),
            a,
        }
    }

    /// WCAG 2's relative luminance.
    pub fn luminance(self) -> f64 {
        let linear = |c: f64| {
            let c = c / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// Whether it is this colour, to the nearest byte.
    pub fn is(self, other: Self) -> bool {
        let near = |x: f64, y: f64| (x - y).abs() < 0.5;
        near(self.r, other.r) && near(self.g, other.g) && near(self.b, other.b)
    }
}

impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let byte = |c: f64| c.round().clamp(0.0, 255.0);
        if self.a < 1.0 {
            write!(
                f,
                "rgba({}, {}, {}, {:.2})",
                byte(self.r),
                byte(self.g),
                byte(self.b),
                self.a
            )
        } else {
            write!(
                f,
                "rgb({}, {}, {})",
                byte(self.r),
                byte(self.g),
                byte(self.b)
            )
        }
    }
}

/// WCAG 2's contrast ratio of two colours (1 to 21).
pub fn contrast(one: Colour, other: Colour) -> f64 {
    let (a, b) = (one.luminance(), other.luminance());
    let (light, dark) = if a >= b { (a, b) } else { (b, a) };
    (light + 0.05) / (dark + 0.05)
}

/// The colour behind an element: the backgrounds of a chain of elements
/// (the element first, the root last, as the page's script reports them),
/// each drawn over the ones after it, all over the canvas. `Err` names a
/// background colour that cannot be read.
pub fn backdrop(chain: &Value, canvas: Colour) -> Result<Colour, String> {
    let mut colour = canvas;
    for entry in chain
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .rev()
    {
        let background = Colour::of(&entry["bg"])
            .ok_or_else(|| format!("cannot read the colour {}", entry["bg"]))?;
        colour = background.over(colour);
    }
    Ok(colour)
}

/// The pictures (`background-image` other than `none`) in a chain, each
/// with the element it is on.
pub fn pictures_in(chain: &Value) -> Vec<String> {
    chain
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|e| e["image"].as_str().is_some_and(|i| i != "none"))
        .map(|e| format!("{} on {}", e["image"], e["el"].as_str().unwrap_or("?")))
        .collect()
}

/// The directives of a `content-security-policy`, each its name (lower
/// case) and its values.
pub fn directives(policy: &str) -> Vec<(String, Vec<String>)> {
    policy
        .split(';')
        .filter_map(|d| {
            let mut words = d.split_whitespace();
            let name = words.next()?.to_ascii_lowercase();
            Some((name, words.map(str::to_owned).collect()))
        })
        .collect()
}

/// What is wrong with a page's `content-security-policy` for "One theme,
/// nothing loaded": `default-src 'none'`, `style-src 'unsafe-inline'`, and
/// no other directive that names a source for anything (no other `*-src`).
/// Directives that are not sources (`form-action`, `frame-ancestors`,
/// `base-uri`, ...) are as the sections before say, not looked at here.
pub fn policy_problems(policy: Option<&str>) -> Vec<String> {
    let Some(policy) = policy else {
        return vec!["has no content-security-policy".to_owned()];
    };
    let all = directives(policy);
    let mut problems = Vec::new();
    let values_of = |name: &str| -> Vec<&Vec<String>> {
        all.iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v)
            .collect()
    };
    for (name, wanted) in [("default-src", "'none'"), ("style-src", "'unsafe-inline'")] {
        let found = values_of(name);
        let right = found.len() == 1
            && found
                .first()
                .is_some_and(|v| v.len() == 1 && v.first().is_some_and(|w| w == wanted));
        if !right {
            problems.push(format!(
                "expected `{name} {wanted}` (exactly that), the policy is {policy:?}"
            ));
        }
    }
    for (name, values) in &all {
        if name.ends_with("-src") && name != "default-src" && name != "style-src" {
            problems.push(format!(
                "names another source: `{name} {}`, in {policy:?}",
                values.join(" ")
            ));
        }
    }
    problems
}

/// Helpers every script of this module begins with.
const PRELUDE: &str = r#"
const paint = document.createElement('canvas');
paint.width = 1; paint.height = 1;
const brush = paint.getContext('2d', { willReadFrequently: true });
const col = (css) => {
  let bytes = null;
  if (brush) {
    brush.clearRect(0, 0, 1, 1);
    brush.fillStyle = 'rgba(0, 0, 0, 0)';
    brush.fillStyle = css;
    brush.fillRect(0, 0, 1, 1);
    bytes = Array.from(brush.getImageData(0, 0, 1, 1).data);
  }
  return { css, bytes };
};
const describe = (el) => {
  if (!el || !el.localName) { return String(el); }
  let d = el.localName;
  if (el.id) { d += '#' + el.id; }
  for (const a of Array.from(el.attributes)) {
    if (a.name.startsWith('data-vm-') || ['name', 'type', 'href', 'value', 'for'].includes(a.name)) {
      d += '[' + a.name + (a.value ? '="' + a.value.slice(0, 40) + '"' : '') + ']';
    }
  }
  return d;
};
const chain = (el) => {
  const out = [];
  for (let e = el; e && e.nodeType === 1; e = e.parentElement) {
    const s = getComputedStyle(e);
    out.push({ el: describe(e), bg: col(s.backgroundColor), image: s.backgroundImage });
  }
  return out;
};
const rendered = (el) => {
  const s = getComputedStyle(el);
  if (s.visibility !== 'visible') { return false; }
  const r = el.getBoundingClientRect();
  return r.width > 0 && r.height > 0;
};
const canvasColour = () => {
  const probe = document.createElement('div');
  probe.style.backgroundColor = 'Canvas';
  probe.style.position = 'absolute';
  probe.style.width = '0';
  probe.style.height = '0';
  (document.body || document.documentElement).appendChild(probe);
  const c = getComputedStyle(probe).backgroundColor;
  probe.remove();
  return col(c);
};
const FOCUSABLE = 'a[href], button, input:not([type="hidden" i]), select, textarea';
"#;

/// The measuring script (after [`PRELUDE`]); its answer is described at
/// [`measure`].
const MEASURE: &str = r#"
const out = {};
out.dark = matchMedia('(prefers-color-scheme: dark)').matches;
out.light = matchMedia('(prefers-color-scheme: light)').matches;
out.reduced_motion = matchMedia('(prefers-reduced-motion: reduce)').matches;
out.styles = Array.from(document.querySelectorAll('style')).map((s) => ({
  in_head: s.parentElement === document.head, text: s.textContent }));
out.color_scheme_metas = Array.from(document.querySelectorAll('meta'))
  .filter((m) => (m.getAttribute('name') || '').toLowerCase() === 'color-scheme')
  .map((m) => m.getAttribute('content'));
out.lang = document.documentElement.getAttribute('lang');
out.h1 = document.querySelectorAll('h1').length;
out.canvas = canvasColour();
out.body = { color: col(getComputedStyle(document.body).color), chain: chain(document.body) };
const ACTS = 'label, button, input, select, textarea, option, [data-vm-error]';
out.texts = [];
const textOf = (el, text, colour, extra) => {
  const s = getComputedStyle(el);
  out.texts.push(Object.assign({
    el: describe(el), text: text.replace(/\s+/g, ' ').trim().slice(0, 60),
    color: col(colour || s.color), size: parseFloat(s.fontSize), weight: parseInt(s.fontWeight, 10),
    acts: el.closest(ACTS) !== null, chain: chain(el) }, extra || {}));
};
for (const el of [document.body, ...Array.from(document.body.querySelectorAll('*'))]) {
  if (['script', 'style', 'template', 'noscript'].includes(el.localName)) { continue; }
  if (!rendered(el)) { continue; }
  const own = Array.from(el.childNodes)
    .filter((n) => n.nodeType === 3 && n.textContent.trim() !== '')
    .map((n) => n.textContent).join(' ');
  if (own.trim() !== '' && el.localName !== 'textarea') { textOf(el, own); }
  const type = (el.getAttribute('type') || '').toLowerCase();
  if (el.localName === 'input' && ['submit', 'button', 'reset'].includes(type)) {
    textOf(el, el.value || '(the browser\'s own label)');
  } else if ((el.localName === 'input' && !['checkbox', 'radio', 'hidden', 'image', 'color', 'range', 'file'].includes(type)) || el.localName === 'textarea') {
    if (el.value) { textOf(el, el.value); }
    if (el.getAttribute('placeholder')) {
      textOf(el, el.getAttribute('placeholder'), getComputedStyle(el, '::placeholder').color, { placeholder: true });
    }
  } else if (el.localName === 'select' && el.selectedOptions.length > 0) {
    textOf(el, el.selectedOptions[0].text);
  }
}
out.fields = Array.from(document.querySelectorAll('input:not([type="hidden" i]):not([type="submit" i]):not([type="button" i]):not([type="reset" i]):not([type="image" i]), select, textarea'))
  .filter(rendered).map((el) => {
    const s = getComputedStyle(el);
    const side = (n) => ({ style: s['border' + n + 'Style'], width: parseFloat(s['border' + n + 'Width']), color: col(s['border' + n + 'Color']) });
    return { el: describe(el), sides: [side('Top'), side('Right'), side('Bottom'), side('Left')],
      own: col(s.backgroundColor), around: chain(el.parentElement) };
  });
const box = (el) => { const r = el.getBoundingClientRect(); return { el: describe(el), width: r.width, height: r.height }; };
out.pressables = Array.from(document.querySelectorAll('button, input:not([type="hidden" i]), select, textarea')).filter(rendered).map(box);
out.links = Array.from(document.querySelectorAll('a[href]')).filter(rendered).map((el) => {
  const parent = el.parentElement;
  const in_sentence = parent ? Array.from(parent.childNodes).some((n) => n !== el && n.nodeType === 3 && n.textContent.trim() !== '') : false;
  return Object.assign(box(el), { in_sentence, text: el.textContent.trim().slice(0, 40) });
});
const TEXT_TYPES = ['', 'text', 'email', 'search', 'url', 'tel', 'password', 'number'];
out.forms = Array.from(document.querySelectorAll('form')).map((form) => ({
  form: describe(form),
  submits: Array.from(form.querySelectorAll('button, input[type="submit" i], input[type="image" i]'))
    .filter((b) => b.localName !== 'button' || ['', 'submit'].includes((b.getAttribute('type') || '').toLowerCase()))
    .filter(rendered).map(box),
  text_fields: Array.from(form.querySelectorAll('input, textarea'))
    .filter((f) => f.localName === 'textarea' || TEXT_TYPES.includes((f.getAttribute('type') || '').toLowerCase()))
    .filter(rendered).map(box),
}));
out.moving = [];
const moves = (value) => (value || '0s').split(',').some((d) => d.trim() !== '0s');
for (const el of [document.documentElement, ...Array.from(document.documentElement.querySelectorAll('*'))]) {
  for (const pseudo of [null, '::before', '::after']) {
    const s = getComputedStyle(el, pseudo);
    if (moves(s.transitionDuration) || moves(s.animationDuration)) {
      out.moving.push({ el: describe(el) + (pseudo || ''), transition: s.transitionDuration, animation: s.animationDuration });
    }
  }
}
return out;
"#;

/// Reads the page shown. The answer is an object:
///
/// - `dark`, `light`, `reduced_motion`: what the page's media queries match;
/// - `styles`: each `<style>`, `{in_head, text}`; `color_scheme_metas`: the
///   `content` of each `<meta name="color-scheme">`; `lang`: the root's;
///   `h1`: how many;
/// - `canvas`: the `Canvas` colour as the page's colour scheme gives it;
///   `body`: `{color, chain}`;
/// - `texts`: each piece of text shown: the element (`el`), the `text`, its
///   `color`, `size` (CSS px), `weight`, whether a person reads it to act
///   (`acts`: in a label, a field, a button or a refusal), and the `chain`
///   of backgrounds from it to the root;
/// - `fields`: each field's four border `sides` (`{style, width, color}`),
///   its `own` background and the chain `around` it;
/// - `pressables` (buttons and fields) and `links` (with `in_sentence`):
///   `{el, width, height}`; `forms`: each form's `submits` and
///   `text_fields`, the same;
/// - `moving`: each element (or `::before`, `::after`) with a transition or
///   an animation duration other than `0s`.
///
/// Each colour is `{css, bytes}` ([`Colour::of`]); each element is
/// described as `tag#id[data-vm-...][name][type][href]`.
pub fn measure(firefox: &mut Firefox) -> Value {
    firefox.script(&format!("{PRELUDE}{MEASURE}"), &[])
}

/// Where the focus is after a key, as [`walk_focus`] reads it.
const FOCUSED: &str = r#"
const a = document.activeElement;
if (!a || a === document.body || a === document.documentElement) { return null; }
const s = getComputedStyle(a);
const list = window.__harnessFocusables || [];
return { index: list.indexOf(a), el: describe(a), focus_visible: a.matches(':focus-visible'),
  style: s.outlineStyle, width: parseFloat(s.outlineWidth), color: col(s.outlineColor),
  behind: chain(a.parentElement), canvas: canvasColour() };
"#;

/// What the keyboard reached on a page ([`walk_focus`]).
#[derive(Clone, Debug, Default)]
pub struct FocusWalk {
    /// Whether the page had the focus when the walk began
    /// ([`Firefox::focus_page`]).
    pub page_focused: bool,
    /// Each link, button and field the keyboard should reach: `{el,
    /// radio}` (`radio`: the group of a radio button, which one Tab
    /// reaches as a whole).
    pub wanted: Vec<Value>,
    /// Where each Tab brought the focus: `{index (in wanted, or -1), el,
    /// focus_visible, style, width, color, behind, canvas}`.
    pub steps: Vec<Value>,
}

/// Presses Tab, as a person does, from the start of the page shown until
/// the focus leaves the page or comes back to where it was, at most once
/// more than there are links, buttons and fields (and 5 more); reads the
/// outline of each thing it reaches. Call it on a page just loaded, before
/// anything else has had the focus.
pub fn walk_focus(firefox: &mut Firefox) -> FocusWalk {
    let page_focused = firefox.focus_page();
    let wanted = firefox.script(
        &format!(
            "{PRELUDE}
const want = Array.from(document.querySelectorAll(FOCUSABLE)).filter((el) => rendered(el) && !el.disabled && el.tabIndex >= 0);
window.__harnessFocusables = want;
return want.map((el) => ({{ el: describe(el), radio: (el.localName === 'input' && (el.getAttribute('type') || '').toLowerCase() === 'radio') ? ((el.form ? describe(el.form) : '') + ' ' + el.name) : null }}));"
        ),
        &[],
    );
    let wanted = wanted.as_array().cloned().unwrap_or_default();
    let mut steps: Vec<Value> = Vec::new();
    let script = format!("{PRELUDE}{FOCUSED}");
    for _ in 0..wanted.len() + 5 {
        firefox.press_key(TAB);
        let at = firefox.script(&script, &[]);
        if at.is_null() {
            break;
        }
        let again = steps
            .iter()
            .any(|s| s["el"] == at["el"] && s["index"] == at["index"]);
        if again {
            break;
        }
        steps.push(at);
    }
    FocusWalk {
        page_focused,
        wanted,
        steps,
    }
}

/// Holds the window to `width` CSS pixels as the page sees it, waiting at
/// most 5 seconds for the page to see it so; then whether the document is
/// no wider ([`crate::firefox::no_wider_than`]).
pub fn narrowed(firefox: &mut Firefox, width: u32) -> Result<(), String> {
    firefox.narrow(width);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = crate::firefox::no_wider_than(firefox, u64::from(width));
        let seen = result
            .as_ref()
            .err()
            .is_none_or(|e| !e.contains("window.innerWidth"));
        if seen || Instant::now() >= deadline {
            return result;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
