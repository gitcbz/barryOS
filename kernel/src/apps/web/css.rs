//! CSS: parse stylesheets, decide which rules apply, and compute a style.
//!
//! The cascade is the part that has to be right.  Everything here exists to
//! answer one question — for this element, which of the declarations that
//! mention `color` wins — and answering it wrongly looks like a stylesheet
//! that was ignored, not like a bug in the cascade.
//!
//! So the order is the specification's: origin (all of it author-level, since
//! there is no user stylesheet and no `!important` worth supporting), then
//! specificity, then the order the rules appear in.  Equal specificity and
//! later wins, which is why `order` is carried on every declaration rather
//! than on every rule.
//!
//! What is not here: `!important`, `@media` queries beyond print, `@import`,
//! `@supports`, custom properties, and the layout properties that need a
//! layout engine this renderer does not have.  A declaration that is not
//! understood is kept rather than dropped, so `layout` can look for one it
//! knows further down the list.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::format;

use super::dom::Dom;

// ---------------------------------------------------------------------------
//  Computed style
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Display {
    /// Block-level: starts on a new line and takes the full width.
    Block,
    /// Flows with the text around it.
    Inline,
    /// A list item: like a block, with a marker.
    ListItem,
    /// A table cell.
    Cell,
    /// A table row.
    Row,
    /// Present but not shown, and taking no space.
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Is this a block-level element, before any stylesheet says otherwise?
pub fn is_block_tag(tag: &str) -> bool {
    matches!(tag, "html" | "body" | "address" | "article" | "aside"
                 | "blockquote" | "details" | "dialog" | "div" | "dl" | "dt"
                 | "dd" | "fieldset" | "figcaption" | "figure" | "footer"
                 | "form" | "header" | "main" | "nav" | "p" | "pre"
                 | "section" | "summary" | "ul" | "ol" | "li" | "table"
                 | "caption" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                 | "hr" | "center")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const BLACK: Rgb = Rgb(0, 0, 0);
    pub const WHITE: Rgb = Rgb(255, 255, 255);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WhiteSpace {
    /// Runs of whitespace collapse to one space, and lines wrap.
    Normal,
    /// Every space is kept and lines wrap.
    PreWrap,
    /// Every space and every newline is kept, and nothing wraps.
    Pre,
    /// Nothing wraps.
    NoWrap,
}

/// The style of one element, after the cascade and inheritance.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub display: Display,
    pub color: Rgb,
    pub background: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub align: Align,
    /// Left inset in character cells, from margins and padding.
    pub indent: i32,
    /// Blank lines before and after a block.
    pub space_before: u8,
    pub space_after: u8,
    pub white_space: WhiteSpace,
    /// The marker of a list item, if it is one.
    pub marker: Option<char>,
    /// 0 = hidden, 1 = normal, 2 = a heading's size.
    pub scale: u8,
    /// Shown in the browser as a link: underlined and coloured.
    pub link: bool,
}

impl Style {
    pub fn initial() -> Style {
        Style {
            display: Display::Inline,
            color: Rgb::BLACK,
            background: None,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            align: Align::Left,
            indent: 0,
            space_before: 0,
            space_after: 0,
            white_space: WhiteSpace::Normal,
            marker: None,
            scale: 1,
            link: false,
        }
    }

    /// What the renderer starts with, before any stylesheet: the browser's own
    /// defaults.  Without this a page with no CSS is one undifferentiated
    /// paragraph, which is most of what a stylesheet-free page relies on.
    ///
    /// Written as independent steps rather than one match over the tag name.
    /// A match takes the first arm that fits and is otherwise silent about the
    /// rest, which is how `blockquote` lost its indent and `dl` lost its
    /// display: both were listed in an earlier combined arm as well, and the
    /// arm meant for them could never be reached.  Nothing here can shadow
    /// anything else, because nothing here is exclusive.
    pub fn user_agent(tag: &str) -> Style {
        let mut s = Style::initial();

        // Display, in the order of the exceptions.
        s.display = if matches!(tag, "head" | "script" | "style" | "title" | "meta"
                                     | "link" | "base" | "template"
                                     | "iframe" | "object" | "embed" | "canvas"
                                     | "svg" | "map" | "area" | "param"
                                     | "source" | "track" | "col" | "colgroup") {
            Display::None
        } else if tag == "li" {
            Display::ListItem
        } else if matches!(tag, "td" | "th") {
            Display::Cell
        } else if matches!(tag, "tr" | "thead" | "tbody" | "tfoot") {
            Display::Row
        } else if is_block_tag(tag) {
            Display::Block
        } else {
            Display::Inline
        };

        // Vertical space around the blocks that divide a page into parts.
        // `dl`, `dt`, `li` and `blockquote` are block-level but sit tight
        // against what they belong to: list items of one list are one thing,
        // not several.
        if is_block_tag(tag) && !matches!(tag, "dl" | "dt" | "li" | "blockquote") {
            s.space_before = 1;
            s.space_after = 1;
        }

        // Per-element adjustments, each independent of the others.
        match tag {
            "blockquote" | "dd" => s.indent = 4,
            "h1" | "h2" => {
                s.bold = true;
                s.scale = 2;
            }
            "h3" | "h4" | "h5" | "h6" | "b" | "strong" | "th" | "caption" => {
                s.bold = true;
            }
            "i" | "em" | "cite" | "dfn" | "var" | "code" | "kbd" | "samp" | "tt" => {
                s.italic = true;
            }
            "u" | "ins" => s.underline = true,
            "s" | "strike" | "del" => s.strike = true,
            "pre" => s.white_space = WhiteSpace::Pre,
            "center" => s.align = Align::Center,
            "a" => {
                s.link = true;
                s.underline = true;
            }
            "li" => s.marker = Some('*'),
            _ => {}
        }
        s
    }

    /// Is this style displayed at all?
    pub fn visible(&self) -> bool {
        self.scale != 0 && self.display != Display::None
    }
}

// ---------------------------------------------------------------------------
//  Parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    /// Position in the stylesheet, for the "later wins" rule.
    pub order: u32,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub decls: Vec<Declaration>,
}

