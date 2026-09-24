//! The built-in library: the objects a script expects to already exist.
//!
//! Written by hand rather than generated, because the useful subset is much
//! smaller than the specification and the boundary is worth being explicit
//! about.  What is here is what page scripts reach for in order to *render*:
//! string and array manipulation, `JSON`, `Math`, and the DOM.  What is not
//! here is `Date`, regular expressions, `Promise`, proxies, and the whole of
//! `Intl` — none of which a page needs to put text on a screen, and all of
//! which would be a large amount of code that nothing would exercise.
//!
//! Everything is installed into an environment once, at the interpreter's
//! construction.  The prototypes are registered globally in `value`, because
//! a string literal has no object to hang methods on until it is used as one.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::format;

use super::interp::{exp_of, ln_of, sqrt_of, Interp};
use super::value::{self, key_of, Obj, ObjKind, Value};

type R = Result<Value, Value>;

/// Shorthand: a native function value.
fn native(f: fn(&mut Interp, Value, &[Value]) -> R, name: &str) -> Value {
    Obj::native(f, name)
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Undefined)
}

fn err(msg: &str) -> Value {
    Value::string(format!("TypeError: {}", msg))
}

// ---------------------------------------------------------------------------
//  Installation
// ---------------------------------------------------------------------------

pub fn install(it: &mut Interp) {
    let g = it.globals.clone();

    // Prototypes first: everything else refers to them.
    let object_proto = Value::object(Obj::plain());
    let array_proto = Value::object(Obj::plain());
    let string_proto = Value::object(Obj::plain());
    let number_proto = Value::object(Obj::plain());
    let boolean_proto = Value::object(Obj::plain());
    let function_proto = Value::object(Obj::plain());

    install_array(&array_proto);
    install_string(&string_proto);
    install_number(&number_proto);
    install_object(&object_proto);
    let _ = &boolean_proto;

    value::set_type_prototypes(
        object_proto.clone(),
        array_proto.clone(),
        string_proto.clone(),
        number_proto,
        boolean_proto,
        function_proto.clone(),
    );

    // --- globals -----------------------------------------------------------

    let object_ctor = constructor("Object", object_proto.clone(), |_it, _this, _args| {
        Ok(Value::object(Obj::plain()))
    });
    value::set_prop(&object_ctor, &Rc::from("keys"), native(|_i, _t, a| {
        let v = arg(a, 0);
        let mut out = Vec::new();
        if let Value::Obj(o) = &v {
            for (k, _) in o.borrow().props.iter() {
                out.push(Value::Str(k.clone()));
            }
        }
        Ok(Value::array(out))
    }, "keys"));
    value::set_prop(&object_ctor, &Rc::from("values"), native(|_i, _t, a| {
        let v = arg(a, 0);
        let mut out = Vec::new();
        if let Value::Obj(o) = &v {
            for (_, val) in o.borrow().props.iter() {
                out.push(val.clone());
            }
        }
        Ok(Value::array(out))
    }, "values"));
    value::set_prop(&object_ctor, &Rc::from("entries"), native(|_i, _t, a| {
        let v = arg(a, 0);
        let mut out = Vec::new();
        if let Value::Obj(o) = &v {
            for (k, val) in o.borrow().props.iter() {
                out.push(Value::array(vec![Value::Str(k.clone()), val.clone()]));
            }
        }
        Ok(Value::array(out))
    }, "entries"));
    value::set_prop(&object_ctor, &Rc::from("assign"), native(|_, _t, a| {
        let target = arg(a, 0);
        for src in a.iter().skip(1) {
            if let Value::Obj(o) = src {
                let b = o.borrow();
                for (k, v) in b.props.iter() {
                    value::set_prop(&target, k, v.clone());
                }
            }
        }
        Ok(target)
    }, "assign"));
    value::set_prop(&object_ctor, &Rc::from("create"), native(|_, _t, a| {
        let mut o = Obj::plain();
        if let Value::Obj(p) = arg(a, 0) {
            o.proto = Some(p);
        }
        Ok(Value::object(o))
    }, "create"));
    value::set_prop(&object_ctor, &Rc::from("freeze"), native(|_, _t, a| Ok(arg(a, 0)), "freeze"));
    value::set_prop(&object_ctor, &Rc::from("getPrototypeOf"), native(|_, _t, a| {
        match arg(a, 0) {
            Value::Obj(o) => Ok(match &o.borrow().proto {
                Some(p) => Value::Obj(p.clone()),
                None => Value::Null,
            }),
            _ => Ok(Value::Null),
        }
    }, "getPrototypeOf"));
    it.define(&g, "Object", object_ctor);

    let array_ctor = constructor("Array", array_proto.clone(), |_it, _this, args| {
        // `new Array(n)` makes n holes; `new Array(a, b)` makes two elements.
        if args.len() == 1 {
            if let Value::Num(n) = args[0] {
                let n = n.max(0.0) as usize;
                if n <= 1_000_000 {
                    return Ok(Value::array(vec![Value::Undefined; n]));
                }
            }
        }
        Ok(Value::array(args.to_vec()))
    });
    value::set_prop(&array_ctor, &Rc::from("isArray"), native(|_, _t, a| {
        Ok(Value::Bool(matches!(&arg(a, 0), Value::Obj(o) if matches!(o.borrow().kind, ObjKind::Array(_)))))
    }, "isArray"));
    it.define(&g, "Array", array_ctor);

    let string_ctor = constructor("String", string_proto.clone(), |_it, _this, args| {
        Ok(Value::string(arg(args, 0).to_js_string()))
    });
    value::set_prop(&string_ctor, &Rc::from("fromCharCode"), native(|_, _t, a| {
        let mut s = String::new();
        for v in a {
            let c = v.to_number() as u32;
            if let Some(ch) = char::from_u32(c) {
                s.push(ch);
            }
        }
        Ok(Value::string(s))
    }, "fromCharCode"));
    it.define(&g, "String", string_ctor);

    let number_ctor = constructor("Number", Value::Undefined, |_it, _this, args| {
        Ok(Value::Num(arg(args, 0).to_number()))
    });
    value::set_prop(&number_ctor, &Rc::from("isInteger"), native(|_, _t, a| {
        match arg(a, 0) {
            Value::Num(n) => Ok(Value::Bool(n.is_finite() && n.fract() == 0.0)),
            _ => Ok(Value::Bool(false)),
        }
    }, "isInteger"));
    value::set_prop(&number_ctor, &Rc::from("isFinite"), native(|_, _t, a| {
        Ok(Value::Bool(match arg(a, 0) {
            Value::Num(n) => n.is_finite(),
            _ => false,
        }))
    }, "isFinite"));
    value::set_prop(&number_ctor, &Rc::from("isNaN"), native(|_, _t, a| {
        Ok(Value::Bool(match arg(a, 0) {
            Value::Num(n) => n.is_nan(),
            _ => false,
        }))
    }, "isNaN"));
    value::set_prop(&number_ctor, &Rc::from("parseInt"), native(nat_parse_int, "parseInt"));
    value::set_prop(&number_ctor, &Rc::from("parseFloat"), native(nat_parse_float, "parseFloat"));
    value::set_prop(&number_ctor, &Rc::from("MAX_SAFE_INTEGER"), Value::Num(9007199254740991.0));
    it.define(&g, "Number", number_ctor);

    it.define(&g, "Boolean", Value::Undefined);
    it.define(&g, "parseInt", native(nat_parse_int, "parseInt"));
    it.define(&g, "parseFloat", native(nat_parse_float, "parseFloat"));
    it.define(&g, "isNaN", native(|_, _t, a| Ok(Value::Bool(arg(a, 0).to_number().is_nan())), "isNaN"));
    it.define(&g, "isFinite", native(|_, _t, a| {
        Ok(Value::Bool(arg(a, 0).to_number().is_finite()))
    }, "isFinite"));
    it.define(&g, "encodeURIComponent", native(nat_encode_uri, "encodeURIComponent"));
    it.define(&g, "decodeURIComponent", native(nat_decode_uri, "decodeURIComponent"));
    it.define(&g, "encodeURI", native(nat_encode_uri, "encodeURI"));
    it.define(&g, "decodeURI", native(nat_decode_uri, "decodeURI"));
    it.define(&g, "NaN", Value::Num(f64::NAN));
    it.define(&g, "Infinity", Value::Num(f64::INFINITY));
    it.define(&g, "undefined", Value::Undefined);

    // Math.
    let math = Value::object(Obj::plain());
    value::set_prop(&math, &Rc::from("PI"), Value::Num(core::f64::consts::PI));
    value::set_prop(&math, &Rc::from("E"), Value::Num(core::f64::consts::E));
    value::set_prop(&math, &Rc::from("LN2"), Value::Num(core::f64::consts::LN_2));
    value::set_prop(&math, &Rc::from("LN10"), Value::Num(core::f64::consts::LN_10));
    value::set_prop(&math, &Rc::from("SQRT2"), Value::Num(core::f64::consts::SQRT_2));
    for (name, f) in [
        ("abs", nat_abs as fn(&mut Interp, Value, &[Value]) -> R),
        ("floor", nat_floor),
        ("ceil", nat_ceil),
        ("round", nat_round),
        ("trunc", nat_trunc),
        ("sqrt", nat_sqrt),
        ("cbrt", nat_cbrt),
        ("sign", nat_sign),
        ("log", nat_log),
        ("log2", nat_log2),
        ("log10", nat_log10),
        ("exp", nat_exp),
        ("sin", nat_sin),
        ("cos", nat_cos),
        ("tan", nat_tan),
        ("atan", nat_atan),
        ("atan2", nat_atan2),
        ("sinh", nat_sinh),
        ("cosh", nat_cosh),
        ("tanh", nat_tanh),
        ("log1p", nat_log1p),
        ("expm1", nat_expm1),
        ("hypot", nat_hypot),
        ("fround", nat_fround),
    ] {
        value::set_prop(&math, &Rc::from(name), native(f, name));
    }
    value::set_prop(&math, &Rc::from("pow"), native(|_, _t, a| {
        Ok(Value::Num(powf(arg(a, 0).to_number(), arg(a, 1).to_number())))
    }, "pow"));
    value::set_prop(&math, &Rc::from("min"), native(|_, _t, a| {
        let mut m = f64::INFINITY;
        for v in a {
            let n = v.to_number();
            if n.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            if n < m {
                m = n;
            }
        }
        Ok(Value::Num(m))
    }, "min"));
    value::set_prop(&math, &Rc::from("max"), native(|_, _t, a| {
        let mut m = f64::NEG_INFINITY;
        for v in a {
            let n = v.to_number();
            if n.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            if n > m {
                m = n;
            }
        }
        Ok(Value::Num(m))
    }, "max"));
    value::set_prop(&math, &Rc::from("random"), native(|it, _t, _a| {
        // Seeded from the same source the rest of the kernel uses; a page's
        // use of this is not security-sensitive, and saying so is better than
        // implying otherwise.
        let _ = it;
        Ok(Value::Num(random_f64()))
    }, "random"));
    it.define(&g, "Math", math);

    // JSON.
    let json = Value::object(Obj::plain());
    value::set_prop(&json, &Rc::from("parse"), native(nat_json_parse, "parse"));
    value::set_prop(&json, &Rc::from("stringify"), native(nat_json_stringify, "stringify"));
    it.define(&g, "JSON", json);

    // console.
    let console = Value::object(Obj::plain());
    for name in ["log", "info", "warn", "error", "debug", "trace"] {
        value::set_prop(&console, &Rc::from(name), native(nat_console, name));
    }
    it.define(&g, "console", console);

    it.define(&g, "alert", native(|it, _t, a| {
        let text = arg(a, 0).to_js_string();
        it.log.push_str(&text);
        it.log.push('\n');
        Ok(Value::Undefined)
    }, "alert"));

    super::domjs::install(it);
}

