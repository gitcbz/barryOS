//! The DOM, as a script sees it.
//!
//! Every element a script touches is wrapped in a JavaScript object backed by
//! the document tree.  Wrapping is cached, so `a === b` holds for the same
//! element and a page that looks up its container a thousand times does not
//! make a thousand objects.
//!
//! Properties are dispatched rather than stored: `innerHTML` is not a field,
//! it is a parse of whatever is in the tree, and `children` is not a list, it
//! is a view.  Storing them would mean keeping two copies of the document in
//! step, which is the bug this design exists to avoid.
//!
//! What is not here: layout queries (`offsetWidth`, `getBoundingClientRect`),
//! which need a layout pass a script can wait for; `MutationObserver`; and
//! events other than the ones this browser generates.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use super::super::dom::{self, Dom, Kind};
use super::interp::Interp;
use super::value::{self, Obj, ObjKind, Value};

/// The document a running script can see.
///
/// The kernel has one interpreter and one document at a time, so this lives in
/// a static rather than being threaded through every native function.  It is
/// set on the way into a page's scripts and cleared on the way out, so nothing
/// survives that could point at a document that has been replaced.
pub struct DomHost {
    pub dom: Rc<RefCell<Dom>>,
    wrappers: Vec<Option<Value>>,
    styles: Vec<Option<Value>>,
    pub timers: Vec<Value>,
}

impl DomHost {
    pub fn new(dom: Rc<RefCell<Dom>>) -> DomHost {
        let n = dom.borrow().nodes.len();
        DomHost {
            dom,
            wrappers: vec![None; n],
            styles: vec![None; n],
            timers: Vec::new(),
        }
    }

    fn wrapper(&mut self, id: usize) -> Value {
        while self.wrappers.len() <= id {
            self.wrappers.push(None);
            self.styles.push(None);
        }
        if let Some(v) = &self.wrappers[id] {
            return v.clone();
        }
        let mut o = Obj::plain();
        o.kind = ObjKind::Element(id);
        // The element methods live on a shared prototype rather than on every
        // wrapper: a page with ten thousand elements should not have ten
        // thousand copies of `appendChild`.
        o.proto = element_proto();
        let v = Value::object(o);
        self.wrappers[id] = Some(v.clone());
        v
    }

    fn style_of(&mut self, id: usize) -> Value {
        while self.styles.len() <= id {
            self.wrappers.push(None);
            self.styles.push(None);
        }
        if let Some(v) = &self.styles[id] {
            return v.clone();
        }
        let o = Value::object(Obj::plain());
        self.styles[id] = Some(o.clone());
        o
    }

    /// Write each element's style object back into its `style` attribute, so
    /// the stylesheet engine sees what the script set.
    pub fn flush_styles(&mut self) {
        for id in 0..self.styles.len().min(self.wrappers.len()) {
            let Some(style) = self.styles[id].clone() else { continue };
            let Value::Obj(o) = &style else { continue };
            let props: Vec<(Rc<str>, Value)> = o.borrow().props.iter().cloned().collect();
            if props.is_empty() {
                continue;
            }
            let mut text = String::new();
            for (k, v) in props {
                // A camelCase property is the DOM's spelling; CSS wants dashes.
                let mut css_name = String::new();
                for c in k.chars() {
                    if c.is_ascii_uppercase() {
                        css_name.push('-');
                        css_name.push(c.to_ascii_lowercase());
                    } else {
                        css_name.push(c);
                    }
                }
                let value = match &v {
                    Value::Num(n) => {
                        if *n == 0.0 {
                            "0".to_string()
                        } else {
                            alloc::format!("{}px", n)
                        }
                    }
                    other => other.to_js_string(),
                };
                text.push_str(&alloc::format!("{}:{};", css_name, value));
            }
            let mut d = self.dom.borrow_mut();
            dom::set_attr(&mut d, id, "style", &text);
        }
    }
}

static mut HOST: Option<DomHost> = None;

/// The prototype every element wrapper falls back to.
static mut ELEMENT_PROTO: Option<Value> = None;

fn element_proto() -> Option<Rc<RefCell<Obj>>> {
    let p = unsafe { core::ptr::addr_of!(ELEMENT_PROTO).as_ref() }?.as_ref()?;
    match p {
        Value::Obj(o) => Some(o.clone()),
        _ => None,
    }
}

pub fn with_host<R>(f: impl FnOnce(&mut DomHost) -> R) -> Option<R> {
    let host = unsafe { core::ptr::addr_of_mut!(HOST).as_mut() }?.as_mut()?;
    Some(f(host))
}

pub fn set_host(h: Option<DomHost>) {
    unsafe {
        *core::ptr::addr_of_mut!(HOST) = h;
    }
}

/// The id of the element a method was called on.
fn self_id(this: &Value) -> Option<usize> {
    match this {
        Value::Obj(o) => match o.borrow().kind {
            ObjKind::Element(id) => Some(id),
            _ => None,
        },
        _ => None,
    }
}

/// Make an element wrapper for a node id, for the natives that create one.
fn wrap(id: usize) -> Value {
    with_host(|h| h.wrapper(id)).unwrap_or(Value::Null)
}

/// The document's own node id, for the paths that need a starting point.
fn root_of(dom: &Dom) -> usize {
    dom.root
}

