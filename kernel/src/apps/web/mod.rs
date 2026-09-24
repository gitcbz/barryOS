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

/// Run the whole engine over a page whose answer is known.
///
/// The pieces all have tests of their own — the parser on real markup, the
/// cascade on real stylesheets, the interpreter against a page it builds
/// itself — but none of those can say whether the four together turn a
/// document into the lines a reader expects.  This can, and it runs at boot
/// where a serial log captures it.
///
/// The page is written to exercise the things that go wrong quietly: a loop
/// and `innerHTML`, an element built and appended, a class whose stylesheet
/// says it should be different, a table, and a `<script>` whose *source* must
/// not appear in the output.
pub fn selftest() -> usize {
    const PAGE: &str = r#"<!doctype html><html><head>
<style>
  .shout { color: #ff0000; font-weight: bold }
  h1 { color: #0000ff }
</style></head><body>
<h1>Heading</h1>
<div id="out"></div>
<ul id="list"></ul>
<table><tr><td>left</td><td>right</td></tr></table>
<pre>  two
  lines</pre>
<script>
  var s = 0;
  for (var i = 1; i <= 10; i++) s += i;
  document.getElementById('out').innerHTML = '<p>sum is ' + s + '</p>';
  var names = ['alpha', 'beta'];
  var ul = document.getElementById('list');
  for (var j = 0; j < names.length; j++) {
    var li = document.createElement('li');
    li.textContent = names[j].toUpperCase();
    li.className = 'shout';
    ul.appendChild(li);
  }
  console.log('done');
</script>
</body></html>"#;

    // Printed because a renderer that runs out of heap corrupts rather than
    // fails: a slice with a garbage length is a wild pointer, not an error.
    crate::serial::print_str("[web]   heap free ");
    crate::serial::print_dec(crate::mem::heap::free() as u64);
    crate::serial::print_str("
");
    let mark = crate::mem::heap::mark();
    let mut failures = 0usize;
    {
        let page = Page::parse(PAGE.as_bytes());
        let outcome = page.run_scripts("https://example.invalid/");
        let has = |needle: &str, what: &str, failures: &mut usize| {
            let lines = page.layout(72);
            let text = lines
                .iter()
                .map(|l| l.text())
                .collect::<alloc::vec::Vec<_>>()
                .join("\n");
            let ok = text.contains(needle);
            crate::serial::print_str("[web]   ");
            crate::serial::print_str(if ok { "ok   " } else { "FAIL " });
            crate::serial::print_str(what);
            if !ok {
                crate::serial::print_str(" (no `");
                crate::serial::print_str(needle);
                crate::serial::print_str("`)");
                *failures += 1;
            }
            crate::serial::print_str("\n");
        };

        has("sum is 55", "a loop and innerHTML", &mut failures);
        has("ALPHA", "createElement, appendChild and toUpperCase", &mut failures);
        has("BETA", "the second iteration", &mut failures);
        has("Heading", "a heading", &mut failures);
        has("left", "a table cell", &mut failures);
        has("right", "the second column", &mut failures);
        has("two", "the first line of a pre", &mut failures);

        // The script's source must not be prose.  This is the one that decides
        // whether the output is a page or a wall of JavaScript.
        let lines = page.layout(72);
        let text = lines.iter().map(|l| l.text()).collect::<alloc::vec::Vec<_>>().join("\n");
        let mut f = 0usize;
        if text.contains("var s = 0") {
            crate::serial::print_str("[web]   FAIL script source in the output\n");
            f += 1;
        } else {
            crate::serial::print_str("[web]   ok   script source is not prose\n");
        }
        failures += f;

        // And the stylesheet must have reached a run: a `.shout` list item is
        // bold, and nothing else on the page is.
        let mut bold = false;
        for line in page.layout(72).iter() {
            for run in &line.runs {
                if run.style.bold && run.text.contains("ALPHA") {
                    bold = true;
                }
            }
        }
        crate::serial::print_str("[web]   ");
        crate::serial::print_str(if bold { "ok   " } else { "FAIL " });
        crate::serial::print_str("a class from the stylesheet reached a run\n");
        if !bold {
            failures += 1;
        }

        if outcome.ran == 0 {
            crate::serial::print_str("[web]   FAIL the script did not run\n");
            failures += 1;
        }
        if let Some(err) = &outcome.error {
            crate::serial::print_str("[web]   FAIL script error: ");
            crate::serial::print_str(err);
            crate::serial::print_str("\n");
            failures += 1;
        }
    }
    // Before the heap goes: the interpreter keeps prototypes in statics and
    // they are heap allocations like any other.
    js::value::forget_prototypes();
    js::domjs::forget();
    unsafe { crate::mem::heap::reset_to(mark) };
    failures
}

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
