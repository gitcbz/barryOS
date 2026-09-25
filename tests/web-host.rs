// Host-side driver for the page renderer.
//
// The parser, the stylesheet engine and the interpreter are all pure code that
// does not need a framebuffer, a network or a boot to be exercised — and the
// only way to know whether any of them handles real pages is to point them at
// real pages.  So this compiles the kernel's own sources for the host, fetches
// whatever URLs it is given with the host's own network, and prints what the
// page would show.
//
//     tests/run-web-host.sh                     # the built-in cases
//     tests/run-web-host.sh https://example.com/
//
// Three environment variables say more about a page that renders wrongly:
//
//     DUMP=1     every text node, with the display and scale it was computed
//     LINKS=1    every run, with the link it points at
//     IMAGES=1   the built-in cases again, with a stand-in picture table
//
// Everything under test is the kernel's own file, reached by `#[path]`.

extern crate alloc;

/// The kernel's heap, as the page renderer uses it.  A real bump arena is not
/// needed to find out whether the pipeline produces the right text, and the
/// host allocator already frees properly — so these are markers.
mod mem {
    pub mod heap {
        pub fn mark() -> usize { 0 }
        pub unsafe fn reset_to(_m: usize) {}
        pub fn used() -> usize { 0 }
        pub fn free() -> usize { usize::MAX }
    }
}

/// The kernel writes its log to COM1; here it goes to stdout.
mod serial {
    pub fn print_str(s: &str) { print!("{}", s); }
    pub fn print_dec(v: u64) { print!("{}", v); }
    pub fn print_hex(v: u64) { print!("{:x}", v); }
}

#[path = "../kernel/src/apps/web/mod.rs"]
mod web;

/// A page, as plain text, the way the browser would lay it out.
fn render(html: &[u8], cols: usize) -> String {
    render_with(html, cols, &web::layout::NoImages)
}

fn render_with(html: &[u8], cols: usize, imgs: &dyn web::layout::ImageSource) -> String {
    let page = web::Page::parse(html);
    let outcome = page.run_scripts("https://example.invalid/");
    if std::env::var("JS").is_ok() {
        println!(
            "  (scripts: {} found, {} ran, {} external)",
            page.scripts.len(),
            outcome.ran,
            page.script_urls.len()
        );
        if !outcome.log.is_empty() {
            print!("  (log: {})", outcome.log);
        }
        if let Some(e) = &outcome.error {
            println!("  (script error: {})", e);
        }
    }
    let lines = page.layout_with(cols, imgs);
    let mut out = String::new();
    for line in &lines {
        match &line.image {
            Some(b) => out.push_str(&format!("<{}x{} picture {}>", b.cols, b.rows, b.src)),
            None => out.push_str(&line.text()),
        }
        out.push('\n');
    }
    out
}

fn show(name: &str, html: &str) {
    show_bytes(name, html.as_bytes());
}

/// The same, for a page that arrived as bytes: a fetched page is bytes, and
/// the interesting failures are on real pages.
fn show_bytes(name: &str, body: &[u8]) {
    println!("\n===== {} =====", name);

    if std::env::var("DUMP").is_ok() {
        dump_text_nodes(body);
    }
    if std::env::var("LINKS").is_ok() {
        dump_runs(body);
    }
    print!("{}", render(body, 60));
}

/// Every text node, with what the cascade decided about it.
///
/// A page that renders as nothing renders as nothing for one of two reasons,
/// and they are indistinguishable from the output: the text is not in the
/// tree, or the tree says it is invisible.  This is the difference.
fn dump_text_nodes(body: &[u8]) {
    let page = web::Page::parse(body);
    let d = page.dom.borrow();
    let styles = web::css::Stylesheet::parse_all(&page.inline_css());
    let computed = web::css::compute(&d, &styles);
    println!("  root={} {} node(s)", d.root, d.nodes.len());
    let mut shown = 0usize;
    for id in 0..d.nodes.len() {
        let text = match d.kind(id) {
            web::dom::Kind::Text(t) => t.trim().to_string(),
            _ => continue,
        };
        if text.is_empty() {
            continue;
        }
        // The ancestor chain, because a node that is visible on its own can
        // still be inside something that is not: `visibility` and `opacity`
        // are inherited here, and the first hidden ancestor is the answer.
        let mut chain = String::new();
        let mut at = Some(id);
        while let Some(n) = at {
            chain.push_str(d.tag(n).unwrap_or("?"));
            chain.push(':');
            chain.push_str(if computed[n].visible() { "vis" } else { "HID" });
            chain.push('<');
            at = d.node(n).parent;
        }
        println!("  {:#4} [{}] {:?}", id, chain, text.chars().take(40).collect::<String>());
        shown += 1;
        if shown > 40 {
            break;
        }
    }
}