/// Build a constructor: a function object with a `prototype` property, which
/// is what `new` reads to decide what kind of object to make.
fn constructor(name: &str, proto: Value, f: fn(&mut Interp, Value, &[Value]) -> R) -> Value {
    let ctor = native(f, name);
    if !matches!(proto, Value::Undefined) {
        value::set_prop(&ctor, &Rc::from("prototype"), proto);
    }
    ctor
}

// ---------------------------------------------------------------------------
//  Array.prototype
// ---------------------------------------------------------------------------

fn this_array(this: &Value) -> Option<Vec<Value>> {
    match this {
        Value::Obj(o) => match &o.borrow().kind {
            ObjKind::Array(items) => Some(items.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn install_array(proto: &Value) {
    let p = |name: &str, f: fn(&mut Interp, Value, &[Value]) -> R| {
        value::set_prop(proto, &Rc::from(name), native(f, name));
    };

    p("push", |_it, t, a| {
        if let Value::Obj(o) = &t {
            let mut b = o.borrow_mut();
            if let ObjKind::Array(items) = &mut b.kind {
                for v in a {
                    items.push(v.clone());
                }
                return Ok(Value::Num(items.len() as f64));
            }
        }
        Ok(Value::Undefined)
    });
    p("pop", |_it, t, _a| {
        if let Value::Obj(o) = &t {
            let mut b = o.borrow_mut();
            if let ObjKind::Array(items) = &mut b.kind {
                return Ok(items.pop().unwrap_or(Value::Undefined));
            }
        }
        Ok(Value::Undefined)
    });
    p("shift", |_it, t, _a| {
        if let Value::Obj(o) = &t {
            let mut b = o.borrow_mut();
            if let ObjKind::Array(items) = &mut b.kind {
                if items.is_empty() {
                    return Ok(Value::Undefined);
                }
                return Ok(items.remove(0));
            }
        }
        Ok(Value::Undefined)
    });
    p("unshift", |_it, t, a| {
        if let Value::Obj(o) = &t {
            let mut b = o.borrow_mut();
            if let ObjKind::Array(items) = &mut b.kind {
                for (i, v) in a.iter().enumerate() {
                    items.insert(i, v.clone());
                }
                return Ok(Value::Num(items.len() as f64));
            }
        }
        Ok(Value::Undefined)
    });
    p("join", |_it, t, a| {
        let sep = match arg(a, 0) {
            Value::Undefined => ",".to_string(),
            other => other.to_js_string(),
        };
        let Some(items) = this_array(&t) else { return Ok(Value::str("")) };
        let mut s = String::new();
        for (i, v) in items.iter().enumerate() {
            if i > 0 {
                s.push_str(&sep);
            }
            if !matches!(v, Value::Undefined | Value::Null) {
                s.push_str(&v.to_js_string());
            }
        }
        Ok(Value::string(s))
    });
    p("slice", |_it, t, a| {
        let Some(items) = this_array(&t) else { return Ok(Value::array(Vec::new())) };
        let n = items.len() as i64;
        let start = clamp_index(arg(a, 0), n, 0);
        let end = if args_len(a) > 1 { clamp_index(arg(a, 1), n, n) } else { n };
        Ok(Value::array(items[start.max(0) as usize..end.max(0) as usize].to_vec()))
    });
    p("splice", |_it, t, a| {
        let Some(items) = this_array(&t) else { return Ok(Value::array(Vec::new())) };
        let n = items.len() as i64;
        let start = clamp_index(arg(a, 0), n, 0).max(0) as usize;
        let delete = if args_len(a) > 1 {
            arg(a, 1).to_number().max(0.0) as usize
        } else {
            items.len().saturating_sub(start)
        };
        let delete = delete.min(items.len().saturating_sub(start));
        let removed: Vec<Value> = items[start..start + delete].to_vec();
        if let Value::Obj(o) = &t {
            let mut b = o.borrow_mut();
            if let ObjKind::Array(items) = &mut b.kind {
                for _ in 0..delete {
                    if start < items.len() {
                        items.remove(start);
                    }
                }
                for (i, v) in a.iter().skip(2).enumerate() {
                    items.insert((start + i).min(items.len()), v.clone());
                }
            }
        }
        Ok(Value::array(removed))
    });
    p("indexOf", |_it, t, a| {
        let Some(items) = this_array(&t) else { return Ok(Value::Num(-1.0)) };
        let want = arg(a, 0);
        let from = arg(a, 1).to_number().max(0.0) as usize;
        for (i, v) in items.iter().enumerate().skip(from) {
            if v.strict_equals(&want) {
                return Ok(Value::Num(i as f64));
            }
        }
        Ok(Value::Num(-1.0))
    });
    p("includes", |_it, t, a| {
        let Some(items) = this_array(&t) else { return Ok(Value::Bool(false)) };
        let want = arg(a, 0);
        for v in items.iter() {
            if v.strict_equals(&want) || (v.to_number().is_nan() && want.to_number().is_nan()) {
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    });
    p("concat", |_it, t, a| {
        let mut out = this_array(&t).unwrap_or_default();
        for v in a {
            match v {
                Value::Obj(o) if matches!(o.borrow().kind, ObjKind::Array(_)) => {
                    out.extend(this_array(v).unwrap_or_default());
                }
                other => out.push(other.clone()),
            }
        }
        Ok(Value::array(out))
    });
    p("reverse", |_it, t, _a| {
        if let Value::Obj(o) = &t {
            let mut b = o.borrow_mut();
            if let ObjKind::Array(items) = &mut b.kind {
                items.reverse();
            }
        }
        Ok(t)
    });
    p("sort", |it, t, a| {
        let Some(mut items) = this_array(&t) else { return Ok(t) };
        let cmp = arg(a, 0);
        if cmp.is_callable() {
            // An insertion sort: the comparator can be any script, and a page's
            // arrays are small.
            for i in 1..items.len() {
                let mut j = i;
                while j > 0 {
                    let r = it.call(&cmp, Value::Undefined, &[items[j - 1].clone(), items[j].clone()])?;
                    if r.to_number() > 0.0 {
                        items.swap(j - 1, j);
                        j -= 1;
                    } else {
                        break;
                    }
                }
            }
        } else {
            items.sort_by(|a, b| {
                a.to_js_string().partial_cmp(&b.to_js_string()).unwrap_or(core::cmp::Ordering::Equal)
            });
        }
        if let Value::Obj(o) = &t {
            if let ObjKind::Array(slot) = &mut o.borrow_mut().kind {
                *slot = items;
            }
        }
        Ok(t)
    });
    for (name, keep) in [("map", true), ("filter", false), ("forEach", false)] {
        let _ = keep;
        let f: fn(&mut Interp, Value, &[Value]) -> R = match name {
            "map" => |it, t, a| {
                let Some(items) = this_array(&t) else { return Ok(Value::array(Vec::new())) };
                let cb = arg(a, 0);
                let mut out = Vec::with_capacity(items.len());
                for (i, v) in items.iter().enumerate() {
                    out.push(it.call(&cb, Value::Undefined, &[v.clone(), Value::Num(i as f64), t.clone()])?);
                }
                Ok(Value::array(out))
            },
            "filter" => |it, t, a| {
                let Some(items) = this_array(&t) else { return Ok(Value::array(Vec::new())) };
                let cb = arg(a, 0);
                let mut out = Vec::new();
                for (i, v) in items.iter().enumerate() {
                    let keep = it.call(&cb, Value::Undefined, &[v.clone(), Value::Num(i as f64), t.clone()])?;
                    if keep.truthy() {
                        out.push(v.clone());
                    }
                }
                Ok(Value::array(out))
            },
            _ => |it, t, a| {
                let Some(items) = this_array(&t) else { return Ok(Value::Undefined) };
                let cb = arg(a, 0);
                for (i, v) in items.iter().enumerate() {
                    it.call(&cb, Value::Undefined, &[v.clone(), Value::Num(i as f64), t.clone()])?;
                }
                Ok(Value::Undefined)
            },
        };
        value::set_prop(proto, &Rc::from(name), native(f, name));
    }
    p("reduce", |it, t, a| {
        let Some(items) = this_array(&t) else { return Ok(Value::Undefined) };
        let cb = arg(a, 0);
        let (mut acc, start) = if args_len(a) > 1 {
            (arg(a, 1), 0)
        } else if !items.is_empty() {
            (items[0].clone(), 1)
        } else {
            return Err(err("reduce of an empty array with no initial value"));
        };
        for (i, v) in items.iter().enumerate().skip(start) {
            acc = it.call(&cb, Value::Undefined, &[acc, v.clone(), Value::Num(i as f64), t.clone()])?;
        }
        Ok(acc)
    });
    p("find", |it, t, a| {
        let Some(items) = this_array(&t) else { return Ok(Value::Undefined) };
        let cb = arg(a, 0);
        for (i, v) in items.iter().enumerate() {
            let r = it.call(&cb, Value::Undefined, &[v.clone(), Value::Num(i as f64), t.clone()])?;
            if r.truthy() {
                return Ok(v.clone());
            }
        }
        Ok(Value::Undefined)
    });
    p("findIndex", |it, t, a| {
        let Some(items) = this_array(&t) else { return Ok(Value::Num(-1.0)) };
        let cb = arg(a, 0);
        for (i, v) in items.iter().enumerate() {
            let r = it.call(&cb, Value::Undefined, &[v.clone(), Value::Num(i as f64), t.clone()])?;
            if r.truthy() {
                return Ok(Value::Num(i as f64));
            }
        }
        Ok(Value::Num(-1.0))
    });
    p("some", |it, t, a| {
        let Some(items) = this_array(&t) else { return Ok(Value::Bool(false)) };
        let cb = arg(a, 0);
        for (i, v) in items.iter().enumerate() {
            if it.call(&cb, Value::Undefined, &[v.clone(), Value::Num(i as f64), t.clone()])?.truthy() {
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    });
    p("every", |it, t, a| {
        let Some(items) = this_array(&t) else { return Ok(Value::Bool(true)) };
        let cb = arg(a, 0);
        for (i, v) in items.iter().enumerate() {
            if !it.call(&cb, Value::Undefined, &[v.clone(), Value::Num(i as f64), t.clone()])?.truthy() {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(true))
    });
    p("flat", |_it, t, a| {
        let depth = if args_len(a) > 0 { arg(a, 0).to_number() } else { 1.0 };
        fn flatten(items: &[Value], depth: f64, out: &mut Vec<Value>) {
            for v in items {
                match v {
                    Value::Obj(o) if depth >= 1.0 && matches!(o.borrow().kind, ObjKind::Array(_)) => {
                        let inner = this_array(v).unwrap_or_default();
                        flatten(&inner, depth - 1.0, out);
                    }
                    other => out.push(other.clone()),
                }
            }
        }
        let mut out = Vec::new();
        flatten(&this_array(&t).unwrap_or_default(), depth, &mut out);
        Ok(Value::array(out))
    });
}

fn args_len(a: &[Value]) -> usize {
    a.len()
}

fn clamp_index(v: Value, len: i64, default: i64) -> i64 {
    match v {
        Value::Undefined => default,
        other => {
            let n = other.to_number();
            if n.is_nan() {
                default
            } else if n < 0.0 {
                (len + n as i64).max(0)
            } else {
                (n as i64).min(len)
            }
        }
    }
}

// ---------------------------------------------------------------------------
//  String.prototype
// ---------------------------------------------------------------------------

fn this_string(this: &Value) -> String {
    match this {
        Value::Str(s) => s.to_string(),
        other => other.display(),
    }
}

fn install_string(proto: &Value) {
    let p = |name: &str, f: fn(&mut Interp, Value, &[Value]) -> R| {
        value::set_prop(proto, &Rc::from(name), native(f, name));
    };

    p("charAt", |_it, t, a| {
        let s = this_string(&t);
        let i = arg(a, 0).to_number().max(0.0) as usize;
        Ok(Value::string(s.chars().nth(i).map(|c| c.to_string()).unwrap_or_default()))
    });
    p("charCodeAt", |_it, t, a| {
        let s = this_string(&t);
        let i = arg(a, 0).to_number().max(0.0) as usize;
        Ok(match s.chars().nth(i) {
            Some(c) => Value::Num(c as u32 as f64),
            None => Value::Num(f64::NAN),
        })
    });
    p("codePointAt", |_it, t, a| {
        let s = this_string(&t);
        let i = arg(a, 0).to_number().max(0.0) as usize;
        Ok(match s.chars().nth(i) {
            Some(c) => Value::Num(c as u32 as f64),
            None => Value::Undefined,
        })
    });
    p("indexOf", |_it, t, a| {
        let s = this_string(&t);
        let needle = arg(a, 0).to_js_string();
        let from = arg(a, 1).to_number().max(0.0) as usize;
        let byte = byte_offset(&s, from);
        Ok(match s[byte.min(s.len())..].find(&needle) {
            Some(i) => Value::Num((s[..byte.min(s.len())].chars().count() + s[byte.min(s.len())..byte + i].chars().count()) as f64),
            None => Value::Num(-1.0),
        })
    });
    p("lastIndexOf", |_it, t, a| {
        let s = this_string(&t);
        let needle = arg(a, 0).to_js_string();
        Ok(match s.rfind(&needle) {
            Some(i) => Value::Num(s[..i].chars().count() as f64),
            None => Value::Num(-1.0),
        })
    });
    p("includes", |_it, t, a| {
        let s = this_string(&t);
        Ok(Value::Bool(s.contains(&arg(a, 0).to_js_string())))
    });
    p("startsWith", |_it, t, a| {
        let s = this_string(&t);
        Ok(Value::Bool(s.starts_with(&arg(a, 0).to_js_string())))
    });
    p("endsWith", |_it, t, a| {
        let s = this_string(&t);
        Ok(Value::Bool(s.ends_with(&arg(a, 0).to_js_string())))
    });
    p("slice", |_it, t, a| {
        let s = this_string(&t);
        let chars: Vec<char> = s.chars().collect();
        let n = chars.len() as i64;
        let start = clamp_index(arg(a, 0), n, 0);
        let end = if args_len(a) > 1 { clamp_index(arg(a, 1), n, n) } else { n };
        if start >= end {
            return Ok(Value::str(""));
        }
        Ok(Value::string(chars[start as usize..end as usize].iter().collect()))
    });
    p("substring", |_it, t, a| {
        let s = this_string(&t);
        let chars: Vec<char> = s.chars().collect();
        let n = chars.len() as i64;
        let mut start = arg(a, 0).to_number().max(0.0).min(n as f64) as i64;
        let mut end = if args_len(a) > 1 {
            arg(a, 1).to_number().max(0.0).min(n as f64) as i64
        } else {
            n
        };
        if start > end {
            core::mem::swap(&mut start, &mut end);
        }
        Ok(Value::string(chars[start as usize..end as usize].iter().collect()))
    });
    p("substr", |_it, t, a| {
        let s = this_string(&t);
        let chars: Vec<char> = s.chars().collect();
        let n = chars.len() as i64;
        let mut start = arg(a, 0).to_number() as i64;
        if start < 0 {
            start = (n + start).max(0);
        }
        let len = if args_len(a) > 1 {
            arg(a, 1).to_number().max(0.0) as i64
        } else {
            n - start
        };
        let end = (start + len).min(n).max(start);
        Ok(Value::string(chars[start.min(n) as usize..end.max(0) as usize].iter().collect()))
    });
    p("toUpperCase", |_it, t, _a| Ok(Value::string(this_string(&t).to_uppercase())));
    p("toLowerCase", |_it, t, _a| Ok(Value::string(this_string(&t).to_lowercase())));
    p("trim", |_it, t, _a| Ok(Value::string(this_string(&t).trim().to_string())));
    p("trimStart", |_it, t, _a| Ok(Value::string(this_string(&t).trim_start().to_string())));
    p("trimEnd", |_it, t, _a| Ok(Value::string(this_string(&t).trim_end().to_string())));
    p("repeat", |_it, t, a| {
        let s = this_string(&t);
        let n = arg(a, 0).to_number().max(0.0).min(10_000.0) as usize;
        Ok(Value::string(s.repeat(n)))
    });
    p("padStart", |_it, t, a| pad(&t, a, true));
    p("padEnd", |_it, t, a| pad(&t, a, false));
    p("concat", |_it, t, a| {
        let mut s = this_string(&t);
        for v in a {
            s.push_str(&v.to_js_string());
        }
        Ok(Value::string(s))
    });
    p("split", |_it, t, a| {
        let s = this_string(&t);
        let sep = arg(a, 0);
        let limit = if args_len(a) > 1 {
            arg(a, 1).to_number().max(0.0) as usize
        } else {
            usize::MAX
        };
        let mut out = Vec::new();
        match sep {
            Value::Undefined => out.push(Value::string(s)),
            Value::Str(sep) if sep.is_empty() => {
                for c in s.chars() {
                    out.push(Value::string(c.to_string()));
                }
            }
            other => {
                let sep = other.to_js_string();
                for part in s.split(&sep) {
                    if out.len() >= limit {
                        break;
                    }
                    out.push(Value::string(part.to_string()));
                }
            }
        }
        out.truncate(limit);
        Ok(Value::array(out))
    });
    p("replace", |_it, t, a| {
        let s = this_string(&t);
        let find = arg(a, 0).to_js_string();
        let with = arg(a, 1).to_js_string();
        Ok(match s.find(&find) {
            Some(i) => {
                let mut out = String::new();
                out.push_str(&s[..i]);
                out.push_str(&with);
                out.push_str(&s[i + find.len()..]);
                Value::string(out)
            }
            None => Value::string(s),
        })
    });
    p("replaceAll", |_it, t, a| {
        let s = this_string(&t);
        let find = arg(a, 0).to_js_string();
        let with = arg(a, 1).to_js_string();
        if find.is_empty() {
            return Ok(Value::string(s));
        }
        Ok(Value::string(s.replace(&find, &with)))
    });
    p("at", |_it, t, a| {
        let s = this_string(&t);
        let chars: Vec<char> = s.chars().collect();
        let n = chars.len() as i64;
        let mut i = arg(a, 0).to_number() as i64;
        if i < 0 {
            i += n;
        }
        Ok(chars.get(i.max(0) as usize).map(|c| Value::string(c.to_string())).unwrap_or(Value::Undefined))
    });
    p("localeCompare", |_it, t, a| {
        let s = this_string(&t);
        let o = arg(a, 0).to_js_string();
        Ok(Value::Num(match s.cmp(&o) {
            core::cmp::Ordering::Less => -1.0,
            core::cmp::Ordering::Equal => 0.0,
            core::cmp::Ordering::Greater => 1.0,
        }))
    });
    p("toString", |_it, t, _a| Ok(Value::string(this_string(&t))));
}

fn pad(this: &Value, a: &[Value], at_start: bool) -> R {
    let s = this_string(this);
    let target = arg(a, 0).to_number().max(0.0) as usize;
    let fill = match arg(a, 1) {
        Value::Undefined => " ".to_string(),
        other => other.to_js_string(),
    };
    let have = s.chars().count();
    if have >= target || fill.is_empty() {
        return Ok(Value::string(s));
    }
    let mut padding = String::new();
    while padding.chars().count() < target - have {
        padding.push_str(&fill);
    }
    let padding: String = padding.chars().take(target - have).collect();
    Ok(Value::string(if at_start {
        format!("{}{}", padding, s)
    } else {
        format!("{}{}", s, padding)
    }))
}

fn byte_offset(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map(|(i, _)| i).unwrap_or(s.len())
}

// ---------------------------------------------------------------------------
//  Number.prototype and Object.prototype
// ---------------------------------------------------------------------------

fn install_number(proto: &Value) {
    value::set_prop(proto, &Rc::from("toFixed"), native(|_it, t, a| {
        let n = t.to_number();
        let digits = arg(a, 0).to_number().clamp(0.0, 20.0) as usize;
        if !n.is_finite() {
            return Ok(Value::string(format!("{}", n)));
        }
        let scale = powf(10.0, digits as f64);
        let scaled = (n * scale).round() / scale;
        let text = format!("{:.*}", digits, scaled);
        Ok(Value::string(text))
    }, "toFixed"));
    value::set_prop(proto, &Rc::from("toString"), native(|_it, t, a| {
        let radix = arg(a, 0).to_number() as u32;
        if radix == 16 {
            return Ok(Value::string(format!("{:x}", t.to_number() as i64)));
        }
        if radix == 2 {
            return Ok(Value::string(format!("{:b}", t.to_number() as i64)));
        }
        Ok(Value::string(t.display()))
    }, "toString"));
    value::set_prop(proto, &Rc::from("valueOf"), native(|_it, t, _a| Ok(Value::Num(t.to_number())), "valueOf"));
}

fn install_object(proto: &Value) {
    value::set_prop(proto, &Rc::from("toString"), native(|_it, t, _a| {
        let tag = match &t {
            Value::Obj(o) => match o.borrow().kind {
                ObjKind::Array(_) => "Array",
                ObjKind::Element(_) => "HTMLElement",
                ObjKind::Function(_) | ObjKind::Native(_) => "Function",
                _ => "Object",
            },
            Value::Str(_) => "String",
            Value::Num(_) => "Number",
            Value::Bool(_) => "Boolean",
            Value::Null => return Ok(Value::str("[object Null]")),
            Value::Undefined => return Ok(Value::str("[object Undefined]")),
        };
        Ok(Value::string(format!("[object {}]", tag)))
    }, "toString"));
    value::set_prop(proto, &Rc::from("hasOwnProperty"), native(|_it, t, a| {
        let key = key_of(&arg(a, 0));
        if let Value::Obj(o) = &t {
            return Ok(Value::Bool(o.borrow().get(&key).is_some()));
        }
        Ok(Value::Bool(false))
    }, "hasOwnProperty"));
    value::set_prop(proto, &Rc::from("valueOf"), native(|_it, t, _a| Ok(t), "valueOf"));
}

// ---------------------------------------------------------------------------
//  Globals
// ---------------------------------------------------------------------------

fn nat_parse_int(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let s = arg(a, 0).to_js_string();
    let radix = match arg(a, 1) {
        Value::Undefined => 0,
        other => other.to_number() as u32,
    };
    let t = s.trim();
    let (neg, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (radix, rest) = if radix == 0 {
        if let Some(r) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            (16, r)
        } else {
            (10, rest)
        }
    } else {
        (radix, rest)
    };
    let mut n: f64 = 0.0;
    let mut any = false;
    for c in rest.chars() {
        match c.to_digit(radix) {
            Some(d) => {
                n = n * radix as f64 + d as f64;
                any = true;
            }
            None => break,
        }
    }
    Ok(if any {
        Value::Num(if neg { -n } else { n })
    } else {
        Value::Num(f64::NAN)
    })
}

fn nat_parse_float(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let s = arg(a, 0).to_js_string();
    let t = s.trim();
    let mut end = 0usize;
    let mut seen_dot = false;
    let mut seen_exp = false;
    for (i, c) in t.char_indices() {
        let ok = if i == 0 && (c == '-' || c == '+') {
            true
        } else if c.is_ascii_digit() {
            true
        } else if c == '.' && !seen_dot && !seen_exp {
            seen_dot = true;
            true
        } else if (c == 'e' || c == 'E') && !seen_exp && i > 0 {
            seen_exp = true;
            true
        } else if (c == '-' || c == '+') && seen_exp && i > 0 {
            true
        } else {
            false
        };
        if !ok {
            break;
        }
        end = i + c.len_utf8();
    }
    Ok(Value::Num(t[..end].parse().unwrap_or(f64::NAN)))
}

fn nat_encode_uri(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let s = arg(a, 0).to_js_string();
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{:02X}", b));
        }
    }
    Ok(Value::string(out))
}

fn nat_decode_uri(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let s = arg(a, 0).to_js_string();
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = core::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Ok(Value::string(String::from_utf8_lossy(&out).to_string()))
}

fn nat_console(it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let mut line = String::new();
    for (i, v) in a.iter().enumerate() {
        if i > 0 {
            line.push(' ');
        }
        match v {
            Value::Str(s) => line.push_str(s),
            other => line.push_str(&other.display()),
        }
    }
    line.push('\n');
    it.log.push_str(&line);
    Ok(Value::Undefined)
}

// --- Math ------------------------------------------------------------------

fn nat_abs(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(arg(a, 0).to_number().abs()))
}
fn nat_floor(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(arg(a, 0).to_number().floor()))
}
fn nat_ceil(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(arg(a, 0).to_number().ceil()))
}
fn nat_round(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    // JavaScript rounds halves toward positive infinity, not away from zero.
    let n = arg(a, 0).to_number();
    Ok(Value::Num((n + 0.5).floor()))
}
fn nat_trunc(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(arg(a, 0).to_number().trunc()))
}
fn nat_sqrt(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(sqrt_of(arg(a, 0).to_number())))
}
fn nat_cbrt(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let n = arg(a, 0).to_number();
    let r = powf(n.abs(), 1.0 / 3.0);
    Ok(Value::Num(if n < 0.0 { -r } else { r }))
}
fn nat_sign(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let n = arg(a, 0).to_number();
    Ok(Value::Num(if n.is_nan() {
        f64::NAN
    } else if n > 0.0 {
        1.0
    } else if n < 0.0 {
        -1.0
    } else {
        n
    }))
}
fn nat_log(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(ln_of(arg(a, 0).to_number())))
}
fn nat_log2(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(ln_of(arg(a, 0).to_number()) / core::f64::consts::LN_2))
}
fn nat_log10(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(ln_of(arg(a, 0).to_number()) / core::f64::consts::LN_10))
}
fn nat_log1p(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(ln_of(1.0 + arg(a, 0).to_number())))
}
fn nat_exp(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(exp_of(arg(a, 0).to_number())))
}
fn nat_expm1(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(exp_of(arg(a, 0).to_number()) - 1.0))
}
fn nat_sin(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(sin_of(arg(a, 0).to_number())))
}
fn nat_cos(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(cos_of(arg(a, 0).to_number())))
}
fn nat_tan(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let x = arg(a, 0).to_number();
    Ok(Value::Num(sin_of(x) / cos_of(x)))
}
fn nat_atan(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(atan_of(arg(a, 0).to_number())))
}
fn nat_atan2(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let y = arg(a, 0).to_number();
    let x = arg(a, 1).to_number();
    let base = atan_of(y / x);
    Ok(Value::Num(if x >= 0.0 {
        base
    } else if y >= 0.0 {
        base + core::f64::consts::PI
    } else {
        base - core::f64::consts::PI
    }))
}
fn nat_sinh(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let x = arg(a, 0).to_number();
    Ok(Value::Num((exp_of(x) - exp_of(-x)) / 2.0))
}
fn nat_cosh(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let x = arg(a, 0).to_number();
    Ok(Value::Num((exp_of(x) + exp_of(-x)) / 2.0))
}
fn nat_tanh(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let x = arg(a, 0).to_number();
    let (e, n) = (exp_of(x), exp_of(-x));
    Ok(Value::Num((e - n) / (e + n)))
}
fn nat_hypot(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let mut sum = 0.0;
    for v in a {
        let n = v.to_number();
        sum += n * n;
    }
    Ok(Value::Num(sqrt_of(sum)))
}
fn nat_fround(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    Ok(Value::Num(arg(a, 0).to_number() as f32 as f64))
}

