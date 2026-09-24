//! The evaluator.
//!
//! A tree-walking interpreter over the syntax tree, with the control flow that
//! `break`, `continue`, `return` and `throw` need expressed as a value rather
//! than as host exceptions — the kernel has no unwinding, and a tree walk that
//! cannot say "this statement wants to leave three levels up" ends up with a
//! flag checked everywhere.
//!
//! Two guards, because a script is code the user did not write and cannot see:
//!
//!   * a step budget, so `while (true) {}` ends rather than hanging a machine
//!     that has no way to kill a process;
//!   * a heap floor, so a script that allocates without bound stops with a
//!     message instead of failing an allocation inside the allocator.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::format;

use super::ast::{Expr, FnDef, PropKey, Stmt};
use super::value::{self, key_of, Env, EnvData, Function, Obj, ObjKind, Value};

/// How many operations a script may run before it is stopped.
///
/// Generous for a page's own code and short enough that a loop which never
/// ends is noticed in about a second.
const STEP_BUDGET: u64 = 20_000_000;

/// Stop if the heap has less than this much room left.
const HEAP_FLOOR: usize = 64 * 1024;

pub enum Flow {
    Normal(Value),
    Break,
    Continue,
    Return(Value),
    Throw(Value),
}

impl Flow {
    pub fn value(self) -> Value {
        match self {
            Flow::Normal(v) | Flow::Return(v) | Flow::Throw(v) => v,
            Flow::Break | Flow::Continue => Value::Undefined,
        }
    }
}

pub struct Interp {
    pub globals: Env,
    pub steps: u64,
    /// Text a script asked for: `console.log`, `document.write`.
    pub log: String,
    /// A navigation the script asked for, from `location.href = ...`.
    pub navigate: Option<String>,
    /// How deep the call stack is, to stop a runaway recursion.
    pub depth: u32,
}

const MAX_DEPTH: u32 = 200;

impl Interp {
    pub fn new(globals: Env) -> Interp {
        Interp {
            globals,
            steps: 0,
            log: String::new(),
            navigate: None,
            depth: 0,
        }
    }

    fn step(&mut self) -> Result<(), Value> {
        self.steps += 1;
        if self.steps > STEP_BUDGET {
            return Err(Value::str("script ran too long and was stopped"));
        }
        // Checked every so often: the query is not free.  Only in the kernel,
        // where there is a heap to ask about.
        #[cfg(target_os = "none")]
        if self.steps % 4096 == 0 && crate::mem::heap::free() < HEAP_FLOOR {
            return Err(Value::str("script ran out of memory and was stopped"));
        }
        Ok(())
    }

    pub fn run(&mut self, body: &[Stmt]) -> Result<(), Value> {
        for stmt in body {
            if let Flow::Throw(v) = self.exec(stmt, &self.globals.clone())? {
                return Err(v);
            }
        }
        Ok(())
    }

    /// Put a global in place, which is how the built-ins are installed.
    pub fn define(&mut self, env: &Env, name: &str, v: Value) {
        value::declare(env, &Rc::from(name), v);
    }

    // -----------------------------------------------------------------------
    //  Statements
    // -----------------------------------------------------------------------