// ---------------------------------------------------------------------------
//  Installation
// ---------------------------------------------------------------------------

pub fn install(it: &mut Interp) {
    value::set_element_hooks(element_get, element_set);
    let g = it.globals.clone();

    // The element methods, made before anything that produces an element.
    let proto = Value::object(Obj::plain());
    unsafe {
        *core::ptr::addr_of_mut!(ELEMENT_PROTO) = Some(proto.clone());
    }
    install_element_methods(&proto);

    let document = Value::object(Obj::plain());
    {
        let d = |name: &str, f: fn(&mut Interp, Value, &[Value]) -> Result<Value, Value>| {
            value::set_prop(&document, &Rc::from(name), Obj::native(f, name));
        };

        d("getElementById", |_it, _t, a| {
            let id = arg(a, 0).to_js_string();
            Ok(with_host(|h| {
                let dom = h.dom.clone();
                let found = dom.borrow().by_id(&id);
                Some(match found {
                    Some(n) => h.wrapper(n),
                    None => Value::Null,
                })
            })
            .flatten()
            .unwrap_or(Value::Null))
        });
        d("querySelector", |_it, _t, a| {
            let sel = arg(a, 0).to_js_string();
            Ok(with_host(|h| {
                let dom = h.dom.clone();
                let found = query_first(&dom.borrow(), &sel);
                Some(match found {
                    Some(n) => h.wrapper(n),
                    None => Value::Null,
                })
            })
            .flatten()
            .unwrap_or(Value::Null))
        });
        d("querySelectorAll", |_it, _t, a| {
            let sel = arg(a, 0).to_js_string();
            Ok(with_host(|h| {
                let dom = h.dom.clone();
                let found = query_all(&dom.borrow(), &sel);
                let items: Vec<Value> = found.into_iter().map(|n| h.wrapper(n)).collect();
                Value::array(items)
            })
            .unwrap_or(Value::Undefined))
        });
        d("getElementsByTagName", |_it, _t, a| {
            let tag = arg(a, 0).to_js_string().to_lowercase();
            Ok(with_host(|h| {
                let dom = h.dom.clone();
                let found = dom.borrow().by_tag(&tag);
                let items: Vec<Value> = found.into_iter().map(|n| h.wrapper(n)).collect();
                Value::array(items)
            })
            .unwrap_or(Value::Undefined))
        });
        d("getElementsByClassName", |_it, _t, a| {
            let want = arg(a, 0).to_js_string();
            Ok(with_host(|h| {
                let dom = h.dom.clone();
                let found: Vec<usize> = {
                    let d = dom.borrow();
                    (0..d.nodes.len())
                        .filter(|&id| {
                            d.attr(id, "class")
                                .unwrap_or("")
                                .split_ascii_whitespace()
                                .any(|c| c == want)
                        })
                        .collect()
                };
                let items: Vec<Value> = found.into_iter().map(|n| h.wrapper(n)).collect();
                Value::array(items)
            })
            .unwrap_or(Value::Undefined))
        });
        d("createElement", |_it, _t, a| {
            let tag = arg(a, 0).to_js_string().to_lowercase();
            Ok(with_host(|h| {
                let parent = {
                    let d = h.dom.borrow();
                    root_of(&d)
                };
                let id = {
                    let mut d = h.dom.borrow_mut();
                    dom::push_element(&mut d, parent, &tag, Vec::new())
                };
                h.wrapper(id)
            })
            .unwrap_or(Value::Null))
        });
        d("createTextNode", |_it, _t, a| {
            let text = arg(a, 0).to_js_string();
            Ok(with_host(|h| {
                let parent = {
                    let d = h.dom.borrow();
                    root_of(&d)
                };
                let id = {
                    let mut d = h.dom.borrow_mut();
                    dom::push_text(&mut d, parent, &text)
                };
                h.wrapper(id)
            })
            .unwrap_or(Value::Null))
        });
        d("write", |_it, _t, a| {
            let text = a.iter().map(|v| v.to_js_string()).collect::<String>();
            with_host(|h| {
                let body = {
                    let d = h.dom.borrow();
                    d.by_tag("body").first().copied().unwrap_or(0)
                };
                let mut d = h.dom.borrow_mut();
                dom::append_html(&mut d, body, &text);
            });
            Ok(Value::Undefined)
        });
        d("writeln", |_it, _t, a| {
            let mut text = a.iter().map(|v| v.to_js_string()).collect::<String>();
            text.push('\n');
            with_host(|h| {
                let body = {
                    let d = h.dom.borrow();
                    d.by_tag("body").first().copied().unwrap_or(0)
                };
                let mut d = h.dom.borrow_mut();
                dom::append_html(&mut d, body, &text);
            });
            Ok(Value::Undefined)
        });
        d("addEventListener", |_it, _t, _a| Ok(Value::Undefined));
        d("removeEventListener", |_it, _t, _a| Ok(Value::Undefined));
        d("createDocumentFragment", |_it, _t, _a| Ok(Value::object(Obj::plain())));
        d("getElementsByName", |_it, _t, a| {
            let want = arg(a, 0).to_js_string();
            Ok(with_host(|h| {
                let dom = h.dom.clone();
                let found: Vec<usize> = {
                    let d = dom.borrow();
                    (0..d.nodes.len())
                        .filter(|&id| d.attr(id, "name") == Some(want.as_str()))
                        .collect()
                };
                let items: Vec<Value> = found.into_iter().map(|n| h.wrapper(n)).collect();
                Value::array(items)
            })
            .unwrap_or(Value::Undefined))
        });
    }

    let body = with_host(|h| {
        let id = {
            let d = h.dom.borrow();
            d.by_tag("body").first().copied().unwrap_or(0)
        };
        h.wrapper(id)
    })
    .unwrap_or(Value::Null);

    value::set_prop(&document, &Rc::from("body"), body);
    value::set_prop(&document, &Rc::from("documentElement"), {
        with_host(|h| {
            let id = {
                let d = h.dom.borrow();
                d.by_tag("html").first().copied().unwrap_or(0)
            };
            h.wrapper(id)
        })
        .unwrap_or(Value::Undefined)
    });
    value::set_prop(&document, &Rc::from("head"), Value::Null);
    value::set_prop(&document, &Rc::from("cookie"), Value::str(""));
    value::set_prop(&document, &Rc::from("readyState"), Value::str("complete"));
    value::set_prop(&document, &Rc::from("hidden"), Value::Bool(false));
    it.define(&g, "document", document.clone());

    // window: an object whose properties are also the globals, which is what
    // it is.  The important ones are lists of the things page scripts call to
    // see whether they are running in a browser at all.
    let window = Value::object(Obj::plain());
    value::set_prop(&window, &Rc::from("innerWidth"), Value::Num(1024.0));
    value::set_prop(&window, &Rc::from("innerHeight"), Value::Num(768.0));
    value::set_prop(&window, &Rc::from("devicePixelRatio"), Value::Num(1.0));
    value::set_prop(&window, &Rc::from("document"), document);
    for name in ["alert", "confirm", "prompt"] {
        let f: fn(&mut Interp, Value, &[Value]) -> Result<Value, Value> = match name {
            "confirm" => |_i, _t, _a| Ok(Value::Bool(false)),
            "prompt" => |_i, _t, _a| Ok(Value::Null),
            _ => |it, _t, a| {
                it.log.push_str(&arg(a, 0).to_js_string());
                it.log.push('\n');
                Ok(Value::Undefined)
            },
        };
        value::set_prop(&window, &Rc::from(name), Obj::native(f, name));
        it.define(&g, name, value::get_prop(&window, &Rc::from(name)));
    }
    for name in ["setTimeout", "setInterval"] {
        let f: fn(&mut Interp, Value, &[Value]) -> Result<Value, Value> = |_it, _t, a| {
            let f = arg(a, 0);
            with_host(|h| h.timers.push(f));
            Ok(Value::Num(0.0))
        };
        value::set_prop(&window, &Rc::from(name), Obj::native(f, name));
        it.define(&g, name, value::get_prop(&window, &Rc::from(name)));
    }
    for name in ["clearTimeout", "clearInterval", "addEventListener",
                 "removeEventListener", "scrollTo", "focus", "blur",
                 "open", "close", "print"] {
        let f: fn(&mut Interp, Value, &[Value]) -> Result<Value, Value> =
            |_i, _t, _a| Ok(Value::Undefined);
        value::set_prop(&window, &Rc::from(name), Obj::native(f, name));
    }
    it.define(&g, "window", window.clone());
    it.define(&g, "self", window.clone());
    it.define(&g, "top", window.clone());
    it.define(&g, "parent", window);

    it.define(&g, "navigator", {
        let n = Value::object(Obj::plain());
        value::set_prop(&n, &Rc::from("userAgent"), Value::str(
            "Mozilla/5.0 (X11; barryOS) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
        ));
        value::set_prop(&n, &Rc::from("language"), Value::str("zh-CN"));
        value::set_prop(&n, &Rc::from("languages"), Value::array(Vec::new()));
        value::set_prop(&n, &Rc::from("platform"), Value::str("barryOS"));
        n
    });
    it.define(&g, "location", {
        let l = Value::object(Obj::plain());
        value::set_prop(&l, &Rc::from("href"), Value::str(""));
        value::set_prop(&l, &Rc::from("protocol"), Value::str("https:"));
        value::set_prop(&l, &Rc::from("host"), Value::str(""));
        value::set_prop(&l, &Rc::from("pathname"), Value::str("/"));
        for name in ["replace", "assign", "reload", "toString"] {
            let f: fn(&mut Interp, Value, &[Value]) -> Result<Value, Value> = |it, t, a| {
                let target = match a.first() {
                    Some(v) if !matches!(v, Value::Undefined) => v.to_js_string(),
                    _ => value::get_prop(&t, &Rc::from("href")).to_js_string(),
                };
                if !target.is_empty() {
                    it.navigate = Some(target);
                }
                Ok(Value::Undefined)
            };
            value::set_prop(&l, &Rc::from(name), Obj::native(f, name));
        }
        l
    });
    it.define(&g, "history", {
        let h = Value::object(Obj::plain());
        value::set_prop(&h, &Rc::from("length"), Value::Num(1.0));
        for name in ["back", "forward", "go", "pushState", "replaceState"] {
            value::set_prop(&h, &Rc::from(name),
                            Obj::native(|_i, _t, _a| Ok(Value::Undefined), name));
        }
        h
    });
    it.define(&g, "screen", {
        let s = Value::object(Obj::plain());
        value::set_prop(&s, &Rc::from("width"), Value::Num(1024.0));
        value::set_prop(&s, &Rc::from("height"), Value::Num(768.0));
        s
    });
    it.define(&g, "localStorage", storage());
    it.define(&g, "sessionStorage", storage());
    it.define(&g, "getComputedStyle", Obj::native(|_i, _t, _a| {
        Ok(Value::object(Obj::plain()))
    }, "getComputedStyle"));
    it.define(&g, "requestAnimationFrame", Obj::native(|it, _t, a| {
        // Run it now rather than later.  There is no frame clock to wait for,
        // and a callback that never runs is worse than one that runs early.
        if let Some(f) = a.first() {
            let _ = it.call(f, Value::Undefined, &[Value::Num(0.0)]);
        }
        Ok(Value::Num(0.0))
    }, "requestAnimationFrame"));
    it.define(&g, "matchMedia", Obj::native(|_it, _t, _a| {
        let o = Value::object(Obj::plain());
        value::set_prop(&o, &Rc::from("matches"), Value::Bool(false));
        value::set_prop(&o, &Rc::from("addListener"),
                        Obj::native(|_i, _t, _a| Ok(Value::Undefined), "addListener"));
        Ok(o)
    }, "matchMedia"));
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).cloned().unwrap_or(Value::Undefined)
}