/// The trigonometric functions, by range reduction and Taylor series.
///
/// No libm in a kernel, and none of the platforms this runs on have one.  The
/// series converges quickly after the argument is brought into [-pi, pi] and
/// then into [-pi/2, pi/2] using the symmetries of the function.
fn sin_of(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let two_pi = 2.0 * core::f64::consts::PI;
    let mut r = x - two_pi * (x / two_pi).round();
    let pi = core::f64::consts::PI;
    let mut sign = 1.0;
    if r > pi / 2.0 {
        r = pi - r;
    } else if r < -pi / 2.0 {
        r = -pi - r;
        sign = 1.0;
    }
    let mut term = r;
    let mut sum = r;
    let x2 = r * r;
    for n in 1..14 {
        term *= -x2 / (((2 * n) * (2 * n + 1)) as f64);
        sum += term;
    }
    sign * sum
}

fn cos_of(x: f64) -> f64 {
    sin_of(x + core::f64::consts::PI / 2.0)
}

fn atan_of(x: f64) -> f64 {
    if !x.is_finite() {
        return if x > 0.0 { core::f64::consts::PI / 2.0 } else { -core::f64::consts::PI / 2.0 };
    }
    let neg = x < 0.0;
    let x = x.abs();
    // atan(x) = pi/2 - atan(1/x) for x > 1, which keeps the series converging.
    if x > 1.0 {
        let r = core::f64::consts::PI / 2.0 - atan_series(1.0 / x);
        return if neg { -r } else { r };
    }
    let r = atan_series(x);
    if neg {
        -r
    } else {
        r
    }
}