    fn exec(&mut self, stmt: &Stmt, env: &Env) -> Result<Flow, Value> {
        self.step()?;
        Ok(match stmt {
            Stmt::Empty => Flow::Normal(Value::Undefined),
            Stmt::Expr(e) => Flow::Normal(self.eval(e, env)?),
            Stmt::Block(body) => {
                let inner = value::new_env(Some(env.clone()));
                let mut last = Value::Undefined;
                for s in body {
                    match self.exec(s, &inner)? {
                        Flow::Normal(v) => last = v,
                        other => return Ok(other),
                    }
                }
                Flow::Normal(last)
            }
            Stmt::Var { name, init, .. } => {
                let v = match init {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Undefined,
                };
                value::declare(env, name, v);
                Flow::Normal(Value::Undefined)
            }
            Stmt::Func(def) => {
                // A declared function is visible before its definition,
                // because hoisting is what makes mutual recursion work.
                let f = self.make_function(def, env);
                value::declare(env, &def.name, f);
                Flow::Normal(Value::Undefined)
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Undefined,
                };
                Flow::Return(v)
            }
            Stmt::If { test, yes, no } => {
                if self.eval(test, env)?.truthy() {
                    self.exec(yes, env)?
                } else if let Some(no) = no {
                    self.exec(no, env)?
                } else {
                    Flow::Normal(Value::Undefined)
                }
            }
            Stmt::While { test, body } => {
                loop {
                    self.step()?;
                    if !self.eval(test, env)?.truthy() {
                        break Flow::Normal(Value::Undefined);
                    }
                    match self.exec(body, env)? {
                        Flow::Break => break Flow::Normal(Value::Undefined),
                        Flow::Continue | Flow::Normal(_) => {}
                        other => break other,
                    }
                }
            }
            Stmt::DoWhile { body, test } => {
                loop {
                    self.step()?;
                    match self.exec(body, env)? {
                        Flow::Break => break Flow::Normal(Value::Undefined),
                        Flow::Continue | Flow::Normal(_) => {}
                        other => break other,
                    }
                    if !self.eval(test, env)?.truthy() {
                        break Flow::Normal(Value::Undefined);
                    }
                }
            }
            Stmt::For { init, test, update, body } => {
                let scope = value::new_env(Some(env.clone()));
                if let Some(init) = init {
                    match self.exec(init, &scope)? {
                        Flow::Normal(_) => {}
                        other => return Ok(other),
                    }
                }
                loop {
                    self.step()?;
                    if let Some(t) = test {
                        if !self.eval(t, &scope)?.truthy() {
                            break Flow::Normal(Value::Undefined);
                        }
                    }
                    match self.exec(body, &scope)? {
                        Flow::Break => break Flow::Normal(Value::Undefined),
                        Flow::Continue | Flow::Normal(_) => {}
                        other => break other,
                    }
                    if let Some(u) = update {
                        self.eval(u, &scope)?;
                    }
                }
            }
            Stmt::ForEach { decl, target, object, body, of } => {
                let iterable = self.eval(object, env)?;
                let scope = value::new_env(Some(env.clone()));
                let items = if *of {
                    self.iterate(&iterable)?
                } else {
                    self.enumerate(&iterable)?
                };
                for item in items {
                    self.step()?;
                    match decl {
                        Some(name) => value::declare(&scope, name, item),
                        None => {
                            let key = match &item {
                                Value::Str(s) => s.clone(),
                                other => Rc::from(other.display().as_str()),
                            };
                            let _ = self.assign_to(target, Value::Str(key), &scope);
                        }
                    }
                    match self.exec(body, &scope)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal(_) => {}
                        other => return Ok(other),
                    }
                }
                Flow::Normal(Value::Undefined)
            }
            Stmt::Break => Flow::Break,
            Stmt::Continue => Flow::Continue,
            Stmt::Throw(e) => Flow::Throw(self.eval(e, env)?),
            Stmt::Try { body, param, handler, finalizer } => {
                let result = self.exec(body, env)?;
                let result = match result {
                    Flow::Throw(v) => match handler {
                        Some(h) => {
                            let inner = value::new_env(Some(env.clone()));
                            if let Some(p) = param {
                                value::declare(&inner, p, v);
                            }
                            self.exec(h, &inner)?
                        }
                        None => Flow::Throw(v),
                    },
                    other => other,
                };
                // A `finally` runs whatever happened, and its own abrupt
                // completion wins.
                if let Some(f) = finalizer {
                    match self.exec(f, env)? {
                        Flow::Normal(_) => result,
                        other => other,
                    }
                } else {
                    result
                }
            }
            Stmt::Switch { disc, cases } => {
                let d = self.eval(disc, env)?;
                let mut matched = false;
                let mut result = Flow::Normal(Value::Undefined);
                for (test, body) in cases {
                    if !matched {
                        match test {
                            Some(t) => {
                                let v = self.eval(t, env)?;
                                matched = v.strict_equals(&d);
                            }
                            None => matched = true,
                        }
                    }
                    if !matched {
                        continue;
                    }
                    for s in body {
                        match self.exec(s, env)? {
                            Flow::Normal(_) => {}
                            Flow::Break => return Ok(Flow::Normal(Value::Undefined)),
                            other => {
                                result = other;
                                break;
                            }
                        }
                    }
                    if !matches!(result, Flow::Normal(_)) {
                        break;
                    }
                }
                result
            }
        })
    }

    fn assign_to(&mut self, target: &Expr, key: Value, env: &Env) -> Result<(), Value> {
        match target {
            Expr::Ident(name) => {
                value::assign(env, name, key);
                Ok(())
            }
            Expr::Member { object, key: k, .. } => {
                let obj = self.eval(object, env)?;
                let prop = key_of(&self.eval(k, env)?);
                value::set_prop(&obj, &prop, key);
                Ok(())
            }
            _ => Err(Value::str("not a valid assignment target")),
        }
    }

    /// The values `for...of` walks: arrays element by element, strings by
    /// character, and anything else by its length property.
    fn iterate(&mut self, v: &Value) -> Result<Vec<Value>, Value> {
        match v {
            Value::Str(s) => Ok(s.chars().map(|c| Value::string(c.to_string())).collect()),
            Value::Obj(o) => {
                let b = o.borrow();
                match &b.kind {
                    ObjKind::Array(items) => Ok(items.clone()),
                    _ => {
                        drop(b);
                        let len = value::get_prop(v, &Rc::from("length")).to_number();
                        let mut out = Vec::new();
                        let mut i = 0.0;
                        while i < len && out.len() < 100_000 {
                            out.push(value::get_prop(v, &Rc::from(format!("{}", i as i64).as_str())));
                            i += 1.0;
                        }
                        Ok(out)
                    }
                }
            }
            _ => Ok(Vec::new()),
        }
    }

    fn enumerate(&mut self, v: &Value) -> Result<Vec<Value>, Value> {
        let Value::Obj(o) = v else {
            return Ok(Vec::new());
        };
        let b = o.borrow();
        let mut out = Vec::new();
        if let ObjKind::Array(items) = &b.kind {
            for i in 0..items.len() {
                out.push(Value::string(format!("{}", i)));
            }
        }
        for (k, _) in b.props.iter() {
            out.push(Value::Str(k.clone()));
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------
    //  Expressions
    // -----------------------------------------------------------------------

    pub fn eval(&mut self, e: &Expr, env: &Env) -> Result<Value, Value> {
        self.step()?;
        Ok(match e {
            Expr::Num(n) => Value::Num(*n),
            Expr::Str(s) => Value::Str(s.clone()),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Null => Value::Null,
            Expr::Undefined => Value::Undefined,
            Expr::This => value::this_of(env),
            Expr::Ident(name) => match value::lookup(env, name) {
                Some(v) => v,
                None => {
                    // An undeclared name is a reference error, except that
                    // reading one is a common way to test for a global.
                    return Err(Value::string(format!("{} is not defined", name)));
                }
            },
            Expr::Function(def) => self.make_function(def, env),
            Expr::Named(_) => Value::Undefined,
            Expr::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for i in items {
                    out.push(self.eval(i, env)?);
                }
                Value::array(out)
            }
            Expr::Object(props) => {
                let mut o = Obj::plain();
                for (k, v) in props {
                    let value = self.eval(v, env)?;
                    match k {
                        PropKey::Named(n) if &**n == "\u{0}spread" => {
                            // `{...other}` copies the other object's own
                            // properties.
                            if let Value::Obj(src) = &value {
                                let b = src.borrow();
                                for (k, v) in b.props.iter() {
                                    o.set(k.clone(), v.clone());
                                }
                            }
                        }
                        PropKey::Named(n) => o.set(n.clone(), value),
                        PropKey::Computed(k) => {
                            let key = key_of(&self.eval(k, env)?);
                            o.set(key, value);
                        }
                    }
                }
                Value::object(o)
            }
            Expr::Member { object, key, computed } => {
                let obj = self.eval(object, env)?;
                let k = if *computed {
                    key_of(&self.eval(key, env)?)
                } else {
                    match &**key {
                        Expr::Str(s) => s.clone(),
                        _ => key_of(&self.eval(key, env)?),
                    }
                };
                if matches!(obj, Value::Undefined) {
                    return Err(Value::str("cannot read a property of undefined"));
                }
                value::get_prop(&obj, &k)
            }
            Expr::Call { callee, args } => {
                // The name of what was called, kept for the error message: a
                // script that fails with "undefined is not a function" and no
                // more is a script nobody can debug, including its author.
                let mut called: Option<String> = None;
                let (this, func) = match &**callee {
                    Expr::Member { object, key, computed } => {
                        let obj = self.eval(object, env)?;
                        let k = if *computed {
                            key_of(&self.eval(key, env)?)
                        } else {
                            match &**key {
                                Expr::Str(s) => s.clone(),
                                _ => key_of(&self.eval(key, env)?),
                            }
                        };
                        called = Some(k.to_string());
                        (obj.clone(), value::get_prop(&obj, &k))
                    }
                    Expr::Ident(name) => {
                        called = Some(name.to_string());
                        (Value::Undefined, self.eval(callee, env)?)
                    }
                    other => (Value::Undefined, self.eval(other, env)?),
                };
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.eval(a, env)?);
                }
                self.call(&func, this, &argv).map_err(|e| {
                    match (e, called) {
                        (Value::Str(s), Some(name)) if s.starts_with("TypeError") => {
                            Value::string(format!("{} (`{}`)", s, name))
                        }
                        (other, _) => other,
                    }
                })?
            }
            Expr::New { callee, args } => {
                let func = self.eval(callee, env)?;
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.eval(a, env)?);
                }
                let proto = value::get_prop(&func, &Rc::from("prototype"));
                let mut o = Obj::plain();
                if let Value::Obj(p) = proto {
                    o.proto = Some(p);
                }
                let this = Value::object(o);
                let result = self.call(&func, this.clone(), &argv)?;
                // A constructor that returns an object replaces `this`.
                match result {
                    Value::Obj(_) => result,
                    _ => this,
                }
            }
            Expr::Unary { op, expr } => {
                match *op {
                    "typeof" => {
                        // `typeof undeclared` is "undefined", not an error.
                        if let Expr::Ident(name) = &**expr {
                            if value::lookup(env, name).is_none() {
                                return Ok(Value::str("undefined"));
                            }
                        }
                        let v = self.eval(expr, env)?;
                        return Ok(Value::str(v.type_of()));
                    }
                    "delete" => {
                        if let Expr::Member { object, key, computed } = &**expr {
                            let obj = self.eval(object, env)?;
                            let k = if *computed {
                                key_of(&self.eval(key, env)?)
                            } else {
                                match &**key {
                                    Expr::Str(s) => s.clone(),
                                    _ => key_of(&self.eval(key, env)?),
                                }
                            };
                            if let Value::Obj(o) = &obj {
                                let mut b = o.borrow_mut();
                                b.props.retain(|(k2, _)| *k2 != k);
                            }
                        }
                        return Ok(Value::Bool(true));
                    }
                    _ => {}
                }
                let v = self.eval(expr, env)?;
                match *op {
                    "!" => Value::Bool(!v.truthy()),
                    "-" => Value::Num(-v.to_number()),
                    "+" => Value::Num(v.to_number()),
                    "~" => Value::Num(!(v.to_number() as i64) as f64),
                    "void" => Value::Undefined,
                    _ => Value::Undefined,
                }
            }
            Expr::Update { op, target, prefix } => {
                let old = self.eval(target, env)?.to_number();
                let new = if *op == "++" { old + 1.0 } else { old - 1.0 };
                self.store(target, Value::Num(new), env)?;
                if *prefix {
                    Value::Num(new)
                } else {
                    Value::Num(old)
                }
            }
            Expr::Logical { op, left, right } => {
                let l = self.eval(left, env)?;
                match *op {
                    "&&" => {
                        if l.truthy() {
                            self.eval(right, env)?
                        } else {
                            l
                        }
                    }
                    "||" => {
                        if l.truthy() {
                            l
                        } else {
                            self.eval(right, env)?
                        }
                    }
                    _ => {
                        // `??` on null or undefined only.
                        if matches!(l, Value::Null | Value::Undefined) {
                            self.eval(right, env)?
                        } else {
                            l
                        }
                    }
                }
            }
            Expr::Binary { op, left, right } => {
                let l = self.eval(left, env)?;
                let r = self.eval(right, env)?;
                self.binary(op, l, r)?
            }
            Expr::Assign { op, target, value } => {
                let v = if *op == "=" {
                    self.eval(value, env)?
                } else {
                    // Both sides are evaluated exactly once.  An earlier
                    // version called back into `eval` per operator to decide
                    // string versus number, which evaluated the right-hand
                    // side three extra times and ran its side effects with it.
                    let cur = self.eval(target, env)?;
                    let rhs = self.eval(value, env)?;
                    match *op {
                        "+=" => {
                            // `+=` concatenates when either side is a string,
                            // which is why it cannot go through to_number.
                            if matches!(cur, Value::Str(_)) || matches!(rhs, Value::Str(_)) {
                                Value::string(format!(
                                    "{}{}",
                                    cur.to_js_string(),
                                    rhs.to_js_string()
                                ))
                            } else {
                                Value::Num(cur.to_number() + rhs.to_number())
                            }
                        }
                        "-=" => Value::Num(cur.to_number() - rhs.to_number()),
                        "*=" => Value::Num(cur.to_number() * rhs.to_number()),
                        "/=" => Value::Num(cur.to_number() / rhs.to_number()),
                        "%=" => Value::Num(cur.to_number() % rhs.to_number()),
                        "**=" => Value::Num(pow(cur.to_number(), rhs.to_number())),
                        "&=" => Value::Num((to_int32(&cur) & to_int32(&rhs)) as f64),
                        "|=" => Value::Num((to_int32(&cur) | to_int32(&rhs)) as f64),
                        "^=" => Value::Num((to_int32(&cur) ^ to_int32(&rhs)) as f64),
                        "<<=" => Value::Num((to_int32(&cur) << (to_int32(&rhs) & 31)) as f64),
                        ">>=" => Value::Num((to_int32(&cur) >> (to_int32(&rhs) & 31)) as f64),
                        "&&=" => {
                            if cur.truthy() {
                                rhs
                            } else {
                                cur
                            }
                        }
                        "||=" => {
                            if cur.truthy() {
                                cur
                            } else {
                                rhs
                            }
                        }
                        "??=" => {
                            if matches!(cur, Value::Null | Value::Undefined) {
                                rhs
                            } else {
                                cur
                            }
                        }
                        _ => Value::Undefined,
                    }
                };
                self.store(target, v.clone(), env)?;
                v
            }
            Expr::Cond { test, yes, no } => {
                if self.eval(test, env)?.truthy() {
                    self.eval(yes, env)?
                } else {
                    self.eval(no, env)?
                }
            }
            Expr::Sequence(list) => {
                let mut last = Value::Undefined;
                for e in list {
                    last = self.eval(e, env)?;
                }
                last
            }
        })
    }

    fn store(&mut self, target: &Expr, v: Value, env: &Env) -> Result<(), Value> {
        match target {
            Expr::Ident(name) => {
                value::assign(env, name, v);
                Ok(())
            }
            Expr::Member { object, key, computed } => {
                let obj = self.eval(object, env)?;
                let k = if *computed {
                    key_of(&self.eval(key, env)?)
                } else {
                    match &**key {
                        Expr::Str(s) => s.clone(),
                        _ => key_of(&self.eval(key, env)?),
                    }
                };
                value::set_prop(&obj, &k, v);
                Ok(())
            }
            _ => Err(Value::str("not a valid assignment target")),
        }
    }

    fn binary(&mut self, op: &str, l: Value, r: Value) -> Result<Value, Value> {
        Ok(match op {
            "+" => {
                // Addition is concatenation when either side is a string.
                if matches!(l, Value::Str(_)) || matches!(r, Value::Str(_)) {
                    Value::string(format!("{}{}", l.to_js_string(), r.to_js_string()))
                } else {
                    Value::Num(l.to_number() + r.to_number())
                }
            }
            "-" => Value::Num(l.to_number() - r.to_number()),
            "*" => Value::Num(l.to_number() * r.to_number()),
            "/" => Value::Num(l.to_number() / r.to_number()),
            "%" => Value::Num(l.to_number() % r.to_number()),
            "**" => Value::Num(pow(l.to_number(), r.to_number())),
            "==" => Value::Bool(l.loose_equals(&r)),
            "!=" => Value::Bool(!l.loose_equals(&r)),
            "===" => Value::Bool(l.strict_equals(&r)),
            "!==" => Value::Bool(!l.strict_equals(&r)),
            "<" | ">" | "<=" | ">=" => {
                // Two strings compare as strings; anything else numerically.
                let result = match (&l, &r) {
                    (Value::Str(a), Value::Str(b)) => match op {
                        "<" => a < b,
                        ">" => a > b,
                        "<=" => a <= b,
                        _ => a >= b,
                    },
                    _ => {
                        let (a, b) = (l.to_number(), r.to_number());
                        if a.is_nan() || b.is_nan() {
                            false
                        } else {
                            match op {
                                "<" => a < b,
                                ">" => a > b,
                                "<=" => a <= b,
                                _ => a >= b,
                            }
                        }
                    }
                };
                Value::Bool(result)
            }
            "&" => Value::Num(((l.to_number() as i64) & (r.to_number() as i64)) as f64),
            "|" => Value::Num(((l.to_number() as i64) | (r.to_number() as i64)) as f64),
            "^" => Value::Num(((l.to_number() as i64) ^ (r.to_number() as i64)) as f64),
            "<<" => Value::Num(((l.to_number() as i64) << ((r.to_number() as i64) & 31)) as f64),
            ">>" => Value::Num(((l.to_number() as i64) >> ((r.to_number() as i64) & 31)) as f64),
            ">>>" => Value::Num(
                (((l.to_number() as i64) as u64) >> ((r.to_number() as i64) & 63)) as f64,
            ),
            "in" => {
                let key = key_of(&l);
                match &r {
                    Value::Obj(o) => {
                        let b = o.borrow();
                        let own = b.props.iter().any(|(k, _)| *k == key);
                        drop(b);
                        Value::Bool(own || !matches!(value::get_prop(&r, &key), Value::Undefined))
                    }
                    _ => Value::Bool(false),
                }
            }
            "instanceof" => {
                let proto = value::get_prop(&r, &Rc::from("prototype"));
                let mut cur = match &l {
                    Value::Obj(o) => Some(o.clone()),
                    _ => None,
                };
                let mut found = false;
                while let Some(o) = cur {
                    let next = o.borrow().proto.clone();
                    if let (Value::Obj(p), Some(q)) = (&proto, &next) {
                        if Rc::ptr_eq(p, q) {
                            found = true;
                            break;
                        }
                    }
                    cur = next;
                }
                Value::Bool(found)
            }
            _ => Value::Undefined,
        })
    }

    // -----------------------------------------------------------------------
    //  Functions
    // -----------------------------------------------------------------------

    pub fn make_function(&self, def: &Rc<FnDef>, env: &Env) -> Value {
        let this = value::this_of(env);
        let mut o = Obj::plain();
        o.kind = ObjKind::Function(Function {
            def: def.clone(),
            env: env.clone(),
            this,
        });
        o.props.push((Rc::from("name"), Value::Str(def.name.clone())));
        let f = Value::object(o);
        // Every function gets a prototype object, which is what `new` uses
        // and what `instanceof` walks.
        let mut proto = Obj::plain();
        proto.set(Rc::from("constructor"), f.clone());
        value::set_prop(&f, &Rc::from("prototype"), Value::object(proto));
        f
    }

    pub fn call(&mut self, func: &Value, this: Value, args: &[Value]) -> Result<Value, Value> {
        let Value::Obj(o) = func else {
            return Err(Value::string(format!("TypeError: {} is not a function", func.display())));
        };

        // A native function is a plain function pointer, which can be taken
        // while the object is borrowed and called after it is not — the native
        // may itself touch objects, and a borrow held across that would panic.
        {
            let b = o.borrow();
            if let ObjKind::Native(f) = b.kind {
                drop(b);
                return f(self, this, args);
            }
        }

        let (def, closure) = {
            let b = o.borrow();
            match &b.kind {
                ObjKind::Function(f) => (f.def.clone(), f.env.clone()),
                _ => {
                    return Err(Value::string(format!(
                        "TypeError: {} is not a function",
                        func.display()
                    )))
                }
            }
        };

        if self.depth >= MAX_DEPTH {
            return Err(Value::str("too much recursion"));
        }
        self.depth += 1;
        let result = self.call_user(&def, &closure, this, args);
        self.depth -= 1;
        result
    }

    fn call_user(
        &mut self,
        def: &Rc<FnDef>,
        closure: &Env,
        this: Value,
        args: &[Value],
    ) -> Result<Value, Value> {
        let scope = value::new_env(Some(closure.clone()));
        scope.borrow_mut().this = this;
        for (i, p) in def.params.iter().enumerate() {
            let v = args.get(i).cloned().unwrap_or(Value::Undefined);
            value::declare(&scope, p, v);
        }
        if let Some(rest) = &def.rest {
            let extra: Vec<Value> = args.iter().skip(def.params.len()).cloned().collect();
            value::declare(&scope, rest, Value::array(extra));
        }
        if let Some(body) = &def.expr_body {
            return self.eval(body, &scope);
        }
        for s in &def.body {
            match self.exec(s, &scope)? {
                Flow::Return(v) => return Ok(v),
                Flow::Throw(v) => return Err(v),
                _ => {}
            }
        }
        Ok(Value::Undefined)
    }
}