/// The methods every element has.
///
/// These are what a page needs to build content: make an element, put text in
/// it, put it somewhere.  A page that does only that — which is most pages
/// that are not a framework — renders correctly with no more than this.
fn install_element_methods(proto: &Value) {
    let p = |name: &str, f: fn(&mut Interp, Value, &[Value]) -> Result<Value, Value>| {
        value::set_prop(proto, &Rc::from(name), Obj::native(f, name));
    };

    p("appendChild", |_i, this, a| {
        let Some(parent) = self_id(&this) else { return Ok(Value::Null) };
        let child = arg(a, 0);
        let Some(cid) = self_id_or_text(&child) else { return Ok(child) };
        with_host(|h| {
            let mut d = h.dom.borrow_mut();
            dom::attach(&mut d, cid, parent);
        });
        Ok(child)
    });
    p("append", |_i, this, a| {
        let Some(parent) = self_id(&this) else { return Ok(Value::Undefined) };
        for v in a {
            match v {
                Value::Str(s) => {
                    with_host(|h| {
                        let mut d = h.dom.borrow_mut();
                        dom::push_text(&mut d, parent, s);
                    });
                }
                other => {
                    if let Some(cid) = self_id_or_text(other) {
                        with_host(|h| {
                            let mut d = h.dom.borrow_mut();
                            dom::attach(&mut d, cid, parent);
                        });
                    }
                }
            }
        }
        Ok(Value::Undefined)
    });
    p("insertBefore", |_i, this, a| {
        let Some(parent) = self_id(&this) else { return Ok(Value::Null) };
        let child = arg(a, 0);
        let Some(cid) = self_id_or_text(&child) else { return Ok(child) };
        let before = self_id_or_text(&arg(a, 1));
        with_host(|h| {
            let mut d = h.dom.borrow_mut();
            dom::insert_at(&mut d, cid, parent, before);
        });
        Ok(child)
    });
    p("removeChild", |_i, _this, a| {
        let child = arg(a, 0);
        if let Some(cid) = self_id_or_text(&child) {
            with_host(|h| {
                let mut d = h.dom.borrow_mut();
                dom::detach(&mut d, cid);
            });
        }
        Ok(child)
    });
    p("remove", |_i, this, _a| {
        if let Some(id) = self_id(&this) {
            with_host(|h| {
                let mut d = h.dom.borrow_mut();
                dom::detach(&mut d, id);
            });
        }
        Ok(Value::Undefined)
    });
    p("replaceChild", |_i, this, a| {
        let Some(parent) = self_id(&this) else { return Ok(Value::Null) };
        let new = arg(a, 0);
        let Some(nid) = self_id_or_text(&new) else { return Ok(new) };
        let old = self_id_or_text(&arg(a, 1));
        with_host(|h| {
            let mut d = h.dom.borrow_mut();
            dom::insert_at(&mut d, nid, parent, old);
            if let Some(oid) = old {
                dom::detach(&mut d, oid);
            }
        });
        Ok(new)
    });
    p("insertAdjacentHTML", |_i, this, a| {
        let Some(id) = self_id(&this) else { return Ok(Value::Undefined) };
        let position = arg(a, 0).to_js_string();
        let html = arg(a, 1).to_js_string();
        with_host(|h| {
            let mut d = h.dom.borrow_mut();
            // The four positions are: inside at the start, inside at the end,
            // immediately before this element, immediately after it.  Parsing
            // always appends, so the "at the start" and "before" cases move
            // what was just added.
            let (parent, mark) = match position.as_str() {
                "beforebegin" => (d.node(id).parent.unwrap_or(d.root), Some((id, false))),
                "afterend" => (d.node(id).parent.unwrap_or(d.root), Some((id, true))),
                "afterbegin" => (id, None),
                _ => (id, None),                       // beforeend, and anything else
            };
            let first_new = d.nodes.len();
            dom::append_many_html(&mut d, parent, &html);
            if position == "afterbegin" {
                let added: Vec<usize> = (first_new..d.nodes.len())
                    .filter(|&k| d.node(k).parent == Some(parent))
                    .collect();
                for (i, k) in added.into_iter().enumerate() {
                    dom::move_child_to(&mut d, parent, k, i);
                }
            } else if let Some((anchor, after)) = mark {
                let added: Vec<usize> = (first_new..d.nodes.len())
                    .filter(|&k| d.node(k).parent == Some(parent))
                    .collect();
                let base = dom::index_of_child(&d, parent, anchor).unwrap_or(0);
                for (i, k) in added.into_iter().enumerate() {
                    let want = if after { base + 1 + i } else { base + i };
                    dom::move_child_to(&mut d, parent, k, want);
                }
            }
        });
        Ok(Value::Undefined)
    });
    p("setAttribute", |_i, this, a| {
        let Some(id) = self_id(&this) else { return Ok(Value::Undefined) };
        let name = arg(a, 0).to_js_string().to_lowercase();
        let value = arg(a, 1).to_js_string();
        with_host(|h| {
            let mut d = h.dom.borrow_mut();
            dom::set_attr(&mut d, id, &name, &value);
        });
        Ok(Value::Undefined)
    });
    p("getAttribute", |_i, this, a| {
        let Some(id) = self_id(&this) else { return Ok(Value::Null) };
        let name = arg(a, 0).to_js_string().to_lowercase();
        Ok(with_host(|h| {
            let d = h.dom.borrow();
            match d.attr(id, &name) {
                Some(v) => Value::string(v.to_string()),
                None => Value::Null,
            }
        })
        .unwrap_or(Value::Null))
    });
    p("removeAttribute", |_i, this, a| {
        let Some(id) = self_id(&this) else { return Ok(Value::Undefined) };
        let name = arg(a, 0).to_js_string().to_lowercase();
        with_host(|h| {
            let mut d = h.dom.borrow_mut();
            dom::remove_attr(&mut d, id, &name);
        });
        Ok(Value::Undefined)
    });
    p("hasAttribute", |_i, this, a| {
        let Some(id) = self_id(&this) else { return Ok(Value::Bool(false)) };
        let name = arg(a, 0).to_js_string().to_lowercase();
        Ok(Value::Bool(with_host(|h| {
            h.dom.borrow().attr(id, &name).is_some()
        })
        .unwrap_or(false)))
    });
    p("querySelector", |_i, this, a| {
        let Some(id) = self_id(&this) else { return Ok(Value::Null) };
        let sel = arg(a, 0).to_js_string();
        Ok(with_host(|h| {
            let dom = h.dom.clone();
            let found = {
                let d = dom.borrow();
                let mut found = None;
                for i in 0..d.nodes.len() {
                    if d.tag(i).is_some() && is_under(&d, i, id) && matches_selector(&d, i, &sel) {
                        found = Some(i);
                        break;
                    }
                }
                found
            };
            match found {
                Some(n) => h.wrapper(n),
                None => Value::Null,
            }
        })
        .unwrap_or(Value::Null))
    });
    p("querySelectorAll", |_i, this, a| {
        let Some(id) = self_id(&this) else { return Ok(Value::array(Vec::new())) };
        let sel = arg(a, 0).to_js_string();
        Ok(with_host(|h| {
            let dom = h.dom.clone();
            let found: Vec<usize> = {
                let d = dom.borrow();
                (0..d.nodes.len())
                    .filter(|&i| d.tag(i).is_some() && is_under(&d, i, id) && matches_selector(&d, i, &sel))
                    .collect()
            };
            let items: Vec<Value> = found.into_iter().map(|n| h.wrapper(n)).collect();
            Value::array(items)
        })
        .unwrap_or(Value::Undefined))
    });
    p("getElementsByTagName", |_i, this, a| {
        let Some(id) = self_id(&this) else { return Ok(Value::array(Vec::new())) };
        let tag = arg(a, 0).to_js_string().to_lowercase();
        Ok(with_host(|h| {
            let dom = h.dom.clone();
            let found: Vec<usize> = {
                let d = dom.borrow();
                (0..d.nodes.len())
                    .filter(|&i| (tag == "*" || d.tag(i) == Some(tag.as_str())) && is_under(&d, i, id))
                    .collect()
            };
            let items: Vec<Value> = found.into_iter().map(|n| h.wrapper(n)).collect();
            Value::array(items)
        })
        .unwrap_or(Value::Undefined))
    });
    p("closest", |_i, this, a| {
        let sel = arg(a, 0).to_js_string();
        let mut cur = self_id(&this);
        Ok(with_host(|h| {
            let dom = h.dom.clone();
            let d = dom.borrow();
            while let Some(id) = cur {
                if matches_selector(&d, id, &sel) {
                    return h.wrapper(id);
                }
                cur = d.node(id).parent;
            }
            Value::Null
        })
        .unwrap_or(Value::Null))
    });
    p("matches", |_i, this, a| {
        let Some(id) = self_id(&this) else { return Ok(Value::Bool(false)) };
        let sel = arg(a, 0).to_js_string();
        Ok(Value::Bool(with_host(|h| {
            matches_selector(&h.dom.borrow(), id, &sel)
        })
        .unwrap_or(false)))
    });
    p("contains", |_i, this, a| {
        let Some(id) = self_id(&this) else { return Ok(Value::Bool(false)) };
        let other = self_id_or_text(&arg(a, 0));
        Ok(Value::Bool(with_host(|h| {
            let d = h.dom.borrow();
            match other {
                Some(o) => o == id || is_under(&d, o, id),
                None => false,
            }
        })
        .unwrap_or(false)))
    });
    p("cloneNode", |_i, this, _a| {
        let Some(id) = self_id(&this) else { return Ok(Value::Null) };
        Ok(with_host(|h| {
            let parent = {
                let d = h.dom.borrow();
                d.root
            };
            let copy = {
                let mut d = h.dom.borrow_mut();
                dom::clone_into(&mut d, id, parent)
            };
            h.wrapper(copy)
        })
        .unwrap_or(Value::Null))
    });
    p("remove", |_i, this, _a| {
        if let Some(id) = self_id(&this) {
            with_host(|h| {
                let mut d = h.dom.borrow_mut();
                dom::detach(&mut d, id);
            });
        }
        Ok(Value::Undefined)
    });
    for name in ["focus", "blur", "scrollIntoView", "click", "addEventListener",
                 "removeEventListener", "dispatchEvent", "prepend"] {
        p(name, |_i, _t, _a| Ok(Value::Undefined));
    }
    p("getBoundingClientRect", |_i, _t, _a| {
        let o = Value::object(Obj::plain());
        for k in ["top", "left", "right", "bottom", "width", "height"] {
            value::set_prop(&o, &Rc::from(k), Value::Num(0.0));
        }
        Ok(o)
    });
}

