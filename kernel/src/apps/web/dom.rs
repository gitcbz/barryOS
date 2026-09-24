//! An HTML5 parser: bytes in, a document tree out.
//!
//! This is not the specification's algorithm.  It is the specification's
//! *result* for the markup that real pages contain — which is a much smaller
//! thing, and the part worth writing out is what it deliberately does not do:
//!
//!   * no foster parenting, so a table's stray text stays where it was written
//!     rather than being moved above the table;
//!   * no adoption agency, so mis-nested formatting elements are closed where
//!     they are rather than rearranged;
//!   * no scripting flag, no `<template>` contents, no foreign content.
//!
//! What it does do is the part that decides whether a page reads correctly:
//! implicit `<html>`, `<head>` and `<body>`, the auto-closing rules that make
//! `<li>` and `<p>` and `<td>` work without end tags, void elements, raw text
//! elements whose contents are not markup, and character references.
//!
//! Those are not details.  A parser that treats `<li>` as a container without
//! the close rule produces a single list item containing the whole document,
//! because that is what the markup actually says.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::format;

/// A node in the tree.  Children and parents are indices, so the tree is one
/// allocation with no pointers to invalidate when it grows.
#[derive(Debug)]
pub struct NodeData {
    pub kind: Kind,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

#[derive(Debug)]
pub enum Kind {
    /// The document itself, which has the `<html>` element as its child.
    Document,
    Element { name: String, attrs: Vec<(String, String)> },
    Text(String),
    Comment,
}

pub struct Dom {
    pub nodes: Vec<NodeData>,
    pub root: usize,
}

/// Elements with no content and no end tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link",
    "meta", "param", "source", "track", "wbr",
];

/// Elements whose contents are text, not markup — so a `<` inside them is a
/// character.  Getting this wrong is how a page's JavaScript becomes visible
/// prose: `<script>if (a < b)` would otherwise open a tag at the `<`.
const RAW_TEXT: &[&str] = &["script", "style", "textarea", "title"];

/// Elements whose contents never render at all.
pub fn is_invisible(name: &str) -> bool {
    matches!(name, "script" | "style" | "head" | "meta" | "link" | "base"
                 | "template" | "iframe" | "object" | "embed"
                 | "canvas" | "svg" | "map" | "area" | "param" | "source"
                 | "track" | "col" | "colgroup")
}

/// Block-level elements, as far as this renderer is concerned.  Everything not
/// here flows inline.
pub fn is_block(name: &str) -> bool {
    matches!(name, "html" | "body" | "div" | "p" | "h1" | "h2" | "h3" | "h4"
                 | "h5" | "h6" | "ul" | "ol" | "li" | "dl" | "dt" | "dd"
                 | "blockquote" | "pre" | "table" | "thead" | "tbody" | "tfoot"
                 | "tr" | "td" | "th" | "caption" | "form" | "fieldset"
                 | "legend" | "article" | "section" | "nav" | "aside"
                 | "header" | "footer" | "main" | "figure" | "figcaption"
                 | "address" | "hr" | "details" | "summary" | "dialog")
}

/// Which elements a start tag closes, when it is still open.
///
/// The rule is per-element rather than "close anything unclosed": `<li>`
/// closes a `<li>` but not a `<div>` around it, and treating every unclosed
/// element as interruptible is how a `<div>` in a list ends up as a sibling of
/// the list.
fn closes_what(name: &str) -> &'static [&'static str] {
    match name {
        "p" => &["p"],
        "li" => &["li"],
        "dt" | "dd" => &["dt", "dd"],
        "td" | "th" => &["td", "th"],
        "tr" => &["td", "th", "tr"],
        "option" => &["option"],
        "optgroup" => &["option", "optgroup"],
        // A heading or a paragraph ends an open paragraph.
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => &["p"],
        "tbody" | "tfoot" => &["td", "th", "tr", "thead", "tbody"],
        _ => &[],
    }
}

/// Elements that belong in `<head>`.  Anything else moves the insertion point
/// into `<body>`, which is how a page with no `<body>` tag gets one.
fn is_head_content(name: &str) -> bool {
    matches!(name, "title" | "meta" | "link" | "style" | "base" | "script"
                 | "noscript" | "template")
}