/// A value as a 32-bit integer, which is what the bitwise operators use.
fn to_int32(v: &Value) -> i64 {
    let n = v.to_number();
    if !n.is_finite() {
        return 0;
    }
    let t = if n < 0.0 { -n.floor() } else { n.floor() };
    let m = t % 4294967296.0;
    let m = if m < 0.0 { m + 4294967296.0 } else { m };
    (if m >= 2147483648.0 { m - 4294967296.0 } else { m }) as i64
}

/// `**`, which `core` does not have for floats.
fn pow(base: f64, exp: f64) -> f64 {
    if exp == 0.0 {
        return 1.0;
    }
    if base == 0.0 {
        return 0.0;
    }
    if exp.fract() == 0.0 && exp.abs() < 1024.0 {
        let mut n = exp.abs() as u32;
        let mut result = 1.0f64;
        let mut b = base;
        while n > 0 {
            if n & 1 == 1 {
                result *= b;
            }
            b *= b;
            n >>= 1;
        }
        return if exp < 0.0 { 1.0 / result } else { result };
    }
    // A general case by exp(ln(x)*y), with both from their series.  Exact
    // enough for page scripts and no libm.
    if base < 0.0 {
        return f64::NAN;
    }
    exp_of(ln_of(base) * exp)
}

/// The natural logarithm, by argument reduction and a series.
pub fn ln_of(x: f64) -> f64 {
    if x <= 0.0 {
        return if x == 0.0 { f64::NEG_INFINITY } else { f64::NAN };
    }
    // x = m * 2^k with m in [1, 2), so ln x = ln m + k ln 2.
    let mut k = 0i32;
    let mut m = x;
    while m >= 2.0 {
        m /= 2.0;
        k += 1;
    }
    while m < 1.0 {
        m *= 2.0;
        k -= 1;
    }
    // ln m for m in [1,2) via atanh: ln m = 2 * atanh((m-1)/(m+1)).
    let t = (m - 1.0) / (m + 1.0);
    let t2 = t * t;
    let mut term = t;
    let mut sum = 0.0;
    let mut n = 1.0;
    for _ in 0..40 {
        sum += term / n;
        term *= t2;
        n += 2.0;
    }
    let ln2 = 0.6931471805599453;
    2.0 * sum + (k as f64) * ln2
}