/// Is `id` inside `ancestor`?
fn is_under(d: &Dom, id: usize, ancestor: usize) -> bool {
    let mut cur = d.node(id).parent;
    while let Some(p) = cur {
        if p == ancestor {
            return true;
        }
        cur = d.node(p).parent;
    }
    false
}

/// The node id behind a value, whether it is an element or a text node.
fn self_id_or_text(v: &Value) -> Option<usize> {
    match v {
        Value::Obj(o) => match o.borrow().kind {
            ObjKind::Element(id) => Some(id),
            _ => None,
        },
        _ => None,
    }
}

fn storage() -> Value {
    let s = Value::object(Obj::plain());
    for name in ["getItem", "setItem", "removeItem", "clear", "key"] {
        let f: fn(&mut Interp, Value, &[Value]) -> Result<Value, Value> = match name {
            "getItem" | "key" => |_i, _t, _a| Ok(Value::Null),
            _ => |_i, _t, _a| Ok(Value::Undefined),
        };
        value::set_prop(&s, &Rc::from(name), Obj::native(f, name));
    }
    value::set_prop(&s, &Rc::from("length"), Value::Num(0.0));
    s
}

// ---------------------------------------------------------------------------
//  Property dispatch
// ---------------------------------------------------------------------------

/// Read a property an element does not carry itself.
///
/// Returns `None` for a name that is not a DOM property, so the object's own
/// properties and its prototype still get their turn.
fn element_get(id: usize, key: &str) -> Option<Value> {
    with_host(|h| -> Option<Value> {
        let dom = h.dom.clone();
        let d = dom.borrow();
        let v = match key {
            "innerHTML" => Value::string(inner_html(&d, id)),
            "outerHTML" => Value::string(outer_html(&d, id)),
            "textContent" | "innerText" => Value::string(d.text_content(id)),
            "tagName" | "nodeName" => Value::string(d.tag(id).unwrap_or("").to_uppercase()),
            "id" => Value::string(d.attr(id, "id").unwrap_or("").to_string()),
            "className" => Value::string(d.attr(id, "class").unwrap_or("").to_string()),
            "value" => Value::string(d.attr(id, "value").unwrap_or("").to_string()),
            "href" => Value::string(d.attr(id, "href").unwrap_or("").to_string()),
            "src" => Value::string(d.attr(id, "src").unwrap_or("").to_string()),
            "type" => Value::string(d.attr(id, "type").unwrap_or("").to_string()),
            "name" => Value::string(d.attr(id, "name").unwrap_or("").to_string()),
            "title" => Value::string(d.attr(id, "title").unwrap_or("").to_string()),
            "placeholder" => {
                Value::string(d.attr(id, "placeholder").unwrap_or("").to_string())
            }
            "checked" => Value::Bool(d.attr(id, "checked").is_some()),
            "disabled" => Value::Bool(d.attr(id, "disabled").is_some()),
            "hidden" => Value::Bool(d.attr(id, "hidden").is_some()),
            "nodeType" => Value::Num(1.0),
            "length" => Value::Num(d.node(id).children.len() as f64),
            "selectedIndex" => Value::Num(0.0),
            "offsetWidth" | "offsetHeight" | "clientWidth" | "clientHeight"
            | "scrollHeight" | "scrollTop" | "scrollWidth" => Value::Num(0.0),
            "parentNode" | "parentElement" => match d.node(id).parent {
                Some(p) => h.wrapper(p),
                None => Value::Null,
            },
            "firstChild" | "firstElementChild" => match d.node(id).children.first() {
                Some(&c) => h.wrapper(c),
                None => Value::Null,
            },
            "lastChild" | "lastElementChild" => match d.node(id).children.last() {
                Some(&c) => h.wrapper(c),
                None => Value::Null,
            },
            "nextSibling" | "nextElementSibling" => {
                let parent = d.node(id).parent.unwrap_or(d.root);
                let siblings = d.node(parent).children.clone();
                match siblings.iter().position(|&c| c == id).and_then(|i| siblings.get(i + 1)) {
                    Some(&c) => h.wrapper(c),
                    None => Value::Null,
                }
            }
            "children" | "childNodes" => {
                let items: Vec<Value> =
                    d.node(id).children.iter().map(|&c| h.wrapper(c)).collect();
                Value::array(items)
            }
            "style" => h.style_of(id),
            "classList" => return Some(class_list(h.wrapper(id), id)),
            "dataset" => Value::object(Obj::plain()),
            _ => return None,
        };
        Some(v)
    })
    .flatten()
}

