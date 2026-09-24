//! Layout: a styled document to lines of characters.
//!
//! This is a text renderer, and the layout engine is shaped by that rather
//! than pretending otherwise.  A block starts a line and its children are
//! indented under it; margins and padding that CSS measures in pixels become
//! whole character cells, because half a cell does not exist.  A table is
//! laid out into real columns, which is the one place worth the effort —
//! a table with no columns is not a table.
//!
//! What it does not do: floats, absolute positioning, flex and grid.  A page
//! whose layout depends on any of those reads as its content in document
//! order, which is what a text browser has always done and is still a page
//! you can read.
//!
//! Every line is a list of runs, so a line can be half bold and half not.
//! That is what makes a stylesheet visible at all: without runs, the only
//! thing CSS could change is where the line breaks fall.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

use super::css::{self, Align, Display, Style, WhiteSpace};
use super::dom::{Dom, Kind};

/// A stretch of characters that share a style.
#[derive(Clone, Debug)]
pub struct Run {
    pub text: String,
    pub style: Style,
}

/// One line of the page.
#[derive(Clone, Debug, Default)]
pub struct Line {
    pub runs: Vec<Run>,
    /// The first link on the line, for a renderer that can click it.
    pub link: Option<String>,
    /// Character cells of blank space before the text, from indentation.
    pub indent: usize,
    pub align: Align,
}

impl Line {
    pub fn text(&self) -> String {
        let mut s = String::new();
        for r in &self.runs {
            s.push_str(&r.text);
        }
        s
    }

    pub fn is_blank(&self) -> bool {
        self.runs.iter().all(|r| r.text.trim().is_empty())
    }

    /// Where the text stops, in character cells.
    pub fn width(&self) -> usize {
        self.runs.iter().map(|r| r.text.chars().count()).sum()
    }
}

/// Lay out a document at a given width in character cells.
pub fn layout(dom: &Dom, sheet: &css::Stylesheet, cols: usize) -> Vec<Line> {
    let styles = css::compute(dom, sheet);
    let mut b = Builder {
        dom,
        styles: &styles,
        lines: Vec::new(),
        current: Line::default(),
        col: 0,
        cols: cols.max(16),
        pending_space: false,
    };
    // The document's own children: `<html>`, and whatever is inside it.
    let root = dom.root;
    for &child in &dom.node(root).children {
        b.block_or_inline(child);
    }
    b.end_line();
    b.trim_blank_edges();
    b.lines
}

struct Builder<'a> {
    dom: &'a Dom,
    styles: &'a [Style],
    lines: Vec<Line>,
    current: Line,
    col: usize,
    cols: usize,
    pending_space: bool,
}

impl<'a> Builder<'a> {
    fn style(&self, id: usize) -> &Style {
        &self.styles[id]
    }

    fn block_or_inline(&mut self, id: usize) {
        let style = self.style(id).clone();
        if !style.visible() {
            return;
        }
        // A text node is never a block, whatever its parent's `display` says.
        // It inherits that property like any other, and treating an inherited
        // `block` as a real one breaks the line before and after every word:
        // a list item's text ends up on its own line, below its marker.
        if matches!(self.dom.kind(id), Kind::Text(_)) {
            self.text_node(id);
            return;
        }
        match self.dom.kind(id) {
            Kind::Comment => {}
            Kind::Element { .. } => {
                let tag = self.dom.tag(id).unwrap_or("");
                if style.display == Display::Block || style.display == Display::ListItem
                    || style.display == Display::Row
                {
                    self.block(id, tag, &style);
                } else {
                    self.inline(id, &style);
                }
            }
            _ => {
                for &c in &self.dom.node(id).children {
                    self.block_or_inline(c);
                }
            }
        }
    }

    fn block(&mut self, id: usize, tag: &str, style: &Style) {
        self.end_line();
        for _ in 0..style.space_before {
            self.blank_line();
        }
        // A horizontal rule is the one element whose whole purpose is a line.
        if tag == "hr" {
            let width = self.cols.saturating_sub(style.indent.max(0) as usize);
            let mut line = Line { align: Align::Left, ..Default::default() };
            line.indent = style.indent.max(0) as usize;
            line.runs.push(Run {
                text: "-".repeat(width.min(72).max(1)),
                style: style.clone(),
            });
            self.lines.push(line);
            for _ in 0..style.space_after {
                self.blank_line();
            }
            return;
        }

        if style.display == Display::Row && tag != "tr" {
            // thead/tbody/tfoot: just their rows.
            for &c in &self.dom.node(id).children {
                self.block_or_inline(c);
            }
            self.end_line();
            return;
        }

        if tag == "tr" {
            self.table_row(id, style);
            return;
        }

        let base_indent = style.indent;
        let base_align = style.align;
        // Children inherit the indent through the current line's setting, so
        // it has to be applied before the first child writes anything.
        let outer = (self.current.indent as i32, self.current.align);
        self.current.indent = (outer.0 + base_indent).max(0) as usize;
        if base_align != Align::Left {
            self.current.align = base_align;
        }

        if style.display == Display::ListItem {
            let marker = style.marker.unwrap_or('*');
            let text = if marker == '1' { "* ".to_string() } else {
                let mut s = String::new();
                s.push(marker);
                s.push(' ');
                s
            };
            self.push_run(&text, style);
            self.pending_space = false;
        }

        for &c in &self.dom.node(id).children {
            self.block_or_inline(c);
        }
        self.end_line();
        self.current.indent = outer.0.max(0) as usize;
        self.current.align = outer.1;

        for _ in 0..style.space_after {
            self.blank_line();
        }
    }