fn atan_series(x: f64) -> f64 {
    let mut term = x;
    let mut sum = x;
    let x2 = x * x;
    for n in 1..60 {
        term *= -x2;
        sum += term / (2 * n + 1) as f64;
    }
    sum
}

/// `Math.pow`, which `core` does not provide for floats.
pub fn powf(base: f64, exp: f64) -> f64 {
    if exp == 0.0 {
        return 1.0;
    }
    if base == 0.0 {
        return 0.0;
    }
    if base < 0.0 && exp.fract() == 0.0 {
        let r = powf(-base, exp);
        return if (exp as i64) % 2 == 0 { r } else { -r };
    }
    if base < 0.0 {
        return f64::NAN;
    }
    exp_of(ln_of(base) * exp)
}

// ---------------------------------------------------------------------------
//  JSON
// ---------------------------------------------------------------------------

fn nat_json_stringify(it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let v = arg(a, 0);
    let mut out = String::new();
    write_json(it, &v, &mut out, 0)?;
    Ok(Value::string(out))
}

fn write_json(it: &mut Interp, v: &Value, out: &mut String, depth: u32) -> Result<(), Value> {
    if depth > 64 {
        return Err(err("object is too deep to serialise"));
    }
    match v {
        Value::Null => out.push_str("null"),
        Value::Undefined => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => {
            if n.is_finite() {
                out.push_str(&format_number(*n));
            } else {
                out.push_str("null");
            }
        }
        Value::Str(s) => write_json_string(s, out),
        Value::Obj(o) => {
            // A `toJSON` method wins, which is how dates and the like publish
            // themselves.
            let to_json = value::get_prop(v, &Rc::from("toJSON"));
            if to_json.is_callable() {
                let r = it.call(&to_json, v.clone(), &[])?;
                return write_json(it, &r, out, depth + 1);
            }
            let is_array = matches!(o.borrow().kind, ObjKind::Array(_));
            if is_array {
                out.push('[');
                let items = this_array(v).unwrap_or_default();
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json(it, item, out, depth + 1)?;
                }
                out.push(']');
            } else {
                out.push('{');
                let props: Vec<(Rc<str>, Value)> =
                    o.borrow().props.iter().cloned().collect();
                let mut first = true;
                for (k, val) in props {
                    if matches!(val, Value::Undefined) {
                        continue;
                    }
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    write_json_string(&k, out);
                    out.push(':');
                    write_json(it, &val, out, depth + 1)?;
                }
                out.push('}');
            }
        }
    }
    Ok(())
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn nat_json_parse(_it: &mut Interp, _t: Value, a: &[Value]) -> R {
    let s = arg(a, 0).to_js_string();
    let b: Vec<char> = s.chars().collect();
    let mut at = 0usize;
    let v = parse_json(&b, &mut at)?;
    Ok(v)
}