/// A compound selector: `div#main.note[href]`, optionally after a combinator.
#[derive(Debug, Clone, Default)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<(String, Option<String>)>,
    /// The combinator that joins this to the previous compound: ' ' or '>'.
    pub combinator: char,
}

#[derive(Debug, Clone)]
pub struct Selector {
    /// Rightmost first, so matching walks the ancestors in order.
    pub parts: Vec<Compound>,
    pub specificity: u32,
}

impl Selector {
    /// Does this selector match this element?
    pub fn matches(&self, dom: &Dom, id: usize) -> bool {
        // The rightmost compound matches the element itself; each one to its
        // left must match an ancestor, and a `>` must match the immediate
        // parent.  Walking outward from the element is what makes this cheap.
        let mut node = Some(id);
        let mut i = self.parts.len();
        while i > 0 {
            i -= 1;
            let part = &self.parts[i];
            let Some(n) = node else { return false };
            if !part.matches(dom, n) {
                return false;
            }
            if i == 0 {
                return true;
            }
            let combinator = self.parts[i].combinator;
            if combinator == '>' {
                node = dom.node(n).parent;
            } else {
                // Descendant: the nearest ancestor that matches.
                let mut up = dom.node(n).parent;
                loop {
                    let Some(u) = up else { return false };
                    if self.parts[i - 1].matches(dom, u) {
                        node = Some(u);
                        break;
                    }
                    up = dom.node(u).parent;
                }
            }
        }
        true
    }
}