/// Every run, with the link it points at.  A line can hold several links and
/// only the one under the pointer should be clickable.
fn dump_runs(body: &[u8]) {
    let page = web::Page::parse(body);
    let lines = page.layout(60);
    println!("  {} line(s)", lines.len());
    for (i, line) in lines.iter().enumerate() {
        if let Some(b) = &line.image {
            println!("  {:3} picture {} {}x{}", i, b.src, b.cols, b.rows);
            continue;
        }
        for run in &line.runs {
            match &run.link {
                Some(href) => println!("  {:3} link {:?} -> {}", i, run.text, href),
                None if !run.text.trim().is_empty() => println!("  {:3} text {:?}", i, run.text),
                None => {}
            }
        }
    }
}

fn fetch(url: &str) -> Option<Vec<u8>> {
    // A local file is read directly, so a test page can be written and run
    // without a server.
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read(path).ok();
    }
    // Otherwise through the host's own TLS, because what is being tested here
    // is the markup, not the handshake.
    let out = std::process::Command::new("curl")
        .args(["-sS", "--max-time", "30", "-L",
               "-A", "Mozilla/5.0 (compatible; barryOS)", url])
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!("fetch failed: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    Some(out.stdout)
}

/// A stand-in for the browser's picture table.
///
/// The layout engine asks whoever owns the pixels how large a picture should be
/// drawn, so a caller with no decoder can still answer: anything ending in
/// `.png` is in slot 0 and is forty cells by ten.  That is enough to tell a
/// layout that places a picture from one that drops it and shows the `alt`
/// text instead — which is the only difference between the two, and one the
/// rendered text cannot show on its own.
struct FakeImages;

impl web::layout::ImageSource for FakeImages {
    fn resolve(&self, src: &str, avail: usize) -> Option<(usize, usize, usize)> {
        if src.ends_with(".png") {
            Some((0, avail.min(40), 10))
        } else {
            None
        }
    }
}

const CASES: &[(&str, &str)] = &[
    ("plain paragraphs", "<html><body><p>One.</p><p>Two.</p></body></html>"),
    ("headings and em", "<h1>Title</h1><p>A <b>bold</b> word and an <i>italic</i> one.</p>"),
    ("list without end tags",
     "<ul><li>alpha<li>beta<li>gamma</ul><p>after</p>"),
    ("nested markup", "<div><p>a <span>b <em>c</em></span> d</p><p>e</p></div>"),
    ("paragraph closed by a block", "<p>one<div>two</div>three"),
    ("entities", "<p>a &amp; b &lt; c &#65; &#x42; &hellip; &nbsp;done</p>"),
    ("script is not prose",
     "<p>before</p><script>if (a < b) { document.write('&'); }</script><p>after</p>"),
    ("style is not prose", "<style>p { color: red }</style><p>visible</p>"),
    ("table", "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>"),
    ("br and pre", "<p>one<br>two</p><pre>  indented\n  line</pre>"),
    ("comments and doctype", "<!DOCTYPE html><!-- hidden --><p>seen</p>"),
    ("unclosed tags", "<div><p>text<b>bold"),
    ("attributes", "<a href=\"https://example.com/\" class='x y'>link</a>"),
    // Three links on one line, which is where a browser that remembers only
    // the first link of a line becomes visibly wrong.
    ("three links on a line",
     "<p><a href=\"/one\">one</a> and <a href=\"/two\">two</a> and <a href=\"https://x.example/three\">three</a></p>"),
    ("a link that is not one",
     "<p><a name=\"top\">anchor</a> then <a href=\"/x\">a link</a></p>"),
    ("inline markup inside a link",
     "<p>Text with <a href=\"/x\"><b>bold</b> and <i>italic</i></a> inside a link.</p>"),
    // A picture with no size to draw it at is its `alt` text; with one it is a
    // line of its own.
    ("picture without pixels", "<p>before</p><img src=\"a.png\" alt=\"a cat\"><p>after</p>"),
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if std::env::var("SELFTEST").is_ok() {
        let n = web::selftest();
        println!("\n{} failure(s)", n);
        std::process::exit(if n == 0 { 0 } else { 1 });
    }
    let show_pictures = std::env::var("IMAGES").is_ok();
    if args.is_empty() {
        for (name, html) in CASES {
            show_bytes(name, html.as_bytes());
            // The same page again, with a picture table, for the one case
            // whose whole point is the difference.
            if show_pictures && html.contains("<img") {
                print!("{}", render_with(html.as_bytes(), 60, &FakeImages));
            }
        }
        println!("\n{} case(s) rendered", CASES.len());
        return;
    }
    for url in &args {
        match fetch(url) {
            Some(body) => {
                show_bytes(&format!("{} ({} bytes)", url, body.len()), &body);
                let text = render(&body, 78);
                // Only the first screenful: a whole page is not readable in a
                // terminal, and the point is to see whether it reads at all.
                for line in text.lines().take(120) {
                    println!("{}", line);
                }
                println!("-- {} lines total", text.lines().count());
            }
            None => eprintln!("could not fetch {}", url),
        }
    }
}
