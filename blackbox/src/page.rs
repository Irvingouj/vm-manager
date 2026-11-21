//! A small reader of HTML, written for the platform's own pages (the
//! Dashboard's, CONTRACT.md, "The Dashboard"): elements with their
//! attributes (entities decoded), their text, and the elements inside them.
//!
//! It is the harness's own rather than a crate's: the pages are the
//! program's, written to be read, and what a test needs of them is small
//! (find the elements that carry an attribute, read the attribute and the
//! text). A full HTML parser would bring a tree of dependencies for that.
//!
//! What it does: comments, `<!...>` and `<?...>` are skipped; `<script>`,
//! `<style>`, `<textarea>` and `<title>` hold raw text up to their end tag;
//! void elements (`<input>`, `<br>`, ...) and `<x/>` have no children; an
//! end tag closes the nearest open element of its name and everything
//! opened inside it (one with no open element of its name is ignored); a
//! `<li>`, `<p>`, `<option>`, `<tr>`, `<td>`, `<th>`, `<dt>` or `<dd>` closes
//! an open one of the same kind directly around it. Names of elements and
//! attributes are read in lower case; an attribute given twice keeps its
//! first value, as browsers do.

/// The elements that never have children.
const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// The elements whose content is raw text up to their end tag.
const RAW_TEXT: [&str; 4] = ["script", "style", "textarea", "title"];

/// The elements a new one of the same kind closes when it is open directly
/// around it.
const SELF_CLOSING_SIBLINGS: [&str; 8] = ["li", "p", "option", "tr", "td", "th", "dt", "dd"];

#[derive(Clone, Debug)]
enum Node {
    Element {
        name: String,
        attributes: Vec<(String, String)>,
        children: Vec<usize>,
    },
    Text(String),
}

/// A page, read.
#[derive(Clone, Debug)]
pub struct Document {
    /// Node 0 is the document itself, an element named `#document`.
    nodes: Vec<Node>,
}

/// One element of a [`Document`].
#[derive(Clone, Copy, Debug)]
pub struct Element<'a> {
    document: &'a Document,
    at: usize,
}

impl Document {
    /// Reads `html`. Nothing is refused: what is not markup is text.
    pub fn parse(html: &str) -> Self {
        let mut document = Self {
            nodes: vec![Node::Element {
                name: "#document".to_owned(),
                attributes: Vec::new(),
                children: Vec::new(),
            }],
        };
        let mut open: Vec<usize> = vec![0];
        let mut rest = html;
        while !rest.is_empty() {
            let Some(at) = rest.find('<') else {
                document.push_text(&open, &decode_entities(rest));
                break;
            };
            let (text, from) = rest.split_at(at);
            if !text.is_empty() {
                document.push_text(&open, &decode_entities(text));
            }
            rest = document.markup(from, &mut open);
        }
        document
    }