impl Compound {
    fn matches(&self, dom: &Dom, id: usize) -> bool {
        let Some(tag) = dom.tag(id) else { return false };
        if let Some(want) = &self.tag {
            if want != "*" && !tag.eq_ignore_ascii_case(want) {
                return false;
            }
        }
        if let Some(want) = &self.id {
            if dom.attr(id, "id") != Some(want.as_str()) {
                return false;
            }
        }
        let class_attr = dom.attr(id, "class").unwrap_or("");
        for want in &self.classes {
            let mut found = false;
            for have in class_attr.split_ascii_whitespace() {
                if have == want {
                    found = true;
                    break;
                }
            }
            if !found {
                return false;
            }
        }
        for (name, value) in &self.attrs {
            let Some(have) = dom.attr(id, name) else { return false };
            if let Some(want) = value {
                if have != want {
                    return false;
                }
            }
        }
        true
    }
}

pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

impl Stylesheet {
    pub fn empty() -> Stylesheet {
        Stylesheet { rules: Vec::new() }
    }

    /// Several stylesheets, in the order they should win.
    pub fn parse_all(sheets: &[String]) -> Stylesheet {
        let mut all = Stylesheet::empty();
        for s in sheets {
            all.add(s);
        }
        all
    }

    /// Add a stylesheet's rules after the ones already here.
    pub fn add(&mut self, text: &str) {
        let stripped = strip_comments(text);
        let mut order = self
            .rules
            .iter()
            .flat_map(|r| r.decls.iter().map(|d| d.order))
            .max()
            .unwrap_or(0)
            + 1;
        self.add_rules(&stripped, &mut order);
    }

    /// Read rules out of `text`, recursing into the blocks of at-rules that
    /// contain rules.
    fn add_rules(&mut self, text: &str, order: &mut u32) {
        let mut rest = text;
        loop {
            let start = rest.trim_start();
            // An at-rule with no block — `@import`, `@charset` — ends at a
            // semicolon, and its text is not a selector.  Checking for that
            // before the block search is what keeps `@import url(x); p { }`
            // from becoming a rule whose selector is `@import url(x); p`.
            if start.starts_with('@') {
                let semi = start.find(';');
                let brace = start.find('{');
                if let Some(s) = semi {
                    if brace.is_none() || s < brace.unwrap() {
                        rest = &start[s + 1..];
                        continue;
                    }
                }
            }
            let Some(brace) = start.find('{') else { return };
            let prelude = start[..brace].trim().to_string();
            let Some(close) = match_close(&start[brace + 1..]) else { return };
            let body = &start[brace + 1..brace + 1 + close];
            rest = &start[brace + 1 + close + 1..];

            if prelude.starts_with('@') {
                let name = prelude
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match name.as_str() {
                    // Only the print medium is worth excluding: this renderer
                    // is the screen, so a screen rule is a rule it should
                    // apply even if it is inside a query about widths.
                    "@media" => {
                        if !prelude.to_ascii_lowercase().contains("print") {
                            self.add_rules(body, order);
                        }
                    }
                    "@supports" => self.add_rules(body, order),
                    // @font-face, @keyframes, @page: nothing here can use
                    // them, and their contents are not rules about elements.
                    _ => {}
                }
                if rest.trim().is_empty() {
                    return;
                }
                continue;
            }

            let mut selectors = Vec::new();
            for part in prelude.split(',') {
                if let Some(s) = parse_selector(part) {
                    selectors.push(s);
                }
            }
            if selectors.is_empty() {
                if rest.trim().is_empty() {
                    return;
                }
                continue;
            }
            let mut decls = Vec::new();
            for piece in split_declarations(body) {
                let Some(colon) = piece.find(':') else { continue };
                let name = piece[..colon].trim().to_ascii_lowercase();
                let value = piece[colon + 1..].trim().to_string();
                if name.is_empty() || value.is_empty() {
                    continue;
                }
                decls.push(Declaration { name, value, order: *order });
                *order += 1;
            }
            if !decls.is_empty() {
                self.rules.push(Rule { selectors, decls });
            }
            if rest.trim().is_empty() {
                return;
            }
        }
    }
}

