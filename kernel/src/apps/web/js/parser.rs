//! A JavaScript parser: tokens to a syntax tree.
//!
//! Recursive descent with precedence climbing, which is what the grammar is.
//! The parts that are easy to get subtly wrong, and so are called out where
//! they happen:
//!
//!   * automatic semicolon insertion — a statement may end because the next
//!     token is on another line, and `return` followed by a newline returns
//!     nothing rather than the expression on the next line;
//!   * arrow functions, which are only recognisable after the parameter list
//!     has been read, so `(a, b)` may turn out to be either;
//!   * `in` is not an operator inside the head of a `for`, because
//!     `for (x in y)` is a statement about properties rather than a
//!     comparison.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::vec;

use super::ast::{Expr, FnDef, PropKey, Stmt};
use super::num;
use super::lexer::{lex, Tok, Token};

pub struct Parser {
    toks: Vec<Token>,
    at: usize,
    /// Set while parsing a `for` head, where `in` is not an operator.
    no_in: u32,
}

/// The left binding power of a binary operator, or 0 if it is not one.
fn precedence(op: &str) -> u8 {
    match op {
        "??" => 2,
        "||" => 3,
        "&&" => 4,
        "|" => 5,
        "^" => 6,
        "&" => 7,
        "==" | "!=" | "===" | "!==" => 8,
        "<" | ">" | "<=" | ">=" | "in" | "instanceof" => 9,
        "<<" | ">>" | ">>>" => 10,
        "+" | "-" => 11,
        "*" | "/" | "%" => 12,
        "**" => 13,
        _ => 0,
    }
}