/// Elements inside which certain children are not allowed by HTML, and which
/// a start tag therefore closes.  `<h1>` inside `<p>` is a paragraph end.
fn implied_end_for(name: &str, open: &str) -> bool {
    matches!(open, "p") && matches!(name, "div" | "table" | "ul" | "ol"
        | "blockquote" | "pre" | "form" | "hr" | "h1" | "h2" | "h3" | "h4"
        | "h5" | "h6" | "section" | "article" | "aside" | "nav" | "header"
        | "footer" | "address" | "figure")
}

struct Parser<'a> {
    src: &'a [u8],
    at: usize,
    dom: Dom,
    /// Indices of the elements currently open, outermost first.
    stack: Vec<usize>,
    /// The implied `<head>` and `<body>`, so a start tag for either can find
    /// the one that already exists instead of making a second.
    head: usize,
    body: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a [u8]) -> Self {
        let mut dom = Dom { nodes: Vec::new(), root: 0 };
        dom.nodes.push(NodeData {
            kind: Kind::Document,
            parent: None,
            children: Vec::new(),
        });
        // The skeleton every document has, whether or not it says so.  Making
        // it up front is what lets everything below assume the same shape: a
        // page that omits `<body>` entirely still has one to append to.
        let html = push_under(&mut dom, 0, Kind::Element {
            name: "html".into(), attrs: Vec::new(),
        });
        let head = push_under(&mut dom, html, Kind::Element {
            name: "head".into(), attrs: Vec::new(),
        });
        let body = push_under(&mut dom, html, Kind::Element {
            name: "body".into(), attrs: Vec::new(),
        });
        Parser { src, at: 0, dom, stack: alloc::vec![html, head], head, body }
    }

    /// Is the insertion point still inside `<head>`?
    fn in_head(&self) -> bool {
        self.stack.last() == Some(&self.head)
    }

    /// Leave `<head>` for `<body>`, which is what any body-ish content does.
    fn enter_body(&mut self) {
        if let Some(pos) = self.stack.iter().position(|&x| x == self.head) {
            self.stack.truncate(pos);
        }
        if !self.stack.contains(&self.body) {
            self.stack.push(self.body);
        }
    }

    fn push(&mut self, kind: Kind) -> usize {
        let parent = self.stack.last().copied().unwrap_or(0);
        push_under(&mut self.dom, parent, kind)
    }

    fn current_tag(&self) -> Option<&str> {
        let id = *self.stack.last()?;
        match &self.dom.nodes[id].kind {
            Kind::Element { name, .. } => Some(name.as_str()),
            _ => None,
        }
    }

    /// Close the nearest open element with this name, or nothing if it is not
    /// open.  An end tag for something never opened is ignored, as the
    /// specification says.
    fn close_element(&mut self, name: &str) {
        // `</head>` is how a document moves into its body, and `</body>` and
        // `</html>` end the content rather than closing a node that was pushed.
        if name == "head" {
            self.enter_body();
            return;
        }
        if name == "body" || name == "html" {
            self.stack.truncate(1);
            return;
        }
        let Some(pos) = self.stack.iter().rposition(|&id| {
            matches!(&self.dom.nodes[id].kind, Kind::Element { name: n, .. } if n == name)
        }) else {
            return;
        };
        self.stack.truncate(pos);
    }

    fn close_tags(&mut self, names: &[&str]) {
        while let Some(open) = self.current_tag() {
            if names.contains(&open) {
                self.stack.pop();
            } else {
                break;
            }
        }
    }

    fn text(&mut self, s: String) {
        if s.is_empty() {
            return;
        }
        // Text that is not whitespace ends the head.  This is what puts the
        // content of a fragment with no tags at all — the whole argument of
        // `element.innerHTML = "hello"` — into the body rather than into a
        // head that never renders.
        if self.in_head() && !s.trim().is_empty() {
            self.enter_body();
        }
        // Adjacent text becomes one node: a raw text element's content is
        // found by looking for the end tag, and a page with a `<` in a script
        // would otherwise produce hundreds of one-character nodes.
        let parent = self.stack.last().copied().unwrap_or(0);
        if let Some(&last) = self.dom.nodes[parent].children.last() {
            if let Kind::Text(t) = &mut self.dom.nodes[last].kind {
                t.push_str(&s);
                return;
            }
        }
        self.push(Kind::Text(s));
    }

    fn parse(&mut self) {
        while self.at < self.src.len() {
            let c = self.src[self.at];
            if c != b'<' {
                let start = self.at;
                while self.at < self.src.len() && self.src[self.at] != b'<' {
                    self.at += 1;
                }
                let raw = &self.src[start..self.at];
                let decoded = decode_entities(raw);
                self.text(decoded);
                continue;
            }

            // Markup.  What follows the `<` decides which kind.
            if self.src[self.at..].starts_with(b"<!--") {
                self.skip_comment();
                continue;
            }
            if self.starts_with_ci(b"<!doctype") {
                // A doctype decides the parsing mode, and this parser has only
                // one mode; it has no effect on what is displayed.
                while self.at < self.src.len() && self.src[self.at] != b'>' {
                    self.at += 1;
                }
                self.at = (self.at + 1).min(self.src.len());
                continue;
            }
            if self.src[self.at..].starts_with(b"<!") {
                // A declaration we do not model; skip to `>`.
                while self.at < self.src.len() && self.src[self.at] != b'>' {
                    self.at += 1;
                }
                self.at = (self.at + 1).min(self.src.len());
                continue;
            }
            if self.src[self.at..].starts_with(b"</") {
                self.at += 2;
                let name = self.read_name();
                self.skip_to_gt();
                if !name.is_empty() {
                    self.close_element(&name);
                }
                continue;
            }
            if self.src[self.at..].starts_with(b"<?") {
                // A bogus comment, per the specification.
                while self.at < self.src.len() && self.src[self.at] != b'>' {
                    self.at += 1;
                }
                self.at = (self.at + 1).min(self.src.len());
                continue;
            }
            self.start_tag();
        }
    }

    fn starts_with_ci(&self, want: &[u8]) -> bool {
        let end = (self.at + want.len()).min(self.src.len());
        let have = &self.src[self.at..end];
        have.len() == want.len()
            && have.iter().zip(want).all(|(a, b)| a.to_ascii_lowercase() == *b)
    }

    fn skip_comment(&mut self) {
        self.at += 4;
        while self.at < self.src.len() {
            if self.src[self.at..].starts_with(b"-->") {
                self.at += 3;
                return;
            }
            self.at += 1;
        }
    }

    fn skip_to_gt(&mut self) {
        while self.at < self.src.len() && self.src[self.at] != b'>' {
            self.at += 1;
        }
        self.at = (self.at + 1).min(self.src.len());
    }

    /// A tag name: letters, digits and the few characters that appear in the
    /// ones nobody expects.
    fn read_name(&mut self) -> String {
        let start = self.at;
        while self.at < self.src.len() {
            let c = self.src[self.at];
            if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b':' {
                self.at += 1;
            } else {
                break;
            }
        }
        String::from_utf8_lossy(&self.src[start..self.at])
            .to_ascii_lowercase()
    }

    fn start_tag(&mut self) {
        self.at += 1;                       // the `<`
        let name = self.read_name();
        if name.is_empty() {
            // A stray `<` is text.  The specification says so too.
            self.at = self.at.saturating_sub(1);
            self.text("<".to_string());
            self.at += 1;
            return;
        }

        let attrs = self.read_attributes();
        let self_closing = self.take_self_closing();

        // Raw text elements swallow everything up to their own end tag, so
        // this has to happen before anything is done with the name.
        if RAW_TEXT.contains(&name.as_str()) {
            let content = self.read_raw_text(&name);
            let attrs = if name == "style" || name == "script" {
                attrs
            } else {
                attrs
            };
            self.insert(&name, attrs, false);
            let content = if name == "title" || name == "textarea" {
                decode_entities(content.as_bytes())
            } else {
                content
            };
            self.text(content);
            if let Some(open) = self.current_tag() {
                if open == name {
                    self.stack.pop();
                }
            }
            return;
        }

        self.insert(&name, attrs, self_closing);
    }

    /// Put an element where it belongs, applying the close rules first.
    fn insert(&mut self, name: &str, attrs: Vec<(String, String)>, self_closing: bool) {
        // The skeleton already exists, so these are markers rather than
        // elements: `<head>` selects the head, `<body>` and everything after
        // it select the body.
        if name == "html" {
            self.stack.truncate(1);
            return;
        }
        if name == "head" {
            self.stack.truncate(1);
            self.stack.push(self.head);
            return;
        }
        if name == "body" {
            self.stack.truncate(1);
            self.stack.push(self.body);
            return;
        }
        if self.in_head() && !is_head_content(name) {
            self.enter_body();
        }

        self.close_tags(closes_what(name));
        if let Some(open) = self.current_tag() {
            if implied_end_for(name, open) {
                self.stack.pop();
            }
        }

        let id = self.push(Kind::Element { name: name.to_string(), attrs });
        if !self_closing && !VOID.contains(&name) {
            self.stack.push(id);
        }
    }

    /// Consume the end of a start tag.  Returns whether it closed itself.
    ///
    /// Only `/>` does.  Returning true for a plain `>` as well — which is what
    /// this did — leaves *no* element open: every start tag becomes a
    /// self-closing one, every text node becomes a sibling of the element it
    /// was written inside, and a page reads as its content in an order that
    /// has nothing to do with its structure.
    fn take_self_closing(&mut self) -> bool {
        let save = self.at;
        while self.at < self.src.len() && self.src[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
        if self.src[self.at..].starts_with(b"/>") {
            self.at += 2;
            return true;
        }
        if self.src[self.at..].starts_with(b">") {
            self.at += 1;
            return false;
        }
        // Malformed: nothing sensible to do but resume from where we were and
        // let the next iteration treat the `>` as text.
        self.at = save;
        self.skip_to_gt();
        false
    }

    fn read_attributes(&mut self) -> Vec<(String, String)> {
        let mut attrs = Vec::new();
        loop {
            while self.at < self.src.len() && self.src[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            if self.at >= self.src.len() || self.src[self.at] == b'>' {
                break;
            }
            if self.src[self.at..].starts_with(b"/>") {
                break;
            }
            let start = self.at;
            while self.at < self.src.len() {
                let c = self.src[self.at];
                if c.is_ascii_whitespace() || c == b'=' || c == b'>' || c == b'/' {
                    break;
                }
                self.at += 1;
            }
            if self.at == start {
                self.at += 1;               // a stray character
                continue;
            }
            let name = String::from_utf8_lossy(&self.src[start..self.at])
                .to_ascii_lowercase();
            let mut value = String::new();
            let save = self.at;
            while self.at < self.src.len() && self.src[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            if self.at < self.src.len() && self.src[self.at] == b'=' {
                self.at += 1;
                while self.at < self.src.len() && self.src[self.at].is_ascii_whitespace() {
                    self.at += 1;
                }
                if self.at < self.src.len() && (self.src[self.at] == b'"'
                    || self.src[self.at] == b'\'')
                {
                    let quote = self.src[self.at];
                    self.at += 1;
                    let vs = self.at;
                    while self.at < self.src.len() && self.src[self.at] != quote {
                        self.at += 1;
                    }
                    value = decode_entities(&self.src[vs..self.at]);
                    if self.at < self.src.len() {
                        self.at += 1;
                    }
                } else {
                    let vs = self.at;
                    while self.at < self.src.len() {
                        let c = self.src[self.at];
                        if c.is_ascii_whitespace() || c == b'>' {
                            break;
                        }
                        self.at += 1;
                    }
                    value = decode_entities(&self.src[vs..self.at]);
                }
            } else {
                self.at = save;
            }
            if !attrs.iter().any(|(n, _): &(String, String)| *n == name) {
                attrs.push((name, value));
            }
        }
        attrs
    }

    /// Everything up to the matching end tag, verbatim.
    fn read_raw_text(&mut self, name: &str) -> String {
        let close = format!("</{}", name);
        let start = self.at;
        while self.at + close.len() <= self.src.len() {
            let slice = &self.src[self.at..self.at + close.len()];
            if slice.eq_ignore_ascii_case(close.as_bytes()) {
                break;
            }
            self.at += 1;
        }
        let content = String::from_utf8_lossy(&self.src[start..self.at]).to_string();
        // Consume the end tag.
        if self.at < self.src.len() {
            while self.at < self.src.len() && self.src[self.at] != b'>' {
                self.at += 1;
            }
            self.at = (self.at + 1).min(self.src.len());
        }
        // The element itself was never pushed, so the stack is untouched.
        content
    }
}

/// Named character references worth having.
///
/// Not the full list — 2231 entries, most of them mathematical symbols no page
/// uses.  These are the ones that appear in real text, where getting them
/// wrong leaves `&amp;` visible on the page.
/// Append a node under `parent` and return its index.
pub fn push_under(dom: &mut Dom, parent: usize, kind: Kind) -> usize {
    let me = dom.nodes.len();
    dom.nodes.push(NodeData { kind, parent: Some(parent), children: Vec::new() });
    dom.nodes[parent].children.push(me);
    me
}

// ---------------------------------------------------------------------------
//  Mutation, for scripts
// ---------------------------------------------------------------------------

pub fn push_element(dom: &mut Dom, parent: usize, tag: &str, attrs: Vec<(String, String)>) -> usize {
    push_under(dom, parent, Kind::Element { name: tag.to_string(), attrs })
}

pub fn push_text(dom: &mut Dom, parent: usize, text: &str) -> usize {
    push_under(dom, parent, Kind::Text(text.to_string()))
}

/// Replace an element's children with a parsed fragment.
pub fn set_inner_html(dom: &mut Dom, id: usize, html: &str) {
    detach_children(dom, id);
    append_html(dom, id, html);
}

/// Replace an element's children with one text node.
pub fn set_text_content(dom: &mut Dom, id: usize, text: &str) {
    detach_children(dom, id);
    if !text.is_empty() {
        push_text(dom, id, text);
    }
}

fn detach_children(dom: &mut Dom, id: usize) {
    let old: Vec<usize> = dom.nodes[id].children.drain(..).collect();
    // The children are left in the arena rather than removed: an index held by
    // a script that still refers to one of them stays valid, which is what a
    // detached node is.
    for c in old {
        dom.nodes[c].parent = None;
    }
}

/// Parse `html` and append what it contains as children of `parent`.
///
/// Parsed as a document and then unwrapped, because that is the only parser
/// there is and a fragment is a document with its context already decided —
/// here, by where it is being inserted.  The body's children are what a
/// fragment means, since the parser puts anything it cannot place there.
pub fn append_html(dom: &mut Dom, parent: usize, html: &str) {
    let parsed = Dom::parse(html.as_bytes());
    let Some(body) = parsed.by_tag("body").first().copied() else { return };
    let children: Vec<usize> = parsed.nodes[body].children.clone();
    for c in children {
        copy_subtree(&parsed, c, dom, parent);
    }
}

/// Deep-copy a node and everything under it.
fn copy_subtree(from: &Dom, from_id: usize, to: &mut Dom, to_parent: usize) -> usize {
    let kind = match &from.nodes[from_id].kind {
        Kind::Text(t) => Kind::Text(t.clone()),
        Kind::Comment => Kind::Comment,
        Kind::Element { name, attrs } => Kind::Element {
            name: name.clone(),
            attrs: attrs.clone(),
        },
        Kind::Document => Kind::Comment,
    };
    let copy = push_under(to, to_parent, kind);
    let children: Vec<usize> = from.nodes[from_id].children.clone();
    for c in children {
        copy_subtree(from, c, to, copy);
    }
    copy
}

pub fn set_attr(dom: &mut Dom, id: usize, name: &str, value: &str) {
    if let Kind::Element { attrs, .. } = &mut dom.nodes[id].kind {
        for (k, v) in attrs.iter_mut() {
            if k == name {
                *v = value.to_string();
                return;
            }
        }
        attrs.push((name.to_string(), value.to_string()));
    }
}

pub fn remove_attr(dom: &mut Dom, id: usize, name: &str) {
    if let Kind::Element { attrs, .. } = &mut dom.nodes[id].kind {
        attrs.retain(|(k, _)| k != name);
    }
}

/// Take a node out of the tree, leaving it in the arena.
///
/// Detached rather than deleted: a script that moved an element still holds a
/// reference to it, and an index into the arena stays valid for as long as the
/// document does.  The alternative — compacting the arena — invalidates every
/// wrapper a script is holding.
pub fn detach(dom: &mut Dom, id: usize) {
    let Some(parent) = dom.nodes[id].parent else { return };
    dom.nodes[parent].children.retain(|&c| c != id);
    dom.nodes[id].parent = None;
}

/// Put a node under `parent`, at the end.
pub fn attach(dom: &mut Dom, id: usize, parent: usize) {
    if id == parent || is_ancestor(dom, parent, id) {
        return;                       // moving a node inside itself is not a move
    }
    detach(dom, id);
    dom.nodes[id].parent = Some(parent);
    dom.nodes[parent].children.push(id);
}

/// Put a node under `parent`, before `before` (or at the end if that is None).
pub fn insert_at(dom: &mut Dom, id: usize, parent: usize, before: Option<usize>) {
    if id == parent || is_ancestor(dom, parent, id) {
        return;
    }
    detach(dom, id);
    dom.nodes[id].parent = Some(parent);
    match before.and_then(|b| dom.nodes[parent].children.iter().position(|&c| c == b)) {
        Some(at) => dom.nodes[parent].children.insert(at, id),
        None => dom.nodes[parent].children.push(id),
    }
}

fn is_ancestor(dom: &Dom, maybe_ancestor: usize, id: usize) -> bool {
    let mut cur = dom.nodes[id].parent;
    while let Some(p) = cur {
        if p == maybe_ancestor {
            return true;
        }
        cur = dom.nodes[p].parent;
    }
    false
}

/// Parse `html` and attach everything it contains under `parent`.
pub fn append_many_html(dom: &mut Dom, parent: usize, html: &str) {
    append_html(dom, parent, html);
}

/// A deep copy of a node and its subtree, attached under `parent`.
pub fn clone_into(dom: &mut Dom, id: usize, parent: usize) -> usize {
    let kind = match &dom.nodes[id].kind {
        Kind::Text(t) => Kind::Text(t.clone()),
        Kind::Comment => Kind::Comment,
        Kind::Element { name, attrs } => Kind::Element {
            name: name.clone(),
            attrs: attrs.clone(),
        },
        Kind::Document => Kind::Comment,
    };
    let children: Vec<usize> = dom.nodes[id].children.clone();
    let copy = push_under(dom, parent, kind);
    for c in children {
        clone_into(dom, c, copy);
    }
    copy
}

/// Where a child sits among its siblings.
pub fn index_of_child(dom: &Dom, parent: usize, id: usize) -> Option<usize> {
    dom.nodes[parent].children.iter().position(|&c| c == id)
}

/// Move a child to a given position among its siblings.
pub fn move_child_to(dom: &mut Dom, parent: usize, id: usize, index: usize) {
    let kids = &mut dom.nodes[parent].children;
    let Some(pos) = kids.iter().position(|&c| c == id) else { return };
    let v = kids.remove(pos);
    let index = index.min(kids.len());
    kids.insert(index, v);
}



fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&', "lt" => '<', "gt" => '>', "quot" => '"', "apos" => '\'',
        "nbsp" | "NonBreakingSpace" => '\u{a0}',
        "copy" => '©', "reg" => '®', "trade" => '™',
        "hellip" => '…', "mdash" => '—', "ndash" => '–',
        "lsquo" => '\u{2018}', "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}', "rdquo" => '\u{201d}',
        "laquo" => '«', "raquo" => '»', "middot" => '·', "bull" => '•',
        "times" => '×', "divide" => '÷', "plusmn" => '±', "deg" => '°',
        "frac12" => '½', "frac14" => '¼', "frac34" => '¾',
        "sup1" => '¹', "sup2" => '²', "sup3" => '³',
        "micro" => 'µ', "para" => '¶', "sect" => '§', "dagger" => '†',
        "euro" => '€', "pound" => '£', "yen" => '¥', "cent" => '¢',
        "curren" => '¤', "infinity" => '∞', "ne" => '≠', "le" => '≤',
        "ge" => '≥', "larr" => '←', "rarr" => '→', "uarr" => '↑', "darr" => '↓',
        "harr" => '↔', "rarr2" => '⇒', "prime" => '′', "Prime" => '″',
        "alpha" => 'α', "beta" => 'β', "gamma" => 'γ', "delta" => 'δ',
        "epsilon" => 'ε', "theta" => 'θ', "lambda" => 'λ', "mu" => 'μ',
        "pi" => 'π', "sigma" => 'σ', "phi" => 'φ', "omega" => 'ω',
        "Alpha" => 'Α', "Beta" => 'Β', "Gamma" => 'Γ', "Delta" => 'Δ',
        "Theta" => 'Θ', "Lambda" => 'Λ', "Pi" => 'Π', "Sigma" => 'Σ',
        "Phi" => 'Φ', "Omega" => 'Ω',
        "OElig" => 'Œ', "oelig" => 'œ', "scaron" => 'š', "fnof" => 'ƒ',
        "spades" => '♠', "clubs" => '♣', "hearts" => '♥', "diams" => '♦',
        "loz" => '◊', "star" => '☆', "check" => '✓', "cross" => '✗',
        _ => return None,
    })
}

