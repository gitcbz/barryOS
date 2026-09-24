//! JavaScript, for the pages that need it.
//!
//! The shape of this is decided by what it is for: not running programs, but
//! letting a page that generates its own content finish doing so.  A page
//! whose text is in the markup needs none of it; a page whose text is written
//! by a script is blank without it.
//!
//! What that means in practice is that the boundary is the interesting part.
//! Scripts that manipulate strings, build elements, and set `innerHTML` work.
//! Scripts that are a bundled framework — megabytes of classes, promises,
//! module loading, and a virtual DOM — do not, and the honest outcome for
//! those is that their `<noscript>` content is what the reader gets.  This
//! browser shows that, deliberately, rather than showing a page that tried and
//! half-failed.
//!
//! Two guards make running a stranger's code safe enough to do at all: a step
//! budget, so a loop that never ends stops, and a heap floor, so a script that
//! allocates without bound stops.  Neither can be enforced by a machine with
//! one address space and no way to kill a process, so they are enforced by
//! counting.

pub mod ast;
pub mod builtins;
pub mod domjs;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod value;

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use super::dom::Dom;
use interp::Interp;
use value::Value;

/// What running a page's scripts produced.
pub struct Outcome {
    /// A URL the script asked to go to, from `location.href = ...`.
    pub navigate: Option<String>,
    /// Anything the script logged or alerted.
    pub log: String,
    /// How many scripts ran to completion.
    pub ran: usize,
    /// The first error, if one stopped a script.
    pub error: Option<String>,
}

/// Run every script in a page, in order, against its document.
///
/// In order and in one interpreter: scripts share globals, and a page's second
/// script routinely depends on what its first one defined.  A script that
/// fails does not stop the ones after it — a page is more likely to be
/// readable with three of its four scripts run than with none.
pub fn run(dom: Rc<RefCell<Dom>>, scripts: &[String], href: &str) -> Outcome {
    let globals = interp::global_env();
    let mut it = Interp::new(globals);
    builtins::install(&mut it);
    domjs::set_host(Some(domjs::DomHost::new(dom)));

    // `location.href` is how a script asks where it is, and redirect stubs
    // are the main thing that uses it.
    if let Some(loc) = value::lookup(&it.globals, "location") {
        value::set_prop(&loc, &Rc::from("href"), Value::string(href.to_string()));
        if let Some((host, path)) = split_url(href) {
            value::set_prop(&loc, &Rc::from("host"), Value::string(host));
            value::set_prop(&loc, &Rc::from("pathname"), Value::string(path));
        }
    }

    let mut ran = 0usize;
    let mut error = None;
    for src in scripts {
        if src.trim().is_empty() {
            continue;
        }
        match parser::parse(src) {
            Ok(body) => match it.run(&body) {
                Ok(()) => ran += 1,
                Err(e) => {
                    if error.is_none() {
                        error = Some(e.display());
                    }
                    // A script that threw leaves the rest of the page alone;
                    // its own remaining scripts still run, because that is
                    // what a browser does with a script that raised.
                }
            },
            Err((msg, line)) => {
                if error.is_none() {
                    error = Some(alloc::format!("line {}: {}", line, msg));
                }
            }
        }
    }

    // Timers set by the page, run once.  There is no event loop to run them
    // on later, and a callback that never runs is a page that never finishes
    // rendering.
    let timers: Vec<Value> = domjs::with_host(|h| core::mem::take(&mut h.timers)).unwrap_or_default();
    for f in timers.iter().take(32) {
        let _ = it.call(f, Value::Undefined, &[]);
    }

    // What the scripts set through `element.style` has to reach the
    // stylesheet engine, which reads an attribute.
    domjs::with_host(|h| h.flush_styles());
    domjs::set_host(None);

    Outcome {
        navigate: it.navigate.take(),
        log: it.log,
        ran,
        error,
    }
}

fn split_url(url: &str) -> Option<(String, String)> {
    let rest = url.split("://").nth(1)?;
    match rest.find('/') {
        Some(i) => Some((rest[..i].to_string(), rest[i..].to_string())),
        None => Some((rest.to_string(), "/".to_string())),
    }
}