    /// The whole document, as an element.
    pub fn root(&self) -> Element<'_> {
        Element {
            document: self,
            at: 0,
        }
    }

    /// Reads the markup at the start of `from` (which begins with `<`) and
    /// returns what follows it.
    fn markup<'h>(&mut self, from: &'h str, open: &mut Vec<usize>) -> &'h str {
        if let Some(after) = from.strip_prefix("<!--") {
            return after.find("-->").map_or("", |end| tail(after, end + 3));
        }
        if from.starts_with("<!") || from.starts_with("<?") {
            return from.find('>').map_or("", |end| tail(from, end + 1));
        }
        if let Some(after) = from.strip_prefix("</") {
            if after.starts_with(|c: char| c.is_ascii_alphabetic()) {
                let end = after.find('>').unwrap_or(after.len());
                let name: String = head(after, end)
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '/')
                    .collect::<String>()
                    .to_ascii_lowercase();
                self.close(&name, open);
                return after.get(end + 1..).unwrap_or("");
            }
            self.push_text(open, "<");
            return tail(from, 1);
        }
        let after = tail(from, 1);
        if !after.starts_with(|c: char| c.is_ascii_alphabetic()) {
            self.push_text(open, "<");
            return after;
        }
        let (name, attributes, self_closed, rest) = start_tag(after);
        let parent = *open.last().unwrap_or(&0);
        if SELF_CLOSING_SIBLINGS.contains(&name.as_str()) {
            let same_kind = |other: &str| {
                other == name
                    || (matches!(other, "td" | "th") && matches!(name.as_str(), "td" | "th"))
            };
            if self.name_of(parent).is_some_and(same_kind) && open.len() > 1 {
                open.pop();
            }
        }
        let parent = *open.last().unwrap_or(&0);
        let at = self.nodes.len();
        self.nodes.push(Node::Element {
            name: name.clone(),
            attributes,
            children: Vec::new(),
        });
        self.add_child(parent, at);
        if RAW_TEXT.contains(&name.as_str()) && !self_closed {
            let lower = rest.to_ascii_lowercase();
            let end = lower.find(&format!("</{name}")).unwrap_or(rest.len());
            let raw = head(rest, end);
            if !raw.is_empty() {
                let text = if matches!(name.as_str(), "textarea" | "title") {
                    decode_entities(raw)
                } else {
                    raw.to_owned()
                };
                let text_at = self.nodes.len();
                self.nodes.push(Node::Text(text));
                self.add_child(at, text_at);
            }
            let after_end = tail(rest, end);
            return after_end
                .find('>')
                .map_or("", |close| tail(after_end, close + 1));
        }
        if !self_closed && !VOID.contains(&name.as_str()) {
            open.push(at);
        }
        rest
    }

    fn name_of(&self, at: usize) -> Option<&str> {
        match self.nodes.get(at) {
            Some(Node::Element { name, .. }) => Some(name),
            _ => None,
        }
    }

    fn close(&mut self, name: &str, open: &mut Vec<usize>) {
        if let Some(depth) = open
            .iter()
            .rposition(|at| *at != 0 && self.name_of(*at) == Some(name))
        {
            open.truncate(depth);
        }
    }

    fn add_child(&mut self, parent: usize, child: usize) {
        if let Some(Node::Element { children, .. }) = self.nodes.get_mut(parent) {
            children.push(child);
        }
    }

    fn push_text(&mut self, open: &[usize], text: &str) {
        let parent = *open.last().unwrap_or(&0);
        let at = self.nodes.len();
        self.nodes.push(Node::Text(text.to_owned()));
        self.add_child(parent, at);
    }
}

/// `text` from byte `from` on; empty when `from` is not a character's
/// start.
fn tail(text: &str, from: usize) -> &str {
    text.get(from..).unwrap_or("")
}

/// `text` up to byte `end`; empty when `end` is not a character's start.
fn head(text: &str, end: usize) -> &str {
    text.get(..end).unwrap_or("")
}

/// `text` from byte `from` up to byte `end`.
fn mid(text: &str, from: usize, end: usize) -> &str {
    text.get(from..end).unwrap_or("")
}

/// Reads a start tag after its `<`: its name, its attributes, whether it
/// ended with `/>`, and what follows its `>`.
fn start_tag(after: &str) -> (String, Vec<(String, String)>, bool, &str) {
    let name_end = after
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(after.len());
    let name = head(after, name_end).to_ascii_lowercase();
    let mut rest = tail(after, name_end);
    let mut attributes: Vec<(String, String)> = Vec::new();
    let mut self_closed = false;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return (name, attributes, self_closed, rest);
        }
        if let Some(after) = rest.strip_prefix('>') {
            return (name, attributes, self_closed, after);
        }
        if let Some(after) = rest.strip_prefix('/') {
            self_closed = after.trim_start().starts_with('>');
            rest = after;
            continue;
        }
        self_closed = false;
        let attribute_end = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '>' || c == '/')
            .unwrap_or(rest.len())
            .max(1);
        let attribute = head(rest, attribute_end).to_ascii_lowercase();
        rest = tail(rest, attribute_end).trim_start();
        let value = if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            match after.chars().next() {
                Some(quote @ ('"' | '\'')) => {
                    let inside = tail(after, 1);
                    let end = inside.find(quote).unwrap_or(inside.len());
                    rest = inside.get(end + 1..).unwrap_or("");
                    decode_entities(head(inside, end))
                }
                _ => {
                    let end = after
                        .find(|c: char| c.is_whitespace() || c == '>')
                        .unwrap_or(after.len());
                    rest = tail(after, end);
                    decode_entities(head(after, end))
                }
            }
        } else {
            String::new()
        };
        if !attributes.iter().any(|(n, _)| *n == attribute) {
            attributes.push((attribute, value));
        }
    }
}