fn element_set(id: usize, key: &str, v: Value) -> bool {
    with_host(|h| -> bool {
        match key {
            "style" => {
                h.style_of(id);
                while h.styles.len() <= id {
                    h.styles.push(None);
                }
                h.styles[id] = Some(v);
                return true;
            }
            _ => {}
        }
        let mut d = h.dom.borrow_mut();
        match key {
            "innerHTML" => {
                let html = v.to_js_string();
                dom::set_inner_html(&mut d, id, &html);
                true
            }
            "textContent" | "innerText" => {
                let text = v.to_js_string();
                dom::set_text_content(&mut d, id, &text);
                true
            }
            "id" | "value" | "href" | "src" | "type" | "name" | "title"
            | "placeholder" | "alt" | "rel" | "action" | "method" => {
                let text = v.to_js_string();
                dom::set_attr(&mut d, id, key, &text);
                true
            }
            "className" => {
                let text = v.to_js_string();
                dom::set_attr(&mut d, id, "class", &text);
                true
            }
            "checked" | "disabled" | "hidden" | "selected" | "readonly" => {
                if v.truthy() {
                    dom::set_attr(&mut d, id, key, "");
                } else {
                    dom::remove_attr(&mut d, id, key);
                }
                true
            }
            _ => false,
        }
    })
    .unwrap_or(false)
}

