//! JavaScript values, objects and environments.
//!
//! Objects are reference-counted rather than garbage collected.  That is a
//! deliberate choice with a known cost: a cycle — `a.b = a`, or two objects
//! pointing at each other — is never freed.  The cost is bounded, because
//! everything a page's script allocates is released wholesale when the page is
//! replaced, and the alternative is a tracing collector that has to see the
//! interpreter's Rust stack, which means threading a shadow stack through
//! every function in `interp`.  For a browser whose script support is
//! explicitly best-effort, a bounded leak is the better trade.
//!
//! Strings are `Rc<str>` and freed by the same reference count.  Numbers are
//! `f64` throughout, including array indices, which is what the language does
//! and what makes `a["0"]` and `a[0]` the same thing.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::format;
use core::cell::RefCell;

use super::ast::FnDef;
use super::interp::Interp;
use super::parser::format_number;

#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<str>),
    Obj(Rc<RefCell<Obj>>),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(Rc::from(s))
    }

    pub fn string(s: String) -> Value {
        Value::Str(Rc::from(s.as_str()))
    }

    pub fn object(o: Obj) -> Value {
        Value::Obj(Rc::new(RefCell::new(o)))
    }

    pub fn array(items: Vec<Value>) -> Value {
        Value::object(Obj {
            kind: ObjKind::Array(items),
            props: Vec::new(),
            proto: None,
        })
    }

    pub fn is_object(&self) -> bool {
        matches!(self, Value::Obj(_) | Value::Null)
    }

    pub fn is_callable(&self) -> bool {
        match self {
            Value::Obj(o) => {
                let b = o.borrow();
                matches!(b.kind, ObjKind::Function(_) | ObjKind::Native(_))
            }
            _ => false,
        }
    }

    /// Truthiness: everything is true except the handful of falsy values.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            Value::Obj(_) => true,
        }
    }

    pub fn to_number(&self) -> f64 {
        match self {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            Value::Num(n) => *n,
            Value::Str(s) => string_to_number(s),
            Value::Obj(o) => {
                // An object with a valueOf that returns a primitive uses it,
                // which is how `new Date() - 0` works.
                let b = o.borrow();
                if let ObjKind::Array(items) = &b.kind {
                    return match items.len() {
                        0 => 0.0,
                        1 => items[0].to_number(),
                        _ => f64::NAN,
                    };
                }
                f64::NAN
            }
        }
    }

    /// The type name `typeof` returns.
    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Obj(o) => match o.borrow().kind {
                ObjKind::Function(_) | ObjKind::Native(_) => "function",
                _ => "object",
            },
        }
    }

    /// A description of the value, which is what most output needs.
    pub fn display(&self) -> String {
        match self {
            Value::Undefined => "undefined".to_string(),
            Value::Null => "null".to_string(),
            Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
            Value::Num(n) => format_number(*n),
            Value::Str(s) => s.to_string(),
            Value::Obj(o) => {
                let b = o.borrow();
                match &b.kind {
                    ObjKind::Array(items) => {
                        let mut s = String::new();
                        for (i, v) in items.iter().enumerate() {
                            if i > 0 {
                                s.push(',');
                            }
                            s.push_str(&element_display(v));
                        }
                        s
                    }
                    ObjKind::Function(_) => {
                        "function () { [native code] }".to_string()
                    }
                    ObjKind::Native(_) => "function () { [native code] }".to_string(),
                    ObjKind::Element(_) => "[object HTMLDivElement]".to_string(),
                    _ => {
                        if let Some(s) = b.get(&Rc::from("\u{0}toString")) {
                            if let Value::Str(s) = s {
                                return s.to_string();
                            }
                        }
                        "[object Object]".to_string()
                    }
                }
            }
        }
    }

    /// The result of `String(value)`.
    pub fn to_js_string(&self) -> String {
        self.display()
    }

    /// Are these the same value under `===`?
    pub fn strict_equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) => true,
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Num(a), Value::Num(b)) => {
                // NaN is not equal to itself, which is the one place the
                // language's equality is not reflexive.
                if a.is_nan() || b.is_nan() {
                    false
                } else {
                    a == b
                }
            }
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Obj(a), Value::Obj(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// `==`, with the type coercions the language actually performs.
    pub fn loose_equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Null, Value::Undefined) | (Value::Undefined, Value::Null) => true,
            (Value::Num(_), Value::Str(_)) => self.to_number() == other.to_number(),
            (Value::Str(_), Value::Num(_)) => self.to_number() == other.to_number(),
            (Value::Bool(_), _) => self.to_number() == other.to_number(),
            (_, Value::Bool(_)) => self.to_number() == other.to_number(),
            _ => self.strict_equals(other),
        }
    }
}