fn parse_json(b: &[char], at: &mut usize) -> R {
    skip_ws(b, at);
    if *at >= b.len() {
        return Err(err("unexpected end of JSON"));
    }
    match b[*at] {
        'n' => {
            expect_word(b, at, "null")?;
            Ok(Value::Null)
        }
        't' => {
            expect_word(b, at, "true")?;
            Ok(Value::Bool(true))
        }
        'f' => {
            expect_word(b, at, "false")?;
            Ok(Value::Bool(false))
        }
        '"' => Ok(Value::string(parse_json_string(b, at)?)),
        '[' => {
            *at += 1;
            let mut items = Vec::new();
            skip_ws(b, at);
            if *at < b.len() && b[*at] == ']' {
                *at += 1;
                return Ok(Value::array(items));
            }
            loop {
                items.push(parse_json(b, at)?);
                skip_ws(b, at);
                if *at < b.len() && b[*at] == ',' {
                    *at += 1;
                    continue;
                }
                break;
            }
            if *at < b.len() && b[*at] == ']' {
                *at += 1;
            }
            Ok(Value::array(items))
        }
        '{' => {
            *at += 1;
            let mut o = Obj::plain();
            skip_ws(b, at);
            if *at < b.len() && b[*at] == '}' {
                *at += 1;
                return Ok(Value::object(o));
            }
            loop {
                skip_ws(b, at);
                let key = parse_json_string(b, at)?;
                skip_ws(b, at);
                if *at >= b.len() || b[*at] != ':' {
                    return Err(err("expected `:` in JSON"));
                }
                *at += 1;
                let v = parse_json(b, at)?;
                o.set(Rc::from(key.as_str()), v);
                skip_ws(b, at);
                if *at < b.len() && b[*at] == ',' {
                    *at += 1;
                    continue;
                }
                break;
            }
            if *at < b.len() && b[*at] == '}' {
                *at += 1;
            }
            Ok(Value::object(o))
        }
        _ => {
            let start = *at;
            while *at < b.len()
                && (b[*at].is_ascii_digit() || "+-.eE".contains(b[*at]))
            {
                *at += 1;
            }
            let text: String = b[start..*at].iter().collect();
            text.parse::<f64>()
                .map(Value::Num)
                .map_err(|_| err("not a number in JSON"))
        }
    }
}