fn inner_html(d: &Dom, id: usize) -> String {
    let mut out = String::new();
    for &c in &d.node(id).children {
        out.push_str(&outer_html(d, c));
    }
    out
}

fn outer_html(d: &Dom, id: usize) -> String {
    match d.kind(id) {
        Kind::Text(t) => t.clone(),
        Kind::Comment => String::new(),
        Kind::Element { name, attrs } => {
            let mut out = alloc::format!("<{}", name);
            for (k, v) in attrs {
                out.push_str(&alloc::format!(" {}=\"{}\"", k, v));
            }
            out.push('>');
            for &c in &d.node(id).children {
                out.push_str(&outer_html(d, c));
            }
            out.push_str(&alloc::format!("</{}>", name));
            out
        }
        Kind::Document => inner_html(d, id),
    }
}

/// `classList`: four methods over the class attribute.
///
/// The methods are plain functions rather than closures.  A closure that
/// captured the element's id would not be a function pointer, and the object
/// carries the element instead — under a name no page will collide with.
fn class_list(elem: Value, _id: usize) -> Value {
    let o = Value::object(Obj::plain());
    value::set_prop(&o, &Rc::from("__owner"), elem);

    let contains: fn(&mut Interp, Value, &[Value]) -> Result<Value, Value> = |_i, t, a| {
        let class = arg(a, 0).to_js_string();
        let Some(id) = owner_of(&t) else { return Ok(Value::Bool(false)) };
        Ok(Value::Bool(with_host(|h| {
            let d = h.dom.borrow();
            d.attr(id, "class")
                .unwrap_or("")
                .split_ascii_whitespace()
                .any(|c| c == class)
        })
        .unwrap_or(false)))
    };
    value::set_prop(&o, &Rc::from("contains"), Obj::native(contains, "contains"));

    let add: fn(&mut Interp, Value, &[Value]) -> Result<Value, Value> = |_i, t, a| {
        let class = arg(a, 0).to_js_string();
        let Some(id) = owner_of(&t) else { return Ok(Value::Undefined) };
        with_host(|h| {
            let mut d = h.dom.borrow_mut();
            let have = d.attr(id, "class").unwrap_or("").to_string();
            if !have.split_ascii_whitespace().any(|c| c == class) {
                let joined = if have.is_empty() {
                    class
                } else {
                    alloc::format!("{} {}", have, class)
                };
                dom::set_attr(&mut d, id, "class", &joined);
            }
        });
        Ok(Value::Undefined)
    };
    value::set_prop(&o, &Rc::from("add"), Obj::native(add, "add"));

    let remove: fn(&mut Interp, Value, &[Value]) -> Result<Value, Value> = |_i, t, a| {
        let class = arg(a, 0).to_js_string();
        let Some(id) = owner_of(&t) else { return Ok(Value::Undefined) };
        with_host(|h| {
            let mut d = h.dom.borrow_mut();
            let have = d.attr(id, "class").unwrap_or("").to_string();
            let kept: Vec<&str> =
                have.split_ascii_whitespace().filter(|c| *c != class).collect();
            dom::set_attr(&mut d, id, "class", &kept.join(" "));
        });
        Ok(Value::Undefined)
    };
    value::set_prop(&o, &Rc::from("remove"), Obj::native(remove, "remove"));

    let toggle: fn(&mut Interp, Value, &[Value]) -> Result<Value, Value> = |_i, t, a| {
        let class = arg(a, 0).to_js_string();
        let Some(id) = owner_of(&t) else { return Ok(Value::Bool(false)) };
        Ok(Value::Bool(with_host(|h| {
            let mut d = h.dom.borrow_mut();
            let have = d.attr(id, "class").unwrap_or("").to_string();
            let present = have.split_ascii_whitespace().any(|c| c == class);
            if present {
                let kept: Vec<&str> =
                    have.split_ascii_whitespace().filter(|c| *c != class).collect();
                dom::set_attr(&mut d, id, "class", &kept.join(" "));
                false
            } else {
                let joined = if have.is_empty() {
                    class
                } else {
                    alloc::format!("{} {}", have, class)
                };
                dom::set_attr(&mut d, id, "class", &joined);
                true
            }
        })
        .unwrap_or(false)))
    };
    value::set_prop(&o, &Rc::from("toggle"), Obj::native(toggle, "toggle"));
    o
}