fn element_display(v: &Value) -> String {
    match v {
        Value::Undefined | Value::Null => String::new(),
        other => other.display(),
    }
}

/// The number a string denotes, with JavaScript's rules: empty is zero,
/// whitespace is trimmed, and anything else that is not a number is NaN.
pub fn string_to_number(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(rest, 16).map(|v| v as f64).unwrap_or(f64::NAN);
    }
    if t == "Infinity" || t == "+Infinity" {
        return f64::INFINITY;
    }
    if t == "-Infinity" {
        return f64::NEG_INFINITY;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

// ---------------------------------------------------------------------------
//  Objects
// ---------------------------------------------------------------------------

pub struct Obj {
    pub kind: ObjKind,
    /// Own properties, in insertion order.  A `Vec` rather than a map: objects
    /// in page scripts are small, and the ordering is observable.
    pub props: Vec<(Rc<str>, Value)>,
    pub proto: Option<Rc<RefCell<Obj>>>,
}

pub enum ObjKind {
    Plain,
    Array(Vec<Value>),
    Function(Function),
    Native(Native),
    /// A DOM node, by id in the document arena.
    Element(usize),
    /// Something the script threw that is not an Error object.
    Error,
}

type Native = fn(&mut Interp, Value, &[Value]) -> Result<Value, Value>;

pub struct Function {
    pub def: Rc<FnDef>,
    pub env: Env,
    pub this: Value,
}

impl Obj {
    pub fn plain() -> Obj {
        Obj { kind: ObjKind::Plain, props: Vec::new(), proto: None }
    }

    pub fn native(f: Native, name: &str) -> Value {
        let mut o = Obj::plain();
        o.kind = ObjKind::Native(f);
        o.props.push((Rc::from("name"), Value::str(name)));
        Value::object(o)
    }

    pub fn get(&self, key: &Rc<str>) -> Option<Value> {
        for (k, v) in self.props.iter().rev() {
            if k == key {
                return Some(v.clone());
            }
        }
        None
    }

    pub fn set(&mut self, key: Rc<str>, value: Value) {
        for (k, v) in self.props.iter_mut() {
            if *k == key {
                *v = value;
                return;
            }
        }
        self.props.push((key, value));
    }
}

/// The prototype every object of a given kind falls back to.
///
/// A string literal has no object to hang methods on, so `"abc".toUpperCase()`
/// works because looking up a property on a primitive looks in the prototype
/// for its type.  That is what the specification does too — the primitive is
/// *boxed*, and its methods come from `String.prototype`.
///
/// Held as one tuple in a static because there is one interpreter and one set
/// of built-ins; a second set would be a second library.
static mut PROTOS: Option<(Value, Value, Value, Value, Value, Value)> = None;

pub fn set_type_prototypes(
    object: Value,
    array: Value,
    string: Value,
    number: Value,
    boolean: Value,
    function: Value,
) {
    unsafe {
        *core::ptr::addr_of_mut!(PROTOS) = Some((object, array, string, number, boolean, function));
    }
}

fn type_proto(v: &Value) -> Option<Value> {
    let p = unsafe { core::ptr::addr_of!(PROTOS).as_ref() }?.as_ref()?;
    Some(match v {
        Value::Str(_) => p.2.clone(),
        Value::Num(_) => p.3.clone(),
        Value::Bool(_) => p.4.clone(),
        Value::Obj(o) => {
            let b = o.borrow();
            match b.kind {
                ObjKind::Array(_) => p.1.clone(),
                ObjKind::Function(_) | ObjKind::Native(_) => p.5.clone(),
                _ => p.0.clone(),
            }
        }
        _ => return None,
    })
}

/// A DOM property that is computed rather than stored.
///
/// An element object does not carry `innerHTML` in its property list, because
/// there is no one value for it: it is the tree.  The read goes to the getter,
/// and a write goes to the setter.  Both return `None`/`false` for a name that
/// is not a DOM property, so the ordinary paths still run.
///
/// Neither takes the interpreter: the DOM is reached through the host the
/// binder installed, and threading an interpreter through every property read
/// in the engine to serve one case is how a value type acquires a dependency
/// on the thing that evaluates it.
type ElementGetter = fn(usize, &str) -> Option<Value>;
type ElementSetter = fn(usize, &str, Value) -> bool;

static mut ELEMENT_HOOKS: Option<(ElementGetter, ElementSetter)> = None;

pub fn set_element_hooks(g: ElementGetter, s: ElementSetter) {
    unsafe {
        *core::ptr::addr_of_mut!(ELEMENT_HOOKS) = Some((g, s));
    }
}

fn element_get(id: usize, key: &str) -> Option<Value> {
    let (g, _) = unsafe { core::ptr::addr_of!(ELEMENT_HOOKS).as_ref() }?.as_ref()?;
    g(id, key)
}

fn element_set(id: usize, key: &str, v: Value) -> bool {
    let Some((_, s)) = (unsafe { core::ptr::addr_of!(ELEMENT_HOOKS).as_ref() })
        .and_then(|h| h.as_ref())
    else {
        return false;
    };
    s(id, key, v)
}

/// Look a property up, following the prototype chain.
///
/// The chain is the object's own `proto` first, and then the prototype for the
/// value's type — so an array created by the `array` helper, which has no
/// `proto` of its own, still finds `push`.
///
/// `Object.prototype` *is* the prototype for plain objects, so consulting the
/// type at every step walks back into it forever.  It is consulted once, on
/// the object the lookup started from.
pub fn get_prop(v: &Value, key: &Rc<str>) -> Value {
    get_prop_in(v, key, true)
}

fn get_prop_in(v: &Value, key: &Rc<str>, try_type: bool) -> Value {
    let Value::Obj(o) = v else {
        // A primitive: its methods come from its type's prototype.
        if !try_type {
            return Value::Undefined;
        }
        return match type_proto(v) {
            Some(p) => get_prop_in(&p, key, false),
            None => Value::Undefined,
        };
    };
    let o = o.clone();
    let b = o.borrow();
    if let ObjKind::Array(items) = &b.kind {
        if &**key == "length" {
            return Value::Num(items.len() as f64);
        }
        // An element is `items[i]`, not a property, so `a[0]` has to be
        // answered here.  `set_prop` knew that and this did not, which made
        // every read of an array element undefined — and then every method
        // call on one "undefined is not a function".
        if let Some(i) = array_index(key) {
            return items.get(i).cloned().unwrap_or(Value::Undefined);
        }
    }
    if let Some(v) = b.get(key) {
        return v;
    }
    let element = match b.kind {
        ObjKind::Element(id) => Some(id),
        _ => None,
    };
    let proto = b.proto.clone();
    drop(b);
    if let Some(id) = element {
        if let Some(v) = element_get(id, key) {
            return v;
        }
    }
    if let Some(p) = proto {
        return get_prop_in(&Value::Obj(p), key, false);
    }
    if !try_type {
        return Value::Undefined;
    }
    match type_proto(&Value::Obj(o)) {
        Some(p) => get_prop_in(&p, key, false),
        None => Value::Undefined,
    }
}

/// Assign a property, which may land on an array's elements or, for an
/// element, be a DOM property that has to be written into the tree.
pub fn set_prop(target: &Value, key: &Rc<str>, value: Value) -> bool {
    let Value::Obj(o) = target else {
        return false;
    };
    {
        let b = o.borrow();
        if let ObjKind::Element(id) = b.kind {
            if b.get(key).is_none() {
                drop(b);
                if element_set(id, key, value.clone()) {
                    return true;
                }
                // Not a DOM property: fall through and store it on the object,
                // because a script that puts its own field on an element
                // expects to read it back.
                o.borrow_mut().set(key.clone(), value);
                return true;
            }
        }
    }
    let mut b = o.borrow_mut();
    if let ObjKind::Array(items) = &mut b.kind {
        if &**key == "length" {
            let n = value.to_number().max(0.0) as usize;
            items.resize(n, Value::Undefined);
            return true;
        }
        if let Some(i) = array_index(key) {
            if i < items.len() {
                items[i] = value;
                return true;
            }
            // A write past the end extends the array; a huge index would
            // allocate the world, so it becomes a property instead.
            if i <= 1_000_000 {
                items.resize(i + 1, Value::Undefined);
                items[i] = value;
                return true;
            }
        }
    }
    b.set(key.clone(), value);
    true
}

/// Parse an array index, including the string form.
pub fn array_index(key: &str) -> Option<usize> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if !key.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    key.parse().ok()
}