    /// A table row: each cell gets a column, because a table without columns
    /// is worse than no table at all.
    fn table_row(&mut self, id: usize, style: &Style) {
        self.end_line();
        let cells: Vec<usize> = self.dom.node(id).children.iter().copied()
            .filter(|&c| matches!(self.style(c).display, Display::Cell))
            .collect();
        if cells.is_empty() {
            return;
        }
        let n = cells.len();
        // Two cells of gap between columns, taken off the available width.
        let gaps = 2 * (n.saturating_sub(1));
        let available = self.cols.saturating_sub(gaps + style.indent.max(0) as usize);
        let each = available / n;

        let mut line = Line {
            indent: style.indent.max(0) as usize,
            align: Align::Left,
            ..Default::default()
        };
        // A row taller than one line is not supported: each cell is flattened
        // to a single line and clipped to its column.  A row that needs more
        // is a layout this renderer cannot express.
        for (i, &cell) in cells.iter().enumerate() {
            let cell_style = self.style(cell).clone();
            let mut runs = Vec::new();
            self.flatten(cell, &mut runs);
            let mut text = String::new();
            for r in &runs {
                text.push_str(&r.text);
            }
            let flat = collapse(&text);
            let mut padded = flat.clone();
            if padded.chars().count() > each {
                padded = padded.chars().take(each.saturating_sub(1)).collect();
                padded.push('…');
            }
            while padded.chars().count() < each {
                padded.push(' ');
            }
            line.runs.push(Run { text: padded, style: cell_style });
            if i + 1 < n {
                line.runs.push(Run { text: "  ".to_string(), style: style.clone() });
            }
        }
        self.lines.push(line);
    }

    /// Flatten a subtree into runs, ignoring block structure.
    fn flatten(&self, id: usize, out: &mut Vec<Run>) {
        let style = self.style(id).clone();
        if !style.visible() {
            return;
        }
        match self.dom.kind(id) {
            Kind::Text(t) => out.push(Run { text: t.clone(), style }),
            Kind::Element { .. } => {
                for &c in &self.dom.node(id).children {
                    self.flatten(c, out);
                }
            }
            _ => {}
        }
    }

    fn inline(&mut self, id: usize, style: &Style) {
        let tag = self.dom.tag(id).unwrap_or("");
        let link = if tag == "a" { self.dom.attr(id, "href") } else { None };
        match tag {
            "br" => {
                self.end_line();
                return;
            }
            "img" => {
                let alt = self.dom.attr(id, "alt").unwrap_or("");
                if !alt.is_empty() {
                    let text = alloc::format!("[{}]", alt);
                    self.push_run(&text, style);
                }
                return;
            }
            "input" => {
                let kind = self.dom.attr(id, "type").unwrap_or("text");
                let value = self.dom.attr(id, "value").unwrap_or("");
                let text = match kind {
                    "checkbox" | "radio" => {
                        if self.dom.attr(id, "checked").is_some() {
                            "[x]".to_string()
                        } else {
                            "[ ]".to_string()
                        }
                    }
                    "submit" | "button" | "reset" => {
                        alloc::format!("[{}]", if value.is_empty() { "submit" } else { value })
                    }
                    "hidden" => String::new(),
                    // An empty text box has nothing to show.  A pair of empty
                    // brackets is noise on every search page there is.
                    _ if value.is_empty() => String::new(),
                    _ => alloc::format!("[{}]", value),
                };
                if !text.is_empty() {
                    self.push_run(&text, style);
                }
                return;
            }
            "select" => {
                // The options are the useful part; a select with none is a
                // control with no content, and not worth a placeholder.
                let mut out = Vec::new();
                self.flatten(id, &mut out);
                let mut text = String::new();
                for r in &out {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(r.text.trim());
                }
                if !text.trim().is_empty() {
                    let t = alloc::format!("[{}]", text.trim());
                    self.push_run(&t, style);
                }
                return;
            }
            "button" => { /* its children are the label */ }
            _ => {}
        }

        let outer_link = self.current.link.clone();
        if let Some(href) = link {
            if self.current.link.is_none() {
                self.current.link = Some(String::from(href));
            }
        }
        for &c in &self.dom.node(id).children {
            self.block_or_inline(c);
        }
        // A child's link does not leak to the rest of the line.
        if link.is_some() {
            self.current.link = outer_link;
        }
    }