/// The element a `classList` belongs to.
fn owner_of(class_list: &Value) -> Option<usize> {
    let owner = value::get_prop(class_list, &Rc::from("__owner"));
    match owner {
        Value::Obj(o) => match o.borrow().kind {
            ObjKind::Element(id) => Some(id),
            _ => None,
        },
        _ => None,
    }
}

// ---------------------------------------------------------------------------
//  Selectors
// ---------------------------------------------------------------------------

/// The first element matching a selector, in document order.
pub fn query_first(d: &Dom, sel: &str) -> Option<usize> {
    query_all(d, sel).into_iter().next()
}

/// Every element matching a selector.
///
/// A selector this does not understand matches nothing, which is the safe way
/// round: one that matched everything would return the wrong element rather
/// than none, and a script cannot tell the difference.
pub fn query_all(d: &Dom, sel: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for id in 0..d.nodes.len() {
        if d.tag(id).is_none() {
            continue;
        }
        if sel.split(',').any(|alt| matches_selector(d, id, alt.trim())) {
            out.push(id);
        }
    }
    out
}

fn matches_selector(d: &Dom, id: usize, sel: &str) -> bool {
    if sel.is_empty() {
        return false;
    }
    if let Some(space) = sel.rfind(' ') {
        let (ancestors, last) = sel.split_at(space);
        if !matches_simple(d, id, last.trim()) {
            return false;
        }
        let mut up = d.node(id).parent;
        while let Some(u) = up {
            if matches_simple(d, u, ancestors.trim()) {
                return true;
            }
            up = d.node(u).parent;
        }
        return false;
    }
    matches_simple(d, id, sel)
}

