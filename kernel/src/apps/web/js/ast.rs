//! JavaScript's syntax tree.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug)]
pub struct FnDef {
    pub name: Rc<str>,
    pub params: Vec<Rc<str>>,
    /// Rest parameter, `...args`, if there is one.
    pub rest: Option<Rc<str>>,
    pub body: Vec<Stmt>,
    /// An arrow function whose body is one expression rather than a block.
    pub expr_body: Option<Expr>,
}

#[derive(Debug)]
pub enum PropKey {
    Named(Rc<str>),
    Computed(Expr),
}

#[derive(Debug)]
pub enum Expr {
    Num(f64),
    Str(Rc<str>),
    Bool(bool),
    Null,
    Undefined,
    Ident(Rc<str>),
    This,
    Array(Vec<Expr>),
    Object(Vec<(PropKey, Expr)>),
    /// A function expression, or a declared function reached through its name.
    Function(Rc<FnDef>),
    /// A function expression reached through its name.
    Named(Rc<str>),
    Call { callee: Box<Expr>, args: Vec<Expr> },
    New { callee: Box<Expr>, args: Vec<Expr> },
    Member { object: Box<Expr>, key: Box<Expr>, computed: bool },
    Unary { op: &'static str, expr: Box<Expr> },
    Update { op: &'static str, target: Box<Expr>, prefix: bool },
    Binary { op: &'static str, left: Box<Expr>, right: Box<Expr> },
    /// `&&`, `||` and `??`, which do not evaluate their right side always.
    Logical { op: &'static str, left: Box<Expr>, right: Box<Expr> },
    Assign { op: &'static str, target: Box<Expr>, value: Box<Expr> },
    Cond { test: Box<Expr>, yes: Box<Expr>, no: Box<Expr> },
    Sequence(Vec<Expr>),
}

#[derive(Debug)]
pub enum Stmt {
    Expr(Expr),
    Var { kind: &'static str, name: Rc<str>, init: Option<Expr> },
    Func(Rc<FnDef>),
    Return(Option<Expr>),
    If { test: Expr, yes: Box<Stmt>, no: Option<Box<Stmt>> },
    While { test: Expr, body: Box<Stmt> },
    DoWhile { body: Box<Stmt>, test: Expr },
    For {
        init: Option<Box<Stmt>>,
        test: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    /// `for (x in o)` and `for (x of o)`.
    ForEach {
        decl: Option<Rc<str>>,
        target: Expr,
        object: Expr,
        body: Box<Stmt>,
        of: bool,
    },
    Block(Vec<Stmt>),
    Break,
    Continue,
    Throw(Expr),
    Try {
        body: Box<Stmt>,
        param: Option<Rc<str>>,
        handler: Option<Box<Stmt>>,
        finalizer: Option<Box<Stmt>>,
    },
    Switch { disc: Expr, cases: Vec<(Option<Expr>, Vec<Stmt>)> },
    Empty,
}

impl Expr {
    pub fn num(v: f64) -> Expr {
        Expr::Num(v)
    }

    pub fn str(s: &str) -> Expr {
        Expr::Str(Rc::from(s))
    }
}

/// A program, kept together so functions can borrow their bodies.
pub struct Program {
    pub body: Vec<Stmt>,
    /// Every string the parser made, so nothing has to be re-created.
    pub source: String,
}