/// A property key from a value, as the language specifies.
pub fn key_of(v: &Value) -> Rc<str> {
    match v {
        Value::Str(s) => s.clone(),
        Value::Num(n) => Rc::from(format_number(*n).as_str()),
        other => Rc::from(other.display().as_str()),
    }
}

// ---------------------------------------------------------------------------
//  Environments
// ---------------------------------------------------------------------------

pub type Env = Rc<RefCell<EnvData>>;

pub struct EnvData {
    pub vars: Vec<(Rc<str>, Value)>,
    pub parent: Option<Env>,
    /// Set when this environment is a function's, so `this` can be found.
    pub this: Value,
}

pub fn new_env(parent: Option<Env>) -> Env {
    Rc::new(RefCell::new(EnvData { vars: Vec::new(), parent, this: Value::Undefined }))
}

pub fn lookup(env: &Env, name: &str) -> Option<Value> {
    let mut e = Some(env.clone());
    while let Some(cur) = e {
        let b = cur.borrow();
        for (k, v) in b.vars.iter().rev() {
            if &**k == name {
                return Some(v.clone());
            }
        }
        e = b.parent.clone();
    }
    None
}

/// Assign to the nearest existing binding, or create one in the global scope.
pub fn assign(env: &Env, name: &Rc<str>, value: Value) {
    let mut e = Some(env.clone());
    while let Some(cur) = e {
        let b = cur.borrow_mut();
        let found = b.vars.iter().rposition(|(k, _)| k == name);
        if let Some(i) = found {
            drop(b);
            cur.borrow_mut().vars[i].1 = value;
            return;
        }
        let parent = b.parent.clone();
        drop(b);
        e = parent;
    }
    env.borrow_mut().vars.push((name.clone(), value));
}

/// Declare in this scope, which is what `var` and a function body do.
pub fn declare(env: &Env, name: &Rc<str>, value: Value) {
    let mut b = env.borrow_mut();
    for (k, v) in b.vars.iter_mut() {
        if k == name {
            *v = value;
            return;
        }
    }
    b.vars.push((name.clone(), value));
}

/// Walk to the environment a function's `this` lives in.
pub fn this_of(env: &Env) -> Value {
    let mut e = Some(env.clone());
    while let Some(cur) = e {
        let b = cur.borrow();
        if !matches!(b.this, Value::Undefined) {
            return b.this.clone();
        }
        e = b.parent.clone();
    }
    Value::Undefined
}