fn matches_simple(d: &Dom, id: usize, sel: &str) -> bool {
    let rest = sel.trim();
    if rest.is_empty() || rest == "*" {
        return true;
    }
    let bytes = rest.as_bytes();
    let tag_end = rest
        .find(|c| c == '#' || c == '.' || c == '[' || c == ':')
        .unwrap_or(rest.len());
    let tag = &rest[..tag_end];
    if tag != "*"
        && !tag.is_empty()
        && !d.tag(id).map(|t| t.eq_ignore_ascii_case(tag)).unwrap_or(false)
    {
        return false;
    }
    let mut i = tag_end;
    while i < rest.len() {
        match bytes[i] {
            b'#' => {
                let start = i + 1;
                let mut end = start;
                while end < rest.len() && !matches!(bytes[end], b'.' | b'#' | b'[' | b':') {
                    end += 1;
                }
                if d.attr(id, "id") != Some(&rest[start..end]) {
                    return false;
                }
                i = end;
            }
            b'.' => {
                let start = i + 1;
                let mut end = start;
                while end < rest.len() && !matches!(bytes[end], b'.' | b'#' | b'[' | b':') {
                    end += 1;
                }
                let want = &rest[start..end];
                if !d
                    .attr(id, "class")
                    .unwrap_or("")
                    .split_ascii_whitespace()
                    .any(|c| c == want)
                {
                    return false;
                }
                i = end;
            }
            b'[' => {
                let Some(close) = rest[i..].find(']') else { return false };
                let inner = &rest[i + 1..i + close];
                let name = inner.split('=').next().unwrap_or("").trim();
                if d.attr(id, name).is_none() {
                    return false;
                }
                i += close + 1;
            }
            // A pseudo-class is a state this browser does not have, and
            // `:not(...)` needs a selector engine inside the selector engine.
            b':' => return false,
            _ => i += 1,
        }
    }
    true
}