pub fn parse(src: &str) -> Result<Vec<Stmt>, (String, u32)> {
    let toks = lex(src)?;
    let mut p = Parser { toks, at: 0, no_in: 0 };
    p.program()
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.at.min(self.toks.len() - 1)].tok
    }

    fn line(&self) -> u32 {
        self.toks[self.at.min(self.toks.len() - 1)].line
    }

    fn next(&mut self) -> Tok {
        let t = self.toks[self.at.min(self.toks.len() - 1)].tok.clone();
        if self.at < self.toks.len() - 1 {
            self.at += 1;
        }
        t
    }

    /// Skip the newline markers the lexer emits.
    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Tok::Newline) {
            self.at += 1;
        }
    }

    /// Was a newline crossed before the current token?
    fn at_newline(&self) -> bool {
        matches!(self.peek(), Tok::Newline)
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(x) if *x == p)
    }

    fn is_keyword(&self, k: &str) -> bool {
        matches!(self.peek(), Tok::Keyword(x) if *x == k)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn eat_keyword(&mut self, k: &str) -> bool {
        if self.is_keyword(k) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, p: &str) -> Result<(), (String, u32)> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            Err((alloc::format!("expected `{}`, found {:?}", p, self.peek()), self.line()))
        }
    }

    fn expect_name(&mut self) -> Result<Rc<str>, (String, u32)> {
        match self.next() {
            Tok::Name(n) => Ok(n),
            // A keyword is a legal property name and, in sloppy mode, a legal
            // variable name; treating it as one keeps `obj.class` and
            // `var name` working.
            Tok::Keyword(k) => Ok(Rc::from(k)),
            other => Err((alloc::format!("expected a name, found {:?}", other), self.line())),
        }
    }

    /// End a statement: a semicolon, a closing brace, or a newline.
    fn end_statement(&mut self) {
        self.skip_newlines();
        if self.eat_punct(";") {
            return;
        }
    }

    fn program(&mut self) -> Result<Vec<Stmt>, (String, u32)> {
        let mut out = Vec::new();
        self.skip_newlines();
        while !matches!(self.peek(), Tok::Eof) {
            out.push(self.statement()?);
            self.skip_newlines();
        }
        Ok(out)
    }

    fn block(&mut self) -> Result<Vec<Stmt>, (String, u32)> {
        self.expect_punct("{")?;
        let mut out = Vec::new();
        self.skip_newlines();
        while !self.is_punct("}") && !matches!(self.peek(), Tok::Eof) {
            out.push(self.statement()?);
            self.skip_newlines();
        }
        self.expect_punct("}")?;
        Ok(out)
    }

    /// One statement.  Returns a block for anything that expands to several.
    fn statement(&mut self) -> Result<Stmt, (String, u32)> {
        self.skip_newlines();
        if self.is_punct("{") {
            return Ok(Stmt::Block(self.block()?));
        }
        if self.is_punct(";") {
            self.at += 1;
            return Ok(Stmt::Empty);
        }

        if let Tok::Keyword(k) = self.peek().clone() {
            match k {
                "var" | "let" | "const" => {
                    self.at += 1;
                    let kind: &'static str = match k {
                        "var" => "var",
                        "let" => "let",
                        _ => "const",
                    };
                    let stmt = self.var_decl(kind)?;
                    self.end_statement();
                    return Ok(stmt);
                }
                "function" => {
                    self.at += 1;
                    let f = self.function_rest(true)?;
                    return Ok(Stmt::Func(f));
                }
                "return" => {
                    self.at += 1;
                    // `return` on its own line returns nothing: the expression
                    // belongs to the next statement.
                    if self.at_newline() || self.is_punct(";") || self.is_punct("}") {
                        self.end_statement();
                        return Ok(Stmt::Return(None));
                    }
                    let e = self.expression()?;
                    self.end_statement();
                    return Ok(Stmt::Return(Some(e)));
                }
                "if" => {
                    self.at += 1;
                    self.expect_punct("(")?;
                    let test = self.expression()?;
                    self.expect_punct(")")?;
                    let yes = Box::new(self.statement()?);
                    self.skip_newlines();
                    let no = if self.eat_keyword("else") {
                        Some(Box::new(self.statement()?))
                    } else {
                        None
                    };
                    return Ok(Stmt::If { test, yes, no });
                }
                "while" => {
                    self.at += 1;
                    self.expect_punct("(")?;
                    let test = self.expression()?;
                    self.expect_punct(")")?;
                    let body = Box::new(self.statement()?);
                    return Ok(Stmt::While { test, body });
                }
                "do" => {
                    self.at += 1;
                    let body = Box::new(self.statement()?);
                    self.skip_newlines();
                    if !self.eat_keyword("while") {
                        return Err(("expected `while` after `do`".to_string(), self.line()));
                    }
                    self.expect_punct("(")?;
                    let test = self.expression()?;
                    self.expect_punct(")")?;
                    self.end_statement();
                    return Ok(Stmt::DoWhile { body, test });
                }
                "for" => {
                    self.at += 1;
                    return self.for_statement();
                }
                "break" => {
                    self.at += 1;
                    self.end_statement();
                    return Ok(Stmt::Break);
                }
                "continue" => {
                    self.at += 1;
                    self.end_statement();
                    return Ok(Stmt::Continue);
                }
                "throw" => {
                    self.at += 1;
                    let e = self.expression()?;
                    self.end_statement();
                    return Ok(Stmt::Throw(e));
                }
                "try" => {
                    self.at += 1;
                    let body = Box::new(Stmt::Block(self.block()?));
                    self.skip_newlines();
                    let mut param = None;
                    let mut handler = None;
                    let mut finalizer = None;
                    if self.eat_keyword("catch") {
                        if self.eat_punct("(") {
                            param = Some(self.expect_name()?);
                            self.expect_punct(")")?;
                        }
                        handler = Some(Box::new(Stmt::Block(self.block()?)));
                        self.skip_newlines();
                    }
                    if self.eat_keyword("finally") {
                        finalizer = Some(Box::new(Stmt::Block(self.block()?)));
                    }
                    return Ok(Stmt::Try { body, param, handler, finalizer });
                }
                "switch" => {
                    self.at += 1;
                    self.expect_punct("(")?;
                    let disc = self.expression()?;
                    self.expect_punct(")")?;
                    self.expect_punct("{")?;
                    let mut cases = Vec::new();
                    self.skip_newlines();
                    while !self.is_punct("}") && !matches!(self.peek(), Tok::Eof) {
                        let test = if self.eat_keyword("case") {
                            let e = self.expression()?;
                            Some(e)
                        } else if self.eat_keyword("default") {
                            None
                        } else {
                            return Err(("expected `case` or `default`".to_string(), self.line()));
                        };
                        self.expect_punct(":")?;
                        let mut body = Vec::new();
                        self.skip_newlines();
                        while !self.is_punct("}") && !self.is_keyword("case")
                            && !self.is_keyword("default") && !matches!(self.peek(), Tok::Eof)
                        {
                            body.push(self.statement()?);
                            self.skip_newlines();
                        }
                        cases.push((test, body));
                    }
                    self.expect_punct("}")?;
                    return Ok(Stmt::Switch { disc, cases });
                }
                "class" => {
                    // A class is syntax this interpreter does not have.  Failing
                    // the whole script is the right answer: a page that defines
                    // a class and then uses it is not a page that half works.
                    return Err((
                        "classes are not supported".to_string(),
                        self.line(),
                    ));
                }
                "import" | "export" => {
                    return Err(("modules are not supported".to_string(), self.line()));
                }
                _ => {}
            }
        }

        let e = self.expression()?;
        self.end_statement();
        Ok(Stmt::Expr(e))
    }

    fn var_decl(&mut self, kind: &'static str) -> Result<Stmt, (String, u32)> {
        let name = self.expect_name()?;
        let init = if self.eat_punct("=") {
            Some(self.assignment()?)
        } else {
            None
        };
        // `var a, b` becomes a block of one declaration each, which is what it
        // means and keeps the tree flat.
        if self.eat_punct(",") {
            let rest = self.var_decl(kind)?;
            return Ok(Stmt::Block(vec![
                Stmt::Var { kind, name, init },
                rest,
            ]));
        }
        Ok(Stmt::Var { kind, name, init })
    }

    fn for_statement(&mut self) -> Result<Stmt, (String, u32)> {
        self.expect_punct("(")?;
        self.no_in += 1;

        // The head may be a declaration, which `for (x in y)` needs.
        let mut decl: Option<Rc<str>> = None;
        let mut init_stmt: Option<Box<Stmt>> = None;
        if self.is_keyword("var") || self.is_keyword("let") || self.is_keyword("const") {
            let kind = match self.next() {
                Tok::Keyword("var") => "var",
                Tok::Keyword("let") => "let",
                _ => "const",
            };
            let name = self.expect_name()?;
            decl = Some(name.clone());
            let value = if self.eat_punct("=") {
                Some(self.assignment()?)
            } else {
                None
            };
            init_stmt = Some(Box::new(Stmt::Var { kind, name, init: value }));
        } else if !self.is_punct(";") {
            let e = self.expression()?;
            init_stmt = Some(Box::new(Stmt::Expr(e)));
        }

        // `for (x in y)` / `for (x of y)`.
        let for_of = self.is_keyword("of");
        if for_of || (self.is_keyword("in") && self.no_in == 1) {
            let of = for_of;
            self.at += 1;
            self.no_in -= 1;
            let object = self.expression()?;
            self.expect_punct(")")?;
            let body = Box::new(self.statement()?);
            let target = match &init_stmt {
                Some(s) => match &**s {
                    Stmt::Var { name, .. } => Expr::Ident(name.clone()),
                    Stmt::Expr(e) => match e {
                        Expr::Ident(n) => Expr::Ident(n.clone()),
                        _ => return Err(("not a valid loop target".to_string(), self.line())),
                    },
                    _ => return Err(("not a valid loop target".to_string(), self.line())),
                },
                None => return Err(("missing loop target".to_string(), self.line())),
            };
            return Ok(Stmt::ForEach { decl, target, object, body, of });
        }

        self.no_in -= 1;
        self.expect_punct(";")?;
        let test = if self.is_punct(";") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect_punct(";")?;
        let update = if self.is_punct(")") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect_punct(")")?;
        let body = Box::new(self.statement()?);
        Ok(Stmt::For { init: init_stmt, test, update, body })
    }

    fn function_rest(&mut self, named: bool) -> Result<Rc<FnDef>, (String, u32)> {
        let name: Rc<str> = if named {
            self.expect_name()?
        } else if let Tok::Name(n) = self.peek().clone() {
            self.at += 1;
            n
        } else {
            Rc::from("")
        };
        let (params, rest) = self.parameters()?;
        let body = self.block()?;
        Ok(Rc::new(FnDef { name, params, rest, body, expr_body: None }))
    }

    fn parameters(&mut self) -> Result<(Vec<Rc<str>>, Option<Rc<str>>), (String, u32)> {
        self.expect_punct("(")?;
        let mut params = Vec::new();
        let mut rest = None;
        self.skip_newlines();
        while !self.is_punct(")") {
            if self.eat_punct("...") {
                rest = Some(self.expect_name()?);
                break;
            }
            params.push(self.expect_name()?);
            self.skip_newlines();
            if !self.eat_punct(",") {
                break;
            }
            self.skip_newlines();
        }
        self.expect_punct(")")?;
        Ok((params, rest))
    }

    fn expression(&mut self) -> Result<Expr, (String, u32)> {
        let first = self.assignment()?;
        if self.is_punct(",") {
            let mut out = vec![first];
            while self.eat_punct(",") {
                out.push(self.assignment()?);
            }
            return Ok(Expr::Sequence(out));
        }
        Ok(first)
    }

    fn assignment(&mut self) -> Result<Expr, (String, u32)> {
        // An arrow function is only known to be one after its parameters have
        // been read, so a `(` here has to be looked past.
        if let Some(arrow) = self.try_arrow()? {
            return Ok(arrow);
        }
        if self.is_keyword("function") {
            self.at += 1;
            let f = self.function_rest(false)?;
            return Ok(Expr::Function(f));
        }

        let left = self.conditional()?;
        if let Tok::Punct(op) = self.peek().clone() {
            let assign_op = match op {
                "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^="
                | "<<=" | ">>=" | ">>>=" | "**=" | "&&=" | "||=" | "??=" => Some(op),
                _ => None,
            };
            if let Some(op) = assign_op {
                self.at += 1;
                let value = self.assignment()?;
                return Ok(Expr::Assign {
                    op,
                    target: Box::new(left),
                    value: Box::new(value),
                });
            }
        }
        Ok(left)
    }

    /// Recognise `x => ...` and `(a, b) => ...`.
    fn try_arrow(&mut self) -> Result<Option<Expr>, (String, u32)> {
        let save = self.at;

        // `name =>`
        if let Tok::Name(name) = self.peek().clone() {
            if matches!(&self.toks[(self.at + 1).min(self.toks.len() - 1)].tok, Tok::Punct("=>")) {
                self.at += 2;
                let body = self.arrow_body()?;
                let def = FnDef {
                    name: Rc::from(""),
                    params: vec![name],
                    rest: None,
                    body: body.0,
                    expr_body: body.1,
                };
                return Ok(Some(Expr::Function(Rc::new(def))));
            }
            return Ok(None);
        }

        if !self.is_punct("(") {
            return Ok(None);
        }

        // Look past the parentheses for `=>`, allowing anything but a newline
        // and only names and commas in between.  Anything else is a grouped
        // expression, and this has to give the position back untouched.
        let mut depth = 0i32;
        let mut j = self.at;
        let mut simple = true;
        while j < self.toks.len() {
            match &self.toks[j].tok {
                Tok::Punct("(") => depth += 1,
                Tok::Punct(")") => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                Tok::Name(_) | Tok::Punct(",") | Tok::Punct("...") | Tok::Newline => {}
                _ => simple = false,
            }
            j += 1;
        }
        if j + 1 >= self.toks.len()
            || !matches!(&self.toks[j + 1].tok, Tok::Punct("=>"))
            || !simple
        {
            return Ok(None);
        }
        let (params, rest) = self.parameters()?;
        if !self.eat_punct("=>") {
            self.at = save;
            return Ok(None);
        }
        let body = self.arrow_body()?;
        let def = FnDef {
            name: Rc::from(""),
            params,
            rest,
            body: body.0,
            expr_body: body.1,
        };
        Ok(Some(Expr::Function(Rc::new(def))))
    }

    /// An arrow's body: a block, or a single expression.
    fn arrow_body(&mut self) -> Result<(Vec<Stmt>, Option<Expr>), (String, u32)> {
        self.skip_newlines();
        if self.is_punct("{") {
            Ok((self.block()?, None))
        } else {
            Ok((Vec::new(), Some(self.assignment()?)))
        }
    }

    fn conditional(&mut self) -> Result<Expr, (String, u32)> {
        let test = self.binary(0)?;
        if self.eat_punct("?") {
            let yes = self.assignment()?;
            self.expect_punct(":")?;
            let no = self.assignment()?;
            return Ok(Expr::Cond {
                test: Box::new(test),
                yes: Box::new(yes),
                no: Box::new(no),
            });
        }
        Ok(test)
    }

    fn binary(&mut self, min_prec: u8) -> Result<Expr, (String, u32)> {
        let mut left = self.unary()?;
        loop {
            let op: &'static str = match self.peek() {
                Tok::Punct(p) => match *p {
                    "=" | "?" | ":" | "," | ")" | "]" | "}" | ";" | "=>" => break,
                    _ => p,
                },
                Tok::Keyword("in") if self.no_in == 0 => "in",
                Tok::Keyword("instanceof") => "instanceof",
                // `of` inside a for head is the loop's, not an operator, and
                // anywhere else it is just a name.
                _ => break,
            };
            let prec = precedence(op);
            if prec == 0 || prec < min_prec {
                break;
            }
            self.at += 1;
            // `**` is right-associative; everything else is left.
            let next_min = if op == "**" { prec } else { prec + 1 };
            let right = self.binary(next_min)?;
            left = match op {
                "&&" | "||" | "??" => Expr::Logical {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                _ => Expr::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, (String, u32)> {
        if let Tok::Punct(op) = self.peek().clone() {
            match op {
                "!" | "-" | "+" | "~" => {
                    self.at += 1;
                    let e = self.unary()?;
                    return Ok(Expr::Unary { op, expr: Box::new(e) });
                }
                "++" | "--" => {
                    self.at += 1;
                    let e = self.unary()?;
                    return Ok(Expr::Update { op, target: Box::new(e), prefix: true });
                }
                _ => {}
            }
        }
        if let Tok::Keyword(k) = self.peek().clone() {
            match k {
                "typeof" | "delete" | "void" => {
                    self.at += 1;
                    let e = self.unary()?;
                    return Ok(Expr::Unary { op: k, expr: Box::new(e) });
                }
                "new" => {
                    self.at += 1;
                    let callee = self.unary()?;
                    // `new X` and `new X(...)` are different: the first has no
                    // arguments, and the call has already been parsed into the
                    // callee if there were any.
                    return match callee {
                        Expr::Call { callee, args } => Ok(Expr::New { callee, args }),
                        other => Ok(Expr::New { callee: Box::new(other), args: Vec::new() }),
                    };
                }
                _ => {}
            }
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, (String, u32)> {
        let mut e = self.primary()?;
        loop {
            if self.eat_punct(".") || self.eat_punct("?.") {
                let name = self.expect_name()?;
                e = Expr::Member {
                    object: Box::new(e),
                    key: Box::new(Expr::Str(name)),
                    computed: false,
                };
                continue;
            }
            if self.eat_punct("[") {
                let key = self.expression()?;
                self.expect_punct("]")?;
                e = Expr::Member {
                    object: Box::new(e),
                    key: Box::new(key),
                    computed: true,
                };
                continue;
            }
            if self.is_punct("(") {
                let args = self.arguments()?;
                e = Expr::Call { callee: Box::new(e), args };
                continue;
            }
            // A postfix `++` has to be on the same line as its operand.
            if !self.at_newline() {
                if let Tok::Punct(op) = self.peek().clone() {
                    if op == "++" || op == "--" {
                        self.at += 1;
                        e = Expr::Update { op, target: Box::new(e), prefix: false };
                        continue;
                    }
                }
            }
            break;
        }
        Ok(e)
    }

    fn arguments(&mut self) -> Result<Vec<Expr>, (String, u32)> {
        self.expect_punct("(")?;
        let mut args = Vec::new();
        self.skip_newlines();
        while !self.is_punct(")") {
            // A spread argument in a call is passed as one value; `apply` is
            // the way to do it properly and it is available.
            if self.eat_punct("...") {
                let e = self.assignment()?;
                args.push(e);
            } else {
                args.push(self.assignment()?);
            }
            self.skip_newlines();
            if !self.eat_punct(",") {
                break;
            }
            self.skip_newlines();
        }
        self.expect_punct(")")?;
        Ok(args)
    }

    fn primary(&mut self) -> Result<Expr, (String, u32)> {
        match self.peek().clone() {
            Tok::Number(v) => {
                self.at += 1;
                Ok(Expr::Num(v))
            }
            Tok::Str(s) | Tok::Template(s) => {
                self.at += 1;
                Ok(Expr::Str(s))
            }
            Tok::Regex => {
                self.at += 1;
                Err(("regular expressions are not supported".to_string(), self.line()))
            }
            Tok::Name(n) => {
                self.at += 1;
                Ok(Expr::Ident(n))
            }
            Tok::Keyword(k) => match k {
                "true" => {
                    self.at += 1;
                    Ok(Expr::Bool(true))
                }
                "false" => {
                    self.at += 1;
                    Ok(Expr::Bool(false))
                }
                "null" => {
                    self.at += 1;
                    Ok(Expr::Null)
                }
                "undefined" => {
                    self.at += 1;
                    Ok(Expr::Undefined)
                }
                "this" => {
                    self.at += 1;
                    Ok(Expr::This)
                }
                "function" => {
                    self.at += 1;
                    let f = self.function_rest(false)?;
                    Ok(Expr::Function(f))
                }
                other => Err((alloc::format!("unexpected keyword `{}`", other), self.line())),
            },
            Tok::Punct("(") => {
                self.at += 1;
                self.skip_newlines();
                let e = self.expression()?;
                self.skip_newlines();
                self.expect_punct(")")?;
                Ok(e)
            }
            Tok::Punct("[") => {
                self.at += 1;
                let mut items = Vec::new();
                self.skip_newlines();
                while !self.is_punct("]") {
                    if self.is_punct(",") {
                        // A hole in an array literal.
                        items.push(Expr::Undefined);
                        self.at += 1;
                        continue;
                    }
                    items.push(self.assignment()?);
                    self.skip_newlines();
                    if !self.eat_punct(",") {
                        break;
                    }
                    self.skip_newlines();
                }
                self.expect_punct("]")?;
                Ok(Expr::Array(items))
            }
            Tok::Punct("{") => {
                self.at += 1;
                let mut props = Vec::new();
                self.skip_newlines();
                while !self.is_punct("}") {
                    if self.eat_punct("...") {
                        let e = self.assignment()?;
                        props.push((PropKey::Named(Rc::from("\u{0}spread")), e));
                        self.skip_newlines();
                        if !self.eat_punct(",") {
                            break;
                        }
                        self.skip_newlines();
                        continue;
                    }
                    let key = match self.next() {
                        Tok::Name(n) => PropKey::Named(n),
                        Tok::Str(s) => PropKey::Named(s),
                        Tok::Number(v) => {
                            PropKey::Named(Rc::from(format_number(v).as_str()))
                        }
                        Tok::Keyword(k) => PropKey::Named(Rc::from(k)),
                        Tok::Punct("[") => {
                            let e = self.expression()?;
                            self.expect_punct("]")?;
                            PropKey::Computed(e)
                        }
                        other => {
                            return Err((
                                alloc::format!("unexpected {:?} in an object literal", other),
                                self.line(),
                            ))
                        }
                    };
                    let value = if self.eat_punct(":") {
                        self.assignment()?
                    } else if let PropKey::Named(n) = &key {
                        // Shorthand `{ a }` and `{ a() {} }`.
                        if self.is_punct("(") {
                            let (params, rest) = self.parameters()?;
                            let body = self.block()?;
                            Expr::Function(Rc::new(FnDef {
                                name: n.clone(),
                                params,
                                rest,
                                body,
                                expr_body: None,
                            }))
                        } else {
                            Expr::Ident(n.clone())
                        }
                    } else {
                        Expr::Undefined
                    };
                    props.push((key, value));
                    self.skip_newlines();
                    if !self.eat_punct(",") {
                        break;
                    }
                    self.skip_newlines();
                }
                self.expect_punct("}")?;
                Ok(Expr::Object(props))
            }
            other => Err((alloc::format!("unexpected {:?}", other), self.line())),
        }
    }
}

/// The shortest text that reads back as the same number.
pub fn format_number(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if v == 0.0 {
        return "0".to_string();
    }
    if num::fract(v) == 0.0 && num::abs(v) < 1e21 {
        return alloc::format!("{}", v as i64);
    }
    alloc::format!("{}", v)
}
