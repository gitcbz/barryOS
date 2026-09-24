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
    let lines = page.layout(cols);
    let mut out = String::new();
    for line in &lines {
        out.push_str(&line.text());
        out.push('\n');
    }
    out
}

fn show(name: &str, html: &str) {
    println!("\n===== {} =====", name);
    if std::env::var("DUMP").is_ok() {
        let page = web::Page::parse(html.as_bytes());
        let d = page.dom.borrow();
        let styles = web::css::Stylesheet::parse_all(&page.inline_css());
        let computed = web::css::compute(&d, &styles);
        for id in 0..d.nodes.len() {
            let s = &computed[id];
            println!(
                "  {:#4} {:<28} display={:?} ws={:?} bold={} indent={} space={}/{}",
                id,
                format!("{:?}", d.kind(id)).chars().take(28).collect::<String>(),
                s.display, s.white_space, s.bold, s.indent, s.space_before, s.space_after
            );
        }
    }
    print!("{}", render(html.as_bytes(), 60));
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
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if std::env::var("SELFTEST").is_ok() {
        let n = web::selftest();
        println!("
{} failure(s)", n);
        std::process::exit(if n == 0 { 0 } else { 1 });
    }
    if args.is_empty() {
        for (name, html) in CASES {
            show(name, html);
        }
        println!("\n{} case(s) rendered", CASES.len());
        return;
    }
    for url in &args {
        match fetch(url) {
            Some(body) => {
                println!("\n===== {} ({} bytes) =====", url, body.len());
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