/// Expand `&...;` references.  A `&` that does not begin one is left alone,
/// which is what a browser does and what makes a query string survive.
pub fn decode_entities(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len());
    let mut at = 0usize;
    while at < input.len() {
        if input[at] != b'&' {
            // Copy a run of bytes that are not an entity start, decoding UTF-8
            // as it goes so a multi-byte character is not split.
            let start = at;
            while at < input.len() && input[at] != b'&' {
                at += 1;
            }
            out.push_str(&String::from_utf8_lossy(&input[start..at]));
            continue;
        }
        let rest = &input[at + 1..];
        let end = rest.iter().position(|&c| c == b';');
        let Some(end) = end.filter(|&e| e <= 32) else {
            out.push('&');
            at += 1;
            continue;
        };
        let name = &rest[..end];
        let decoded = if name.first() == Some(&b'#') {
            numeric_entity(&name[1..])
        } else {
            core::str::from_utf8(name).ok().and_then(named_entity)
        };
        match decoded {
            Some(c) => {
                out.push(c);
                at += 1 + end + 1;
            }
            None => {
                out.push('&');
                at += 1;
            }
        }
    }
    out
}

fn numeric_entity(body: &[u8]) -> Option<char> {
    if body.is_empty() {
        return None;
    }
    let (digits, radix) = if body[0] == b'x' || body[0] == b'X' {
        (&body[1..], 16)
    } else {
        (body, 10)
    };
    let mut v: u32 = 0;
    for &c in digits {
        let d = (c as char).to_digit(radix)?;
        v = v.checked_mul(radix)?.checked_add(d)?;
        if v > 0x10FFFF {
            return None;
        }
    }
    char::from_u32(v)
}