/// The exponential, by range reduction and a series.
pub fn exp_of(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x > 709.0 {
        return f64::INFINITY;
    }
    if x < -709.0 {
        return 0.0;
    }
    // x = k ln2 + r, with r small; e^x = 2^k e^r.
    let ln2 = 0.6931471805599453;
    let k = (x / ln2).round();
    let r = x - k * ln2;
    let mut term = 1.0;
    let mut sum = 1.0;
    for n in 1..24 {
        term *= r / n as f64;
        sum += term;
    }
    sum * pow2(k as i32)
}

fn pow2(k: i32) -> f64 {
    let mut v = 1.0f64;
    let mut n = k.abs();
    let base = if k < 0 { 0.5 } else { 2.0 };
    while n > 0 {
        v *= base;
        n -= 1;
    }
    v
}

/// The square root, by Newton's method — `core` has no `sqrt`.
pub fn sqrt_of(x: f64) -> f64 {
    if x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let mut g = x;
    for _ in 0..60 {
        let next = 0.5 * (g + x / g);
        if (next - g).abs() < 1e-15 * g {
            return next;
        }
        g = next;
    }
    g
}

// Small helpers used by compound assignment, which needs the right-hand side
// evaluated once but converted depending on the operator.
fn v_is_str(e: &Expr, i: &mut Interp, env: &Env) -> Result<bool, Value> {
    Ok(matches!(i.eval(e, env)?, Value::Str(_)))
}
fn v_is_get(e: &Expr, i: &mut Interp, env: &Env) -> Result<String, Value> {
    Ok(i.eval(e, env)?.to_js_string())
}
fn v_is_num(e: &Expr, i: &mut Interp, env: &Env) -> f64 {
    i.eval(e, env).map(|v| v.to_number()).unwrap_or(f64::NAN)
}

/// An environment with the globals in it, ready for a program.
pub fn global_env() -> Env {
    Rc::new(core::cell::RefCell::new(EnvData {
        vars: Vec::new(),
        parent: None,
        this: Value::Undefined,
    }))
}