    fn text_node(&mut self, id: usize) {
        let style = self.style(id).clone();
        let text = match self.dom.kind(id) {
            Kind::Text(t) => t.clone(),
            _ => return,
        };
        match style.white_space {
            WhiteSpace::Pre | WhiteSpace::PreWrap => {
                let mut first = true;
                for part in text.split('\n') {
                    if !first {
                        self.end_line();
                        self.pending_space = false;
                    }
                    first = false;
                    if !part.is_empty() {
                        let collapsed = if style.white_space == WhiteSpace::Pre {
                            part.to_string()
                        } else {
                            collapse(part)
                        };
                        self.push_run(&collapsed, &style);
                    }
                }
            }
            WhiteSpace::Normal | WhiteSpace::NoWrap => {
                // Whether a space separates this text from what came before
                // depends on the source, not on the fact that a word was just
                // written: `<b>a</b><i>b</i>` is "ab" and `<b>a</b> <i>b</i>`
                // is "a b".
                let leading = text.starts_with(|c: char| c.is_whitespace());
                let trailing = text.ends_with(|c: char| c.is_whitespace());
                self.pending_space |= leading;
                for word in text.split_ascii_whitespace() {
                    self.push_word(word, &style);
                    // Words inside one text node are separated by the
                    // whitespace that split them, whatever the node does at
                    // its edges.
                    self.pending_space = true;
                }
                self.pending_space = trailing;
            }
        }
    }

    /// Place one word, breaking the line first if it will not fit.
    ///
    /// Does not touch `pending_space`: the caller knows whether the source had
    /// whitespace after the word, and this does not.
    fn push_word(&mut self, word: &str, style: &Style) {
        let w = word.chars().count();
        let lead = usize::from(self.pending_space && self.col > 0);
        if self.col + lead + w > self.cols && self.col > 0 {
            self.end_line();
        }
        if self.pending_space && self.col > 0 {
            self.push_run_raw(" ", style);
        }
        self.push_run_raw(word, style);
        self.pending_space = false;
    }

    fn push_run(&mut self, text: &str, style: &Style) {
        self.push_run_raw(text, style);
        self.pending_space = false;
    }

    fn push_run_raw(&mut self, text: &str, style: &Style) {
        if text.is_empty() {
            return;
        }
        self.col += text.chars().count();
        // Merge with the previous run when the style is the same, so a
        // paragraph of one colour does not become one run per word.
        if let Some(last) = self.current.runs.last_mut() {
            if last.style == *style {
                last.text.push_str(text);
                return;
            }
        }
        self.current.runs.push(Run { text: text.to_string(), style: style.clone() });
    }

    fn end_line(&mut self) {
        if self.col != 0 {
        }
        if !self.current.runs.is_empty() {
            let line = core::mem::take(&mut self.current);
            self.lines.push(line);
        }
        self.current = Line::default();
        self.col = 0;
        self.pending_space = false;
    }

    fn blank_line(&mut self) {
        self.end_line();
        self.lines.push(Line::default());
    }

    fn trim_blank_edges(&mut self) {
        while self.lines.first().map(|l| l.is_blank()).unwrap_or(false) {
            self.lines.remove(0);
        }
        while self.lines.last().map(|l| l.is_blank()).unwrap_or(false) {
            self.lines.pop();
        }
        // Runs of three or more blank lines become one: a page's markup has
        // far more empty containers than it has deliberate space.
        let mut out: Vec<Line> = Vec::with_capacity(self.lines.len());
        let mut blanks = 0;
        for line in self.lines.drain(..) {
            if line.is_blank() {
                blanks += 1;
                if blanks > 1 {
                    continue;
                }
            } else {
                blanks = 0;
            }
            out.push(line);
        }
        self.lines = out;
    }
}

/// Collapse a run of whitespace to single spaces.
pub fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

/// The byte range of each line within a flat text, for callers that want to
/// draw from a single buffer rather than from runs.
pub fn line_offsets(lines: &[Line]) -> Vec<Range<usize>> {
    let mut out = Vec::with_capacity(lines.len());
    let mut at = 0usize;
    for line in lines {
        let n = line.text().len();
        out.push(at..at + n);
        at += n;
    }
    out
}