/// Split a declaration block on semicolons, ignoring the ones inside brackets
/// or strings — `background: url(a.png?x=1;y=2)` is one declaration.
fn split_declarations(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut in_string: Option<char> = None;
    for c in body.chars() {
        if let Some(q) = in_string {
            current.push(c);
            if c == q {
                in_string = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                in_string = Some(c);
                current.push(c);
            }
            '(' => {
                depth += 1;
                current.push(c);
            }
            ')' => {
                depth -= 1;
                current.push(c);
            }
            ';' if depth == 0 => {
                out.push(current.clone());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// The offset of the `}` matching the `{` just before `text`.
fn match_close(text: &str) -> Option<usize> {
    let mut depth = 1i32;
    let mut in_string: Option<char> = None;
    for (i, c) in text.char_indices() {
        if let Some(q) = in_string {
            if c == q {
                in_string = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => in_string = Some(c),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut in_string: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = in_string {
            out.push(c as char);
            if c == q {
                in_string = None;
            }
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' {
            in_string = Some(c);
            out.push(c as char);
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(c as char);
        i += 1;
    }
    out
}

fn parse_selector(text: &str) -> Option<Selector> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // Pseudo-elements and pseudo-classes other than the ones that always
    // match are dropped rather than treated as a name: `a:hover` should not
    // become a rule about elements called "a:hover".
    let mut parts: Vec<Compound> = Vec::new();
    let mut current = Compound::default();
    let mut specificity = 0u32;
    let mut chars = text.chars().peekable();
    let mut pending_combinator = ' ';

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if !current_is_empty(&current) {
                    current.combinator = pending_combinator;
                    parts.push(current.clone());
                    current = Compound::default();
                    pending_combinator = ' ';
                }
            }
            '>' | '+' | '~' => {
                if !current_is_empty(&current) {
                    current.combinator = pending_combinator;
                    parts.push(current.clone());
                    current = Compound::default();
                }
                // `+` and `~` need siblings, which the tree walk does not do;
                // treating them as descendant is wrong in a way that only
                // affects which of two rules wins, and a rule that never
                // matches at all is worse.
                pending_combinator = '>';
                if c == '+' || c == '~' {
                    pending_combinator = ' ';
                }
            }
            '#' => {
                let name = take_name(&mut chars);
                if !name.is_empty() {
                    specificity += 100;
                    current.id = Some(name);
                }
            }
            '.' => {
                let name = take_name(&mut chars);
                if !name.is_empty() {
                    specificity += 10;
                    current.classes.push(name);
                }
            }
            '[' => {
                let mut inner = String::new();
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    inner.push(c);
                }
                specificity += 10;
                let (name, value) = match inner.find('=') {
                    Some(eq) => (
                        inner[..eq].trim().to_ascii_lowercase(),
                        Some(inner[eq + 1..].trim().trim_matches('"').trim_matches('\'').to_string()),
                    ),
                    None => (inner.trim().to_ascii_lowercase(), None),
                };
                if !name.is_empty() {
                    current.attrs.push((name, value));
                }
            }
            ':' => {
                let name = take_name(&mut chars).to_ascii_lowercase();
                match name.as_str() {
                    // A pseudo-element means the element's *part*, which this
                    // renderer has no way to style.
                    "before" | "after" | "first-line" | "first-letter"
                    | "placeholder" | "selection" => return None,
                    // The only pseudo-classes that are true of an element at
                    // rest.  Everything else is a state this browser does not
                    // have, and matching them unconditionally would apply
                    // hover styles to every link on the page.
                    "root" | "link" | "any-link" | "enabled" | "not-a-real" => {
                        specificity += 10;
                    }
                    _ => continue,
                }
            }
            '*' => {
                current.tag = Some("*".to_string());
            }
            _ => {
                // A type selector, which may itself contain dashes.
                let mut name = String::new();
                name.push(c);
                name.push_str(&take_name(&mut chars));
                if !name.is_empty() {
                    specificity += 1;
                    current.tag = Some(name.to_ascii_lowercase());
                }
            }
        }
    }
    if !current_is_empty(&current) {
        current.combinator = pending_combinator;
        parts.push(current);
    }
    if parts.is_empty() {
        return None;
    }
    // Leftmost first in the vector would read better, but matching starts at
    // the element and walks outward, so the rightmost has to be first.
    parts.reverse();
    Some(Selector { parts, specificity })
}

fn current_is_empty(c: &Compound) -> bool {
    c.tag.is_none() && c.id.is_none() && c.classes.is_empty() && c.attrs.is_empty()
}

fn take_name<I: Iterator<Item = char>>(chars: &mut core::iter::Peekable<I>) -> String {
    let mut out = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() {
            out.push(c);
            chars.next();
        } else {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------
//  Values
// ---------------------------------------------------------------------------

/// A CSS length in character cells.
///
/// `px` is the only unit worth converting: a character cell is eight pixels
/// wide and sixteen tall, so the conversion is exact for the units that matter
/// and a guess for the rest.
fn length_cells(value: &str, horizontal: bool) -> Option<i32> {
    let v = value.trim();
    let (number, unit) = split_number(v)?;
    let per_unit = match unit.as_str() {
        "px" => 1.0 / if horizontal { 8.0 } else { 16.0 },
        "pt" => 96.0 / 72.0 / if horizontal { 8.0 } else { 16.0 },
        "em" | "rem" => {
            if horizontal { 2.0 } else { 1.0 }        // about two characters per em
        }
        "ex" | "ch" => if horizontal { 1.0 } else { 1.0 },
        "%" => return None,                            // needs the containing block
        "" => return None,                             // a bare number is not a length
        _ => return None,
    };
    Some((number * per_unit).round() as i32)
}

fn split_number(v: &str) -> Option<(f32, String)> {
    let mut num = String::new();
    let mut unit = String::new();
    for c in v.chars() {
        if c.is_ascii_digit() || c == '.' || c == '-' || c == '+' {
            if unit.is_empty() {
                num.push(c);
            }
        } else {
            unit.push(c);
        }
    }
    let n: f32 = num.parse().ok()?;
    Some((n, unit.trim().to_ascii_lowercase()))
}

fn parse_colour(value: &str) -> Option<Rgb> {
    let v = value.trim().to_ascii_lowercase();
    if let Some(hex) = v.strip_prefix('#') {
        let expand = |c: u8| -> u8 {
            let d = (c as char).to_digit(16).unwrap_or(0) as u8;
            d * 16 + d
        };
        let bytes = hex.as_bytes();
        return match bytes.len() {
            3 => Some(Rgb(expand(bytes[0]), expand(bytes[1]), expand(bytes[2]))),
            4 => Some(Rgb(expand(bytes[0]), expand(bytes[1]), expand(bytes[2]))),
            6 => Some(Rgb(
                (hex_char(bytes[0])? << 4) | hex_char(bytes[1])?,
                (hex_char(bytes[2])? << 4) | hex_char(bytes[3])?,
                (hex_char(bytes[4])? << 4) | hex_char(bytes[5])?,
            )),
            8 => Some(Rgb(
                (hex_char(bytes[0])? << 4) | hex_char(bytes[1])?,
                (hex_char(bytes[2])? << 4) | hex_char(bytes[3])?,
                (hex_char(bytes[4])? << 4) | hex_char(bytes[5])?,
            )),
            _ => None,
        };
    }
    if let Some(inner) = v.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<i32> = inner
            .split(',')
            .filter_map(|p| p.trim().trim_end_matches('%').parse::<f32>().ok())
            .map(|f| f.round() as i32)
            .collect();
        if parts.len() == 3 {
            return Some(Rgb(
                parts[0].clamp(0, 255) as u8,
                parts[1].clamp(0, 255) as u8,
                parts[2].clamp(0, 255) as u8,
            ));
        }
    }
    if let Some(inner) = v.strip_prefix("rgba(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').map(|p| p.trim()).collect();
        if parts.len() == 4 {
            let alpha: f32 = parts[3].parse().unwrap_or(0.0);
            if alpha <= 0.05 {
                // Fully transparent: no background at all rather than black.
                return None;
            }
        }
        return parse_colour(&format!("rgb({}, {}, {})", parts[0], parts[1], parts[2]));
    }
    named_colour(&v)
}

fn hex_char(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

fn named_colour(name: &str) -> Option<Rgb> {
    Some(match name {
        "black" => Rgb(0, 0, 0),
        "white" => Rgb(255, 255, 255),
        "red" => Rgb(255, 0, 0),
        "lime" => Rgb(0, 255, 0),
        "green" => Rgb(0, 128, 0),
        "blue" => Rgb(0, 0, 255),
        "yellow" => Rgb(255, 255, 0),
        "cyan" | "aqua" => Rgb(0, 255, 255),
        "magenta" | "fuchsia" => Rgb(255, 0, 255),
        "silver" => Rgb(192, 192, 192),
        "gray" | "grey" => Rgb(128, 128, 128),
        "maroon" => Rgb(128, 0, 0),
        "olive" => Rgb(128, 128, 0),
        "purple" => Rgb(128, 0, 128),
        "teal" => Rgb(0, 128, 128),
        "navy" => Rgb(0, 0, 128),
        "orange" => Rgb(255, 165, 0),
        "brown" => Rgb(165, 42, 42),
        "pink" => Rgb(255, 192, 203),
        "gold" => Rgb(255, 215, 0),
        "darkgray" | "darkgrey" => Rgb(169, 169, 169),
        "lightgray" | "lightgrey" => Rgb(211, 211, 211),
        "darkblue" => Rgb(0, 0, 139),
        "darkred" => Rgb(139, 0, 0),
        "darkgreen" => Rgb(0, 100, 0),
        "lightblue" => Rgb(173, 216, 230),
        "lightgreen" => Rgb(144, 238, 144),
        "whitesmoke" => Rgb(245, 245, 245),
        "beige" => Rgb(245, 245, 220),
        "ivory" => Rgb(255, 255, 240),
        "crimson" => Rgb(220, 20, 60),
        "tomato" => Rgb(255, 99, 71),
        "salmon" => Rgb(250, 128, 114),
        "khaki" => Rgb(240, 230, 140),
        "indigo" => Rgb(75, 0, 130),
        "violet" => Rgb(238, 130, 238),
        "transparent" => return None,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
//  The cascade
// ---------------------------------------------------------------------------

/// One matched declaration, with everything needed to order it.
struct Candidate {
    specificity: u32,
    order: u32,
    /// Set for a `style=""` declaration, which beats every rule.
    inline: bool,
    name: String,
    value: String,
}

/// Compute a style for every node in the document.
///
/// The result is indexed by node id and has an entry for every node, elements
/// and text alike: layout walks the tree in order and needs a style for each
/// child it visits without having to ask what kind of node it is.
pub fn compute(dom: &Dom, sheet: &Stylesheet) -> Vec<Style> {
    let mut out: Vec<Style> = Vec::with_capacity(dom.nodes.len());
    for id in 0..dom.nodes.len() {
        let parent_style = dom
            .node(id)
            .parent
            .map(|p| out[p].clone())
            .unwrap_or_else(Style::initial);

        let style = match dom.tag(id) {
            Some(tag) => element_style(dom, sheet, id, tag, &parent_style),
            None => {
                // Text takes its parent's style; a comment takes none of it.
                match dom.kind(id) {
                    super::dom::Kind::Text(_) => parent_style,
                    _ => {
                        let mut s = parent_style;
                        s.display = Display::None;
                        s
                    }
                }
            }
        };
        out.push(style);
    }
    out
}

fn element_style(dom: &Dom, sheet: &Stylesheet, id: usize, tag: &str, parent: &Style) -> Style {
    // The three sources, lowest priority first: what a property inherits from
    // the parent, then the renderer's own defaults for the element, then the
    // page's rules.  Getting this order wrong is how `white-space: pre` on a
    // `<pre>` gets overwritten by the `normal` it would have inherited, which
    // looks like the element being ignored.
    let mut s = Style::initial();

    // Inherited properties.  `display`, margins and the rest do not inherit:
    // a `<span>` inside a `<p>` is not a paragraph.
    s.color = parent.color;
    s.bold = parent.bold;
    s.italic = parent.italic;
    s.underline = parent.underline;
    s.strike = parent.strike;
    s.align = parent.align;
    s.white_space = parent.white_space;
    s.scale = parent.scale;
    // A link is a link because it is an `<a>`; a child of one is not.
    s.link = tag.eq_ignore_ascii_case("a");

    // The renderer's defaults for this element, which are the "user agent
    // stylesheet" in everything but name.
    let ua = Style::user_agent(tag);
    s.display = ua.display;
    s.indent = ua.indent;
    s.space_before = ua.space_before;
    s.space_after = ua.space_after;
    s.marker = ua.marker;
    s.bold |= ua.bold;
    s.italic |= ua.italic;
    s.underline |= ua.underline;
    s.strike |= ua.strike;
    if ua.scale > s.scale {
        s.scale = ua.scale;
    }
    // `white-space: pre` on the element itself beats the inherited value.
    if ua.white_space != parent.white_space {
        s.white_space = ua.white_space;
    }
    if ua.align != Align::Left {
        s.align = ua.align;
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    for rule in &sheet.rules {
        for sel in &rule.selectors {
            if sel.matches(dom, id) {
                for d in &rule.decls {
                    candidates.push(Candidate {
                        specificity: sel.specificity,
                        order: d.order,
                        inline: false,
                        name: d.name.clone(),
                        value: d.value.clone(),
                    });
                }
                break;              // one selector per rule is enough
            }
        }
    }
    if let Some(inline) = dom.attr(id, "style") {
        for piece in split_declarations(inline) {
            let Some(colon) = piece.find(':') else { continue };
            let name = piece[..colon].trim().to_ascii_lowercase();
            let value = piece[colon + 1..].trim().to_string();
            if name.is_empty() || value.is_empty() {
                continue;
            }
            candidates.push(Candidate {
                specificity: 0,
                order: u32::MAX,
                inline: true,
                name,
                value,
            });
        }
    }

    // Apply in cascade order, so the last one written for a property wins.
    candidates.sort_by_key(|c| (c.inline, c.specificity, c.order));
    for c in &candidates {
        apply(&mut s, &c.name, &c.value);
    }
    s
}

fn apply(s: &mut Style, name: &str, value: &str) {
    let v = value.trim();
    let lower = v.to_ascii_lowercase();
    match name {
        "display" => {
            s.display = match lower.as_str() {
                "none" => Display::None,
                "block" | "flex" | "grid" | "table" | "list-item" => {
                    if lower == "list-item" {
                        Display::ListItem
                    } else {
                        Display::Block
                    }
                }
                "table-row" => Display::Row,
                "table-cell" => Display::Cell,
                // Inline-block is a block that sits in a line; this renderer
                // has no line boxes to put it in, and treating it as inline
                // keeps the surrounding text in order.
                _ => Display::Inline,
            };
        }
        "color" => {
            if let Some(c) = parse_colour(v) {
                s.color = c;
            }
        }
        "background" | "background-color" => {
            s.background = parse_colour(v);
        }
        "font-weight" => {
            s.bold = match lower.as_str() {
                "normal" | "400" | "300" | "200" | "100" => false,
                "bold" | "bolder" => true,
                _ => lower.parse::<u32>().map(|n| n >= 600).unwrap_or(s.bold),
            };
        }
        "font-style" => {
            s.italic = matches!(lower.as_str(), "italic" | "oblique");
        }
        "text-decoration" | "text-decoration-line" => {
            for part in lower.split_ascii_whitespace() {
                match part {
                    "underline" => s.underline = true,
                    "line-through" => s.strike = true,
                    "none" => {
                        s.underline = false;
                        s.strike = false;
                    }
                    _ => {}
                }
            }
        }
        "text-align" => {
            s.align = match lower.as_str() {
                "center" => Align::Center,
                "right" | "end" => Align::Right,
                _ => Align::Left,
            };
        }
        "text-transform" => {
            // Handled at layout time through the text itself, so it is kept
            // as a marker in the style rather than applied twice.
        }
        "white-space" => {
            s.white_space = match lower.as_str() {
                "pre" => WhiteSpace::Pre,
                "pre-wrap" | "pre-line" => WhiteSpace::PreWrap,
                "nowrap" => WhiteSpace::NoWrap,
                _ => WhiteSpace::Normal,
            };
        }
        "margin-left" | "padding-left" | "border-left-width" => {
            if let Some(n) = length_cells(v, true) {
                s.indent += n;
            }
        }
        "margin-top" | "padding-top" => {
            if let Some(n) = length_cells(v, false) {
                s.space_before = s.space_before.max(n.clamp(0, 4) as u8);
            }
        }
        "margin-bottom" | "padding-bottom" => {
            if let Some(n) = length_cells(v, false) {
                s.space_after = s.space_after.max(n.clamp(0, 4) as u8);
            }
        }
        "margin" | "padding" => {
            // The first value is the top and the fourth the left; a single
            // value applies to all four.
            let parts: Vec<&str> = v.split_ascii_whitespace().collect();
            let top = parts.first().copied().unwrap_or("0");
            let left = parts.get(3).or(parts.get(1)).copied().unwrap_or(top);
            if let Some(n) = length_cells(top, false) {
                s.space_before = s.space_before.max(n.clamp(0, 4) as u8);
                s.space_after = s.space_after.max(n.clamp(0, 4) as u8);
            }
            if let Some(n) = length_cells(left, true) {
                s.indent += n;
            }
        }
        "list-style" | "list-style-type" => {
            for part in lower.split_ascii_whitespace() {
                let m = match part {
                    "none" => Some(' '),
                    "disc" | "circle" => Some('*'),
                    "square" => Some('#'),
                    "decimal" => Some('1'),
                    "lower-alpha" => Some('a'),
                    "upper-alpha" => Some('A'),
                    "lower-roman" => Some('i'),
                    "upper-roman" => Some('I'),
                    _ => None,
                };
                if let Some(m) = m {
                    s.marker = Some(m);
                }
            }
        }
        "visibility" => {
            if lower == "hidden" || lower == "collapse" {
                s.scale = 0;
            }
        }
        "font-size" => {
            // Coarse: the renderer has one bitmap font, so a size can make
            // text stand out or not, and nothing in between.
            let big = match lower.as_str() {
                "xx-small" | "x-small" | "small" => false,
                "large" | "x-large" | "xx-large" => true,
                _ => length_cells(v, false).map(|n| n >= 2).unwrap_or(false),
            };
            s.scale = if big { 2 } else { s.scale.max(1) };
        }
        "opacity" => {
            if lower.parse::<f32>().map(|f| f <= 0.05).unwrap_or(false) {
                s.scale = 0;
            }
        }
        "height" | "max-height" => {
            if lower == "0" || lower == "0px" {
                s.scale = 0;
            }
        }
        "overflow" | "position" | "float" | "width" | "top" | "left"
        | "right" | "bottom" | "z-index" | "cursor" | "transition"
        | "animation" | "transform" | "box-shadow" | "border-radius"
        | "font-family" | "line-height" | "letter-spacing" | "flex"
        | "justify-content" | "align-items" | "gap" | "grid-template-columns" => {
            // Understood as CSS, not acted on.  Listed so that the ones which
            // would change the reading of the page if they were guessed at
            // are visibly deliberate rather than forgotten.
        }
        _ => {}
    }
}