/// Decodes the character references of HTML that a page of the platform's
/// may use: the five of XML, `&nbsp;`, and numeric ones. Anything else is
/// left as it is.
pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(head(rest, at));
        let from = tail(rest, at);
        let end = from
            .char_indices()
            .take(12)
            .find(|(_, c)| *c == ';')
            .map(|(i, _)| i);
        let decoded = end.and_then(|end| {
            let name = mid(from, 1, end);
            let c = match name {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => name.strip_prefix('#').and_then(|number| {
                    let code = match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => number.parse::<u32>().ok(),
                    };
                    code.and_then(char::from_u32)
                }),
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = tail(from, end + 1);
            }
            None => {
                out.push('&');
                rest = tail(from, 1);
            }
        }
    }
    out.push_str(rest);
    out
}

impl<'a> Element<'a> {
    fn node(&self) -> &'a Node {
        &self.document.nodes[self.at]
    }

    /// The element's name, lower case.
    pub fn name(&self) -> &'a str {
        match self.node() {
            Node::Element { name, .. } => name,
            Node::Text(_) => "",
        }
    }

    /// Every attribute, as (name, value), in order.
    pub fn attributes(&self) -> &'a [(String, String)] {
        match self.node() {
            Node::Element { attributes, .. } => attributes,
            Node::Text(_) => &[],
        }
    }

    /// The value of an attribute (`Some("")` for one without a value).
    pub fn attr(&self, name: &str) -> Option<&'a str> {
        self.attributes()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whether the element has this attribute.
    pub fn has(&self, name: &str) -> bool {
        self.attr(name).is_some()
    }

    fn child_nodes(&self) -> &'a [usize] {
        match self.node() {
            Node::Element { children, .. } => children,
            Node::Text(_) => &[],
        }
    }

    /// The elements directly inside this one.
    pub fn children(&self) -> Vec<Element<'a>> {
        self.child_nodes()
            .iter()
            .filter(|at| matches!(self.document.nodes[**at], Node::Element { .. }))
            .map(|at| Element {
                document: self.document,
                at: *at,
            })
            .collect()
    }

    /// Every element inside this one, at any depth, in document order.
    pub fn descendants(&self) -> Vec<Element<'a>> {
        let mut found = Vec::new();
        let mut stack: Vec<usize> = self.child_nodes().iter().rev().copied().collect();
        while let Some(at) = stack.pop() {
            if let Node::Element { children, .. } = &self.document.nodes[at] {
                found.push(Element {
                    document: self.document,
                    at,
                });
                stack.extend(children.iter().rev());
            }
        }
        found
    }

    /// The elements inside this one that carry this attribute.
    pub fn with_attr(&self, name: &str) -> Vec<Element<'a>> {
        self.descendants()
            .into_iter()
            .filter(|e| e.has(name))
            .collect()
    }

    /// The elements inside this one whose attribute `name` is `value`.
    pub fn with_attr_value(&self, name: &str, value: &str) -> Vec<Element<'a>> {
        self.descendants()
            .into_iter()
            .filter(|e| e.attr(name) == Some(value))
            .collect()
    }

    /// The elements of this name inside this one.
    pub fn named(&self, name: &str) -> Vec<Element<'a>> {
        self.descendants()
            .into_iter()
            .filter(|e| e.name() == name)
            .collect()
    }

    /// The text inside this element, entities decoded, white space
    /// squashed to single spaces and trimmed. The raw text of a `<script>`
    /// or `<style>` is left out, as a person never reads it.
    pub fn text(&self) -> String {
        let mut text = String::new();
        let mut stack: Vec<usize> = vec![self.at];
        while let Some(at) = stack.pop() {
            match &self.document.nodes[at] {
                Node::Text(t) => {
                    text.push_str(t);
                }
                Node::Element { name, children, .. } => {
                    if at != self.at && matches!(name.as_str(), "script" | "style") {
                        continue;
                    }
                    text.push(' ');
                    stack.extend(children.iter().rev());
                }
            }
        }
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The text inside this element as a screen reader says it, near
    /// enough for a test: what is inside an element with
    /// `aria-hidden="true"` is left out; an element with an `aria-label`
    /// that is not empty says its label instead of what is inside it; an
    /// `<img>` says its `alt`. The raw text of a `<script>`, `<style>` or
    /// `<title>` is left out. White space squashed and trimmed.
    pub fn spoken(&self) -> String {
        let mut text = String::new();
        let mut stack: Vec<usize> = self.child_nodes().iter().rev().copied().collect();
        while let Some(at) = stack.pop() {
            match &self.document.nodes[at] {
                Node::Text(t) => text.push_str(t),
                Node::Element { .. } => {
                    let element = Element {
                        document: self.document,
                        at,
                    };
                    if matches!(element.name(), "script" | "style" | "title")
                        || element
                            .attr("aria-hidden")
                            .is_some_and(|h| h.trim().eq_ignore_ascii_case("true"))
                    {
                        continue;
                    }
                    text.push(' ');
                    if let Some(label) = element.attr("aria-label").filter(|l| !l.trim().is_empty())
                    {
                        text.push_str(label);
                    } else if element.name() == "img" {
                        text.push_str(element.attr("alt").unwrap_or_default());
                    } else {
                        stack.extend(element.child_nodes().iter().rev());
                    }
                    text.push(' ');
                }
            }
        }
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The element's name as a screen reader says it, near enough for a
    /// test: its `aria-label` when that is not empty, else what is inside
    /// it as [`Element::spoken`] reads it.
    pub fn accessible_name(&self) -> String {
        match self.attr("aria-label").filter(|l| !l.trim().is_empty()) {
            Some(label) => label.split_whitespace().collect::<Vec<_>>().join(" "),
            None => self.spoken(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_attributes_and_text_are_read() {
        let doc = Document::parse(
            r#"<!doctype html><html><body><ul><li data-vm-application="a&amp;b" class=x>One <b>bold</b><li data-vm-application='c'>Two</ul><p data-x>&lt;script&gt; &#39;q&#x27; &quot;</p></body></html>"#,
        );
        let root = doc.root();
        let apps = root.with_attr("data-vm-application");
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].attr("data-vm-application"), Some("a&b"));
        assert_eq!(apps[0].attr("class"), Some("x"));
        assert_eq!(apps[0].text(), "One bold");
        assert_eq!(apps[1].attr("data-vm-application"), Some("c"));
        assert_eq!(apps[1].text(), "Two");
        let p = root.with_attr("data-x");
        assert_eq!(p[0].attr("data-x"), Some(""));
        assert_eq!(p[0].text(), "<script> 'q' \"");
    }

    #[test]
    fn void_elements_and_self_closed_ones_have_no_children() {
        let doc = Document::parse(
            r#"<form data-vm-form="share"><input type="hidden" name="action" value="share"><input name="value"/><select name="kind"><option value="user">A person<option value="group">A Group</select><button>Share</button></form><p id=after>x</p>"#,
        );
        let form = &doc.root().with_attr("data-vm-form")[0];
        assert_eq!(form.named("input").len(), 2);
        assert_eq!(form.named("option").len(), 2);
        assert_eq!(form.named("button").len(), 1);
        assert!(form.with_attr("id").is_empty());
        assert_eq!(doc.root().with_attr("id").len(), 1);
    }

    #[test]
    fn raw_text_and_comments_are_not_markup() {
        let doc = Document::parse(
            "<div a=1><!-- <span b=2> --><script>if (x < y) { '<span c=3>' }</script><span d=4>t</span></div>",
        );
        let root = doc.root();
        assert!(root.with_attr("b").is_empty());
        assert!(root.with_attr("c").is_empty());
        assert_eq!(root.with_attr("d").len(), 1);
        assert_eq!(root.named("script").len(), 1);
        assert_eq!(root.with_attr("a")[0].text(), "t");
    }

    #[test]
    fn an_end_tag_closes_what_was_opened_inside_its_element() {
        let doc = Document::parse("<div x><span>a<b>b</div><p y>c</p>");
        let root = doc.root();
        assert_eq!(root.with_attr("x")[0].text(), "a b");
        assert_eq!(root.with_attr("y")[0].text(), "c");
        assert!(root.with_attr("x")[0].with_attr("y").is_empty());
    }

    #[test]
    fn what_a_screen_reader_says_leaves_out_the_hidden_and_says_labels() {
        let doc = Document::parse(
            r#"<a x href="/"><svg aria-hidden="true"><path d="M0"/></svg><span>Open</span> <span class="sr">board</span></a><p y><svg role="img" aria-label="Missing access"><path/></svg> Fix <span aria-hidden=true>!</span><img alt="pic"></p><a z aria-label="Open  board">Go</a>"#,
        );
        let root = doc.root();
        assert_eq!(root.with_attr("x")[0].spoken(), "Open board");
        assert_eq!(root.with_attr("y")[0].spoken(), "Missing access Fix pic");
        assert_eq!(root.with_attr("z")[0].accessible_name(), "Open board");
        assert_eq!(root.with_attr("x")[0].accessible_name(), "Open board");
    }

    #[test]
    fn unknown_entities_and_a_lone_ampersand_stay_as_they_are() {
        assert_eq!(decode_entities("a & b &bogus; &#60;"), "a & b &bogus; <");
    }
}