impl Dom {
    pub fn parse(html: &[u8]) -> Dom {
        let mut p = Parser::new(html);
        p.parse();
        // Anything still open is closed by the end of the document.
        p.stack.truncate(1);
        p.dom
    }

    pub fn node(&self, id: usize) -> &NodeData {
        &self.nodes[id]
    }

    pub fn kind(&self, id: usize) -> &Kind {
        &self.nodes[id].kind
    }

    pub fn tag(&self, id: usize) -> Option<&str> {
        match &self.nodes[id].kind {
            Kind::Element { name, .. } => Some(name),
            _ => None,
        }
    }

    pub fn text_of(&self, id: usize) -> Option<&str> {
        match &self.nodes[id].kind {
            Kind::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn attr(&self, id: usize, name: &str) -> Option<&str> {
        match &self.nodes[id].kind {
            Kind::Element { attrs, .. } => attrs
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str()),
            _ => None,
        }
    }

    /// The element with this id, in document order.
    pub fn by_id(&self, want: &str) -> Option<usize> {
        (0..self.nodes.len()).find(|&i| self.attr(i, "id") == Some(want))
    }

    /// Every element with this tag name, in document order.
    pub fn by_tag(&self, want: &str) -> Vec<usize> {
        (0..self.nodes.len())
            .filter(|&i| self.tag(i) == Some(want))
            .collect()
    }

    /// The concatenation of all the text under this node, skipping the
    /// elements that are not prose.  This is what a *reader* sees.
    pub fn text_content(&self, id: usize) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    /// The text underneath, with nothing skipped.  This is what a `<script>`
    /// or a `<style>` contains, and reading those with `text_content` — which
    /// exists to keep script and style out of the rendered page — returns the
    /// empty string, which a page's script then is.
    pub fn source_text(&self, id: usize) -> String {
        let mut out = String::new();
        self.collect_all(id, &mut out);
        out
    }

    fn collect_all(&self, id: usize, out: &mut String) {
        match &self.nodes[id].kind {
            Kind::Text(t) => out.push_str(t),
            _ => {
                for &c in &self.nodes[id].children {
                    self.collect_all(c, out);
                }
            }
        }
    }

    fn collect_text(&self, id: usize, out: &mut String) {
        match &self.nodes[id].kind {
            Kind::Text(t) => out.push_str(t),
            Kind::Element { name, .. } => {
                if name == "script" || name == "style" {
                    return;
                }
                for &c in &self.nodes[id].children {
                    self.collect_text(c, out);
                }
            }
            _ => {
                for &c in &self.nodes[id].children {
                    self.collect_text(c, out);
                }
            }
        }
    }
}