fn skip_ws(b: &[char], at: &mut usize) {
    while *at < b.len() && b[*at].is_whitespace() {
        *at += 1;
    }
}

fn expect_word(b: &[char], at: &mut usize, word: &str) -> R {
    let n = word.chars().count();
    if *at + n > b.len() {
        return Err(err("unexpected end of JSON"));
    }
    for (i, c) in word.chars().enumerate() {
        if b[*at + i] != c {
            return Err(err("unexpected token in JSON"));
        }
    }
    *at += n;
    Ok(Value::Undefined)
}

fn parse_json_string(b: &[char], at: &mut usize) -> Result<String, Value> {
    if *at >= b.len() || b[*at] != '"' {
        return Err(err("expected a string in JSON"));
    }
    *at += 1;
    let mut out = String::new();
    while *at < b.len() {
        let c = b[*at];
        if c == '"' {
            *at += 1;
            return Ok(out);
        }
        if c == '\\' && *at + 1 < b.len() {
            *at += 1;
            let e = b[*at];
            *at += 1;
            match e {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => {
                    let hex: String = b[*at..(*at + 4).min(b.len())].iter().collect();
                    if let Ok(v) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(v) {
                            out.push(ch);
                        }
                    }
                    *at += 4;
                }
                other => out.push(other),
            }
            continue;
        }
        out.push(c);
        *at += 1;
    }
    Err(err("unterminated string in JSON"))
}

use super::parser::format_number;

/// A number in [0, 1).  From the kernel's own source of variation where there
/// is one, and from a counter where there is not.
fn random_f64() -> f64 {
    #[cfg(target_os = "none")]
    {
        let mut b = [0u8; 4];
        crate::crypto::tls::random_bytes(&mut b);
        return u32::from_le_bytes(b) as f64 / 4294967296.0;
    }
    #[cfg(not(target_os = "none"))]
    {
        use core::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0x2545F4914F6CDD1D);
        let x = N.fetch_add(0x9E3779B97F4A7C15, Ordering::Relaxed);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        (z >> 11) as f64 / 9007199254740992.0
    }
}
