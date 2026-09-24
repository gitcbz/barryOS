//! Rendering a web page: markup, stylesheet and script.
//!
//! Three layers, each of which can be read and tested on its own:
//!
//!   dom     bytes to a document tree
//!   css     stylesheets, and which rules apply to which elements
//!   layout  a document with computed styles to lines of styled characters
//!   js      the page's scripts, and the little DOM they are given to change
//!
//! None of this draws.  It produces the lines; `browser` puts them on screen,
//! and keeping the two apart is what makes the interesting part testable
//! without a framebuffer.

pub mod css;
pub mod dom;
pub mod js;
pub mod layout;

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

/// Everything one page needs, in the order a browser does it.
///
/// The document is shared rather than owned, because a script has to be able
/// to change it while the interpreter is running and the layout has to see
/// what changed afterwards.
pub struct Page {
    pub dom: Rc<RefCell<dom::Dom>>,
    /// `<script>` contents, in document order.
    pub scripts: Vec<String>,
    /// `<script src>` URLs, which the caller has to fetch.
    pub script_urls: Vec<String>,
    /// `<link rel=stylesheet>` hrefs, which the caller has to fetch.
    pub stylesheets: Vec<String>,
    /// Stylesheets that have been fetched, in the order they should win.
    pub css: Vec<String>,
}

impl Page {
    pub fn parse(html: &[u8]) -> Page {
        let dom = Rc::new(RefCell::new(dom::Dom::parse(html)));
        let mut scripts = Vec::new();
        let mut script_urls = Vec::new();
        let mut stylesheets = Vec::new();
        {
            let d = dom.borrow();
            for &id in &d.by_tag("script") {
                match d.attr(id, "src") {
                    Some(src) => script_urls.push(String::from(src)),
                    // `source_text`, not `text_content`: the latter skips
                    // script and style so they stay out of the rendered page,
                    // which is exactly what is wanted here and the opposite
                    // of what it gives.
                    None => scripts.push(d.source_text(id)),
                }
            }
            for &id in &d.by_tag("link") {
                let rel = d.attr(id, "rel").unwrap_or("");
                if rel.eq_ignore_ascii_case("stylesheet") {
                    if let Some(href) = d.attr(id, "href") {
                        stylesheets.push(String::from(href));
                    }
                }
            }
        }
        Page { dom, scripts, script_urls, stylesheets, css: Vec::new() }
    }

    /// The stylesheets written into the document, which need no fetching.
    pub fn inline_css(&self) -> Vec<String> {
        let d = self.dom.borrow();
        d.by_tag("style")
            .into_iter()
            .map(|id| d.source_text(id))
            .collect()
    }

    /// A `<meta http-equiv="refresh">` target, if the page has one.
    ///
    /// This is the redirect that works with scripting turned off, which is
    /// exactly the case this browser is in when a page's scripts are too much
    /// for it.  baidu.com's https page is nothing but a script redirect to the
    /// plain-http one and a meta refresh saying the same thing — so honouring
    /// the meta tag is the difference between a blank page and the site.
    pub fn meta_refresh(&self) -> Option<String> {
        let d = self.dom.borrow();
        for &id in &d.by_tag("meta") {
            let http_equiv = d.attr(id, "http-equiv").unwrap_or("");
            if !http_equiv.eq_ignore_ascii_case("refresh") {
                continue;
            }
            let content = d.attr(id, "content")?;
            let lower = content.to_ascii_lowercase();
            let at = lower.find("url=")?;
            let url = content[at + 4..].trim().trim_matches('"').trim_matches('\'');
            if !url.is_empty() {
                return Some(String::from(url));
            }
        }
        None
    }

    /// The base URL for relative links, from `<base href>`, if present.
    pub fn base_href(&self) -> Option<String> {
        let d = self.dom.borrow();
        let id = *d.by_tag("base").first()?;
        d.attr(id, "href").map(String::from)
    }

    /// Run the page's scripts and return what they did.
    pub fn run_scripts(&self, href: &str) -> js::Outcome {
        js::run(self.dom.clone(), &self.scripts, href)
    }

    /// Lay the document out at a given width.
    pub fn layout(&self, cols: usize) -> Vec<layout::Line> {
        let sheets: Vec<String> = self
            .inline_css()
            .into_iter()
            .chain(self.css.iter().cloned())
            .collect();
        let styles = css::Stylesheet::parse_all(&sheets);
        layout::layout(&self.dom.borrow(), &styles, cols)
    }
}
