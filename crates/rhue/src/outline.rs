//! What editors need from a file without type-checking it: its symbols, its
//! tokens classified for highlighting, and how it uses and defines module
//! fields (for go to definition and find references across files). Parsing and
//! resolving names is enough for all of it, so it's cheap to do for every file
//! in a workspace.

use std::collections::HashMap;

use rhue_compiler::resolve::{Binding, Resolution};
use rhue_lexer::Token;
use rhue_parser::ast::{
    Block, Chunk, Expr, ExprKind, FunctionBody, GenericParam, InterpPart, Name, Stmt, StmtKind,
    TableItem, TypeExpr, TypeKind, TypePack,
};

use crate::Span;

/// A file's outline; see [`outline`].
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Outline {
    /// Its functions, variables, fields and types, nested as written: `function
    /// M.f()` sits under `local M`.
    pub symbols: Vec<Symbol>,
    /// Every keyword, literal and name, classified, in source order with no
    /// overlaps. Only keywords and literals when the file doesn't parse.
    pub tokens: Vec<SemanticToken>,
    /// Locals bound to a module. `local m = require("m")` gives the declaration
    /// of `m` and `"m"`.
    pub requires: Vec<(Span, String)>,
    /// Every `x.name` and `x:name(...)` where `x` is a local or a
    /// `require(...)` call.
    pub field_uses: Vec<FieldUse>,
    /// What the file returns, when it returns a local or a table literal.
    pub exports: Exports,
}

/// A named thing defined in a file, for outlines and workspace symbols.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Symbol {
    /// Its name (a field's own name, `f` for `M.f`, listed under `M`).
    pub name: String,
    /// What kind of thing it is.
    pub kind: SymbolKind,
    /// The whole definition.
    pub span: Span,
    /// Just its name.
    pub name_span: Span,
    /// What the checker's type for it is recorded on (the name, or for a
    /// function, the function).
    pub type_span: Span,
    /// What's defined inside it (a table's fields, a class's methods).
    pub children: Vec<Symbol>,
}

/// What kind of thing a [`Symbol`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SymbolKind {
    /// A function (`local function f`, `function f`).
    Function,
    /// A function stored in a table's field (`function M.f`, `function T:m`, `M.f = function ...`).
    Method,
    /// A local or global variable.
    Variable,
    /// A table field holding something other than a function.
    Field,
    /// A local holding a module (`local m = require(...)`).
    Module,
    /// A type alias (`type T = ...`).
    Type,
}

/// A highlighted range of source, for semantic highlighting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SemanticToken {
    /// The range it covers.
    pub span: Span,
    /// How to colour it.
    pub kind: TokenKind,
    /// Where the name is declared (or a field defined).
    pub declaration: bool,
    /// A global (not a local).
    pub global: bool,
}

/// What a [`SemanticToken`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TokenKind {
    /// A keyword (`local`, `function`, `if`...).
    Keyword,
    /// A string literal.
    String,
    /// A number literal.
    Number,
    /// A variable.
    Variable,
    /// A function parameter.
    Parameter,
    /// A name holding a function.
    Function,
    /// A method name (after `:`).
    Method,
    /// A field name (after `.`).
    Property,
    /// A type name.
    Type,
    /// A generic type parameter (`T` in `<T>`).
    TypeParameter,
    /// A local holding a module (`local m = require("m")`).
    Namespace,
}

/// A field read from a local or a module (`m.name`, `m:name()`), for navigating
/// across modules.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct FieldUse {
    /// The field's name as written.
    pub span: Span,
    /// The field's name.
    pub name: String,
    /// What it's read from.
    pub base: Base,
}

/// What a field is read from.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Base {
    /// The local declared at this span.
    Local(Span),
    /// `require("name")` itself.
    Module(String),
}

/// What a module file exports, for navigating across modules.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Exports {
    /// For `return M`, where `M` is declared.
    pub local: Option<Span>,
    /// The fields it's known to have and where each is defined: `function
    /// M.f()`, `M.x = ...` and fields of `local M = { ... }` at the top level,
    /// or the fields of a returned table literal (where a field's value is a
    /// local, the local's declaration).
    pub fields: Vec<(String, Span)>,
}

/// The outline of `source` (see [`Outline`]). A file that doesn't parse or
/// resolve only gets its lexical tokens.
pub fn outline(source: &str) -> Outline {
    let mut out = Outline::default();
    let lexed = rhue_lexer::lex(source);
    let Ok(chunk) = rhue_parser::parse_chunk(source) else {
        out.tokens = lexical(&lexed.tokens, &[]);
        return out;
    };
    let Ok(res) = rhue_compiler::resolve::resolve(&chunk) else {
        out.tokens = lexical(&lexed.tokens, &[]);
        return out;
    };
    let mut w = Walker {
        source,
        res: &res,
        decls: HashMap::new(),
        names: Vec::new(),
        holes: Vec::new(),
        generics: Vec::new(),
        out: &mut out,
    };
    w.block(&chunk.body);
    let names = std::mem::take(&mut w.names);
    let holes = std::mem::take(&mut w.holes);
    let mut tokens = lexical(&lexed.tokens, &holes);
    tokens.extend(names);
    tokens.sort_by_key(|t| (t.span.start, std::cmp::Reverse(t.span.end)));
    // keep the first of overlapping tokens (a name reached two ways)
    let mut end = 0;
    tokens.retain(|t| {
        let keep = t.span.start >= end && t.span.end > t.span.start;
        if keep {
            end = t.span.end;
        }
        keep
    });
    out.tokens = tokens;
    out.symbols = symbols(&chunk.body, &res, true);
    out.exports = exports(&chunk, &res);
    out
}

/// Keywords and literals, interpolated strings minus their holes.
fn lexical(tokens: &[rhue_lexer::Spanned], holes: &[Span]) -> Vec<SemanticToken> {
    let mut out = Vec::new();
    let mut push = |span: Span, kind| {
        out.push(SemanticToken {
            span,
            kind,
            declaration: false,
            global: false,
        })
    };
    for t in tokens {
        use Token::*;
        let kind = match t.token {
            And | Break | Do | Else | Elseif | End | False | For | Function | If | In | Local
            | Nil | Not | Or | Repeat | Return | Then | True | Until | While => TokenKind::Keyword,
            Number(_) => TokenKind::Number,
            String => TokenKind::String,
            InterpString => {
                let mut at = t.span.start;
                for h in holes
                    .iter()
                    .filter(|h| h.start >= t.span.start && h.end <= t.span.end)
                {
                    if h.start > at {
                        push(Span::new(at, h.start), TokenKind::String);
                    }
                    at = at.max(h.end);
                }
                if at < t.span.end {
                    push(Span::new(at, t.span.end), TokenKind::String);
                }
                continue;
            }
            _ => continue,
        };
        push(t.span, kind);
    }
    out
}

struct Walker<'a, 'o> {
    source: &'a str,
    res: &'a Resolution,
    /// What each local is, by where it's declared.
    decls: HashMap<Span, TokenKind>,
    names: Vec<SemanticToken>,
    /// Interpolation holes, which aren't string.
    holes: Vec<Span>,
    /// Generic parameters in scope.
    generics: Vec<std::rc::Rc<str>>,
    out: &'o mut Outline,
}

fn is_require(e: &Expr, res: &Resolution) -> Option<String> {
    let ExprKind::Call { callee, args } = &e.kind else {
        return None;
    };
    match (&callee.kind, args.as_slice()) {
        (ExprKind::Name(n), [arg])
            if &*n.text == "require" && matches!(res.binding(n), Binding::Global) =>
        {
            match &arg.kind {
                ExprKind::String(s) => Some(s.to_string()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `x.name` written with a dot (not `x["name"]`), the name.
fn dotted(key: &Expr) -> Option<&str> {
    match &key.kind {
        ExprKind::String(s) if (key.span.end - key.span.start) as usize == s.len() => Some(s),
        _ => None,
    }
}

impl Walker<'_, '_> {
    fn token(&mut self, span: Span, kind: TokenKind, declaration: bool, global: bool) {
        self.names.push(SemanticToken {
            span,
            kind,
            declaration,
            global,
        });
    }

    fn declare(&mut self, name: &Name, kind: TokenKind) {
        self.decls.insert(name.span, kind);
        self.token(name.span, kind, true, false);
    }

    /// The local `name` refers to, where it's declared.
    fn local_decl(&self, name: &Name) -> Option<Span> {
        match self.res.binding(name) {
            Binding::Local(id) => Some(self.res.locals[id as usize].span),
            Binding::Global => None,
        }
    }

    fn base(&self, object: &Expr) -> Option<Base> {
        match &object.kind {
            ExprKind::Name(n) => self.local_decl(n).map(Base::Local),
            _ => is_require(object, self.res).map(Base::Module),
        }
    }

    fn field_use(&mut self, object: &Expr, name: &str, span: Span) {
        if let Some(base) = self.base(object) {
            self.out.field_uses.push(FieldUse {
                span,
                name: name.to_string(),
                base,
            });
        }
    }

    fn block(&mut self, b: &Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(r) = &b.ret {
            for v in &r.values {
                self.expr(v, false);
            }
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "offsets in a source, which the lexer caps at 4 GiB"
    )]
    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Local {
                names,
                types,
                values,
            } => {
                for v in values {
                    self.expr(v, false);
                }
                for t in types.iter().flatten() {
                    self.ty(t);
                }
                for (i, n) in names.iter().enumerate() {
                    let kind = match values.get(i).map(|v| &v.kind) {
                        Some(ExprKind::Function(_)) => TokenKind::Function,
                        _ if names.len() == 1 && values.len() == 1 => {
                            match is_require(&values[0], self.res) {
                                Some(m) => {
                                    self.out.requires.push((n.span, m));
                                    TokenKind::Namespace
                                }
                                None => TokenKind::Variable,
                            }
                        }
                        _ => TokenKind::Variable,
                    };
                    self.declare(n, kind);
                }
            }
            StmtKind::Assign { targets, values } => {
                for t in targets {
                    self.target(t, false);
                }
                for v in values {
                    self.expr(v, false);
                }
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.target(target, false);
                self.expr(value, false);
            }
            StmtKind::Do(b) => self.block(b),
            StmtKind::While { cond, body } => {
                self.expr(cond, false);
                self.block(body);
            }
            StmtKind::Repeat { body, cond } => {
                self.block(body);
                self.expr(cond, false);
            }
            StmtKind::If {
                branches,
                else_block,
            } => {
                for (c, b) in branches {
                    self.expr(c, false);
                    self.block(b);
                }
                if let Some(b) = else_block {
                    self.block(b);
                }
            }
            StmtKind::NumericFor {
                var,
                var_type,
                start,
                limit,
                step,
                body,
            } => {
                self.expr(start, false);
                self.expr(limit, false);
                if let Some(e) = step {
                    self.expr(e, false);
                }
                if let Some(t) = var_type {
                    self.ty(t);
                }
                self.declare(var, TokenKind::Variable);
                self.block(body);
            }
            StmtKind::GenericFor {
                names,
                types,
                exprs,
                body,
            } => {
                for e in exprs {
                    self.expr(e, false);
                }
                for t in types.iter().flatten() {
                    self.ty(t);
                }
                for n in names {
                    self.declare(n, TokenKind::Variable);
                }
                self.block(body);
            }
            StmtKind::Break => {}
            StmtKind::Continue => self.token(s.span, TokenKind::Keyword, false, false),
            StmtKind::Call(e) => self.expr(e, false),
            StmtKind::Function { target, func } => {
                self.target(target, true);
                self.function(func);
            }
            StmtKind::LocalFunction { name, func } => {
                self.declare(name, TokenKind::Function);
                self.function(func);
            }
            StmtKind::TypeAlias(a) => {
                // `export type` / `type`, contextual keywords
                let head = &self.source[s.span.start as usize..a.name_span.start as usize];
                let mut at = s.span.start;
                for word in head.split_whitespace() {
                    if let Some(i) = self.source[at as usize..].find(word) {
                        let start = at + i as u32;
                        self.token(
                            Span::new(start, start + word.len() as u32),
                            TokenKind::Keyword,
                            false,
                            false,
                        );
                        at = start + word.len() as u32;
                    }
                }
                self.token(a.name_span, TokenKind::Type, true, false);
                self.open_generics(&a.generics);
                self.ty(&a.ty);
                self.generics
                    .truncate(self.generics.len() - a.generics.len());
            }
        }
    }

    /// An assignment target. `defines` for `function a.b()`.
    fn target(&mut self, t: &Expr, defines: bool) {
        match &t.kind {
            ExprKind::Name(n) => self.name(n, defines, defines),
            ExprKind::Index { object, key } if dotted(key).is_some() => {
                self.expr(object, false);
                let name = dotted(key).unwrap_or_default();
                self.field_use(object, name, key.span);
                let kind = if defines {
                    TokenKind::Method
                } else {
                    TokenKind::Property
                };
                self.token(key.span, kind, defines, false);
            }
            _ => self.expr(t, false),
        }
    }

    /// A name used (or, with `defines`, a global function defined).
    fn name(&mut self, n: &Name, called: bool, defines: bool) {
        match self.local_decl(n) {
            Some(decl) => {
                let kind = self
                    .decls
                    .get(&decl)
                    .copied()
                    .unwrap_or(TokenKind::Variable);
                let kind = if called && kind == TokenKind::Variable {
                    TokenKind::Function
                } else {
                    kind
                };
                self.token(n.span, kind, decl == n.span, false);
            }
            None => {
                let kind = if called {
                    TokenKind::Function
                } else {
                    TokenKind::Variable
                };
                self.token(n.span, kind, defines, true);
            }
        }
    }

    fn function(&mut self, f: &FunctionBody) {
        self.open_generics(&f.generics);
        for (i, p) in f.params.iter().enumerate() {
            if f.is_method && i == 0 {
                // the implicit `self` isn't written
                self.decls.insert(p.span, TokenKind::Parameter);
                continue;
            }
            self.declare(p, TokenKind::Parameter);
        }
        for t in f.param_types.iter().flatten() {
            self.ty(t);
        }
        if let Some(t) = &f.vararg_type {
            self.ty(t);
        }
        if let Some(p) = &f.return_type {
            self.pack(p);
        }
        self.block(&f.body);
        self.generics
            .truncate(self.generics.len() - f.generics.len());
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "offsets in a source, which the lexer caps at 4 GiB"
    )]
    fn open_generics(&mut self, generics: &[GenericParam]) {
        for g in generics {
            let end = g.span.start + g.name.len() as u32;
            self.token(
                Span::new(g.span.start, end),
                TokenKind::TypeParameter,
                true,
                false,
            );
            self.generics.push(g.name.clone());
        }
        for g in generics {
            if let Some(d) = &g.default {
                self.ty(d);
            }
        }
    }

    /// `called` means it's the callee of a call (a name there is a function).
    fn expr(&mut self, e: &Expr, called: bool) {
        match &e.kind {
            ExprKind::Nil
            | ExprKind::Bool(_)
            | ExprKind::Number(_)
            | ExprKind::String(_)
            | ExprKind::Vararg => {}
            ExprKind::Interp(parts) => {
                for p in parts {
                    if let InterpPart::Expr(x) = p {
                        self.holes.push(x.span);
                        self.expr(x, false);
                    }
                }
            }
            ExprKind::Name(n) => self.name(n, called, false),
            ExprKind::Group(x) => self.expr(x, false),
            ExprKind::Unary { operand, .. } => self.expr(operand, false),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs, false);
                self.expr(rhs, false);
            }
            ExprKind::IfElse {
                branches,
                else_expr,
            } => {
                for (c, v) in branches {
                    self.expr(c, false);
                    self.expr(v, false);
                }
                self.expr(else_expr, false);
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee, true);
                for a in args {
                    self.expr(a, false);
                }
            }
            ExprKind::Function(f) => self.function(f),
            ExprKind::Index { object, key } => {
                self.expr(object, false);
                match dotted(key) {
                    Some(name) => {
                        self.field_use(object, name, key.span);
                        let kind = if called {
                            TokenKind::Method
                        } else {
                            TokenKind::Property
                        };
                        self.token(key.span, kind, false, false);
                    }
                    None => self.expr(key, false),
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                method_span,
                args,
            } => {
                self.expr(object, false);
                self.field_use(object, method, *method_span);
                self.token(*method_span, TokenKind::Method, false, false);
                for a in args {
                    self.expr(a, false);
                }
            }
            ExprKind::Table(items) => {
                for item in items {
                    match item {
                        TableItem::Positional(v) => self.expr(v, false),
                        TableItem::Keyed { key, value } => {
                            match dotted(key) {
                                Some(_) => {
                                    let kind = if matches!(value.kind, ExprKind::Function(_)) {
                                        TokenKind::Method
                                    } else {
                                        TokenKind::Property
                                    };
                                    self.token(key.span, kind, true, false);
                                }
                                None => self.expr(key, false),
                            }
                            self.expr(value, false);
                        }
                    }
                }
            }
            ExprKind::Cast { expr, ty } => {
                self.expr(expr, false);
                self.ty(ty);
            }
        }
    }

    fn pack(&mut self, p: &TypePack) {
        for t in &p.types {
            self.ty(t);
        }
        if let Some(v) = &p.variadic {
            self.ty(v);
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "offsets in a source, which the lexer caps at 4 GiB"
    )]
    fn ty(&mut self, t: &TypeExpr) {
        match &t.kind {
            TypeKind::Name { name, args } => {
                let span = Span::new(t.span.start, t.span.start + name.len() as u32);
                let kind = if self.generics.contains(name) {
                    TokenKind::TypeParameter
                } else {
                    TokenKind::Type
                };
                // `nil` already lexes as a keyword
                if &**name != "nil" {
                    self.token(span, kind, false, false);
                }
                for a in args {
                    self.ty(a);
                }
            }
            TypeKind::Singleton(_) => {}
            TypeKind::Typeof(e) => {
                let end = t.span.start + "typeof".len() as u32;
                self.token(
                    Span::new(t.span.start, end),
                    TokenKind::Keyword,
                    false,
                    false,
                );
                self.expr(e, false);
            }
            TypeKind::Optional(x) => self.ty(x),
            TypeKind::Union(xs) | TypeKind::Intersection(xs) => {
                for x in xs {
                    self.ty(x);
                }
            }
            TypeKind::Table { props, indexer } => {
                for p in props {
                    // the span starts at the name, `read`/`write` is before it
                    let prefix = p.access.prefix().trim_end();
                    let before = self.source[..p.span.start as usize].trim_end();
                    if !prefix.is_empty() && before.ends_with(prefix) {
                        let end = before.len() as u32;
                        let span = Span::new(end - prefix.len() as u32, end);
                        self.token(span, TokenKind::Keyword, false, false);
                    }
                    let text = &self.source[p.span.start as usize..p.span.end as usize];
                    if text.starts_with(&*p.name) {
                        let span = Span::new(p.span.start, p.span.start + p.name.len() as u32);
                        self.token(span, TokenKind::Property, true, false);
                    }
                    self.ty(&p.ty);
                }
                if let Some(ix) = indexer {
                    self.ty(&ix.0);
                    self.ty(&ix.1);
                }
            }
            TypeKind::GenericPack(name) => {
                let span = Span::new(t.span.start, t.span.start + name.len() as u32);
                self.token(span, TokenKind::TypeParameter, false, false);
            }
            TypeKind::Pack(p) => self.pack(p),
            TypeKind::Function {
                generics,
                params,
                variadic,
                returns,
            } => {
                self.open_generics(generics);
                for p in params {
                    self.ty(&p.ty);
                }
                if let Some(v) = variadic {
                    self.ty(v);
                }
                self.pack(returns);
                self.generics.truncate(self.generics.len() - generics.len());
            }
        }
    }
}

// ─ Symbols ───────────────────────────────────────────────────────────────────

fn symbol(name: &str, kind: SymbolKind, span: Span, name_span: Span) -> Symbol {
    Symbol {
        name: name.to_string(),
        kind,
        span,
        name_span,
        type_span: name_span,
        children: Vec::new(),
    }
}

/// `a.b.c` as written, and the name of each step's key span.
fn path_of(e: &Expr) -> Option<(Vec<String>, Span)> {
    match &e.kind {
        ExprKind::Name(n) => Some((vec![n.text.to_string()], n.span)),
        ExprKind::Index { object, key } => {
            let (mut path, _) = path_of(object)?;
            path.push(dotted(key)?.to_string());
            Some((path, key.span))
        }
        _ => None,
    }
}

/// `a.b.c`, or `a.b:c` for a method.
fn joined(parts: &[String], method: bool) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => {
            let sep = if method { ":" } else { "." };
            format!("{}{sep}{last}", init.join("."))
        }
    }
}

/// The symbols a block defines. At the top level every local and global is one,
/// inside functions and blocks only functions are.
fn symbols(b: &Block, res: &Resolution, top: bool) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    for s in &b.stmts {
        match &s.kind {
            StmtKind::Local { names, values, .. } => {
                for (i, n) in names.iter().enumerate() {
                    let value = values.get(i);
                    let (kind, children) = match value {
                        Some(Expr {
                            kind: ExprKind::Function(f),
                            ..
                        }) => (SymbolKind::Function, symbols(&f.body, res, false)),
                        _ if !top => continue,
                        Some(Expr {
                            kind: ExprKind::Table(items),
                            ..
                        }) => (SymbolKind::Variable, table_fields(items, res)),
                        Some(v) if is_require(v, res).is_some() => (SymbolKind::Module, Vec::new()),
                        _ => (SymbolKind::Variable, Vec::new()),
                    };
                    let mut sym = symbol(&n.text, kind, s.span, n.span);
                    sym.children = children;
                    out.push(sym);
                }
            }
            StmtKind::LocalFunction { name, func } => {
                let mut sym = symbol(&name.text, SymbolKind::Function, s.span, name.span);
                sym.children = symbols(&func.body, res, false);
                out.push(sym);
            }
            StmtKind::Function { target, func } => {
                let Some((path, name_span)) = path_of(target) else {
                    continue;
                };
                let kind = if path.len() > 1 {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                // under the local or global it's defined on, if that's here
                let parent = (path.len() > 1)
                    .then(|| out.iter_mut().rev().find(|p| p.name == path[0]))
                    .flatten();
                let shown = if parent.is_some() {
                    &path[1..]
                } else {
                    &path[..]
                };
                let mut sym = symbol(&joined(shown, func.is_method), kind, s.span, name_span);
                sym.type_span = func.span;
                sym.children = symbols(&func.body, res, false);
                match parent {
                    Some(p) => p.children.push(sym),
                    None => out.push(sym),
                }
            }
            StmtKind::Assign { targets, values } if top => {
                for (i, t) in targets.iter().enumerate() {
                    let value = values.get(i);
                    let is_fn = matches!(value.map(|v| &v.kind), Some(ExprKind::Function(_)));
                    match &t.kind {
                        ExprKind::Name(n) if matches!(res.binding(n), Binding::Global) => {
                            if out.iter().any(|p| p.name == *n.text) {
                                continue;
                            }
                            let (kind, children) = match value.map(|v| &v.kind) {
                                Some(ExprKind::Function(f)) => {
                                    (SymbolKind::Function, symbols(&f.body, res, false))
                                }
                                Some(ExprKind::Table(items)) => {
                                    (SymbolKind::Variable, table_fields(items, res))
                                }
                                _ => (SymbolKind::Variable, Vec::new()),
                            };
                            let mut sym = symbol(&n.text, kind, s.span, n.span);
                            sym.children = children;
                            out.push(sym);
                        }
                        ExprKind::Index { object, key } => {
                            let (ExprKind::Name(n), Some(field)) = (&object.kind, dotted(key))
                            else {
                                continue;
                            };
                            let Some(parent) = out.iter_mut().rev().find(|p| p.name == *n.text)
                            else {
                                continue;
                            };
                            if parent.children.iter().any(|c| c.name == field) {
                                continue;
                            }
                            let kind = if is_fn {
                                SymbolKind::Method
                            } else {
                                SymbolKind::Field
                            };
                            let mut sym = symbol(field, kind, s.span, key.span);
                            if let Some(v) = value {
                                sym.type_span = v.span;
                            }
                            parent.children.push(sym);
                        }
                        _ => {}
                    }
                }
            }
            StmtKind::TypeAlias(a) => {
                out.push(symbol(&a.name, SymbolKind::Type, s.span, a.name_span));
            }
            StmtKind::Do(b) => out.extend(symbols(b, res, false)),
            StmtKind::While { body, .. }
            | StmtKind::Repeat { body, .. }
            | StmtKind::NumericFor { body, .. }
            | StmtKind::GenericFor { body, .. } => out.extend(symbols(body, res, false)),
            StmtKind::If {
                branches,
                else_block,
            } => {
                for (_, b) in branches {
                    out.extend(symbols(b, res, false));
                }
                if let Some(b) = else_block {
                    out.extend(symbols(b, res, false));
                }
            }
            _ => {}
        }
    }
    out
}

fn table_fields(items: &[TableItem], res: &Resolution) -> Vec<Symbol> {
    items
        .iter()
        .filter_map(|item| {
            let TableItem::Keyed { key, value } = item else {
                return None;
            };
            let name = dotted(key)?;
            let (kind, children) = match &value.kind {
                ExprKind::Function(f) => (SymbolKind::Method, symbols(&f.body, res, false)),
                ExprKind::Table(items) => (SymbolKind::Field, table_fields(items, res)),
                _ => (SymbolKind::Field, Vec::new()),
            };
            let mut sym = symbol(name, kind, key.span.to(value.span), key.span);
            sym.type_span = value.span;
            sym.children = children;
            Some(sym)
        })
        .collect()
}

// ─ Exports ───────────────────────────────────────────────────────────────────

fn exports(chunk: &Chunk, res: &Resolution) -> Exports {
    let mut out = Exports::default();
    let Some(ret) = &chunk.body.ret else {
        return out;
    };
    let [value] = ret.values.as_slice() else {
        return out;
    };
    let decl_of = |n: &Name| match res.binding(n) {
        Binding::Local(id) => Some(res.locals[id as usize].span),
        Binding::Global => None,
    };
    let mut add = |out: &mut Exports, name: &str, span: Span| {
        if !out.fields.iter().any(|(n, _)| n == name) {
            out.fields.push((name.to_string(), span));
        }
    };
    let keyed =
        |out: &mut Exports, items: &[TableItem], add: &mut dyn FnMut(&mut Exports, &str, Span)| {
            for item in items {
                if let TableItem::Keyed { key, value } = item
                    && let Some(name) = dotted(key)
                {
                    let span = match &value.kind {
                        ExprKind::Name(n) => decl_of(n).unwrap_or(key.span),
                        _ => key.span,
                    };
                    add(out, name, span);
                }
            }
        };
    match &value.kind {
        ExprKind::Table(items) => keyed(&mut out, items, &mut add),
        ExprKind::Name(n) => {
            let Some(decl) = decl_of(n) else {
                return out;
            };
            out.local = Some(decl);
            let on_it = |e: &Expr| matches!(&e.kind, ExprKind::Name(m) if decl_of(m) == Some(decl));
            for s in &chunk.body.stmts {
                match &s.kind {
                    StmtKind::Local { names, values, .. } => {
                        for (i, m) in names.iter().enumerate() {
                            if m.span == decl
                                && let Some(Expr {
                                    kind: ExprKind::Table(items),
                                    ..
                                }) = values.get(i)
                            {
                                keyed(&mut out, items, &mut add);
                            }
                        }
                    }
                    StmtKind::Function { target, .. } => {
                        if let ExprKind::Index { object, key } = &target.kind
                            && on_it(object)
                            && let Some(name) = dotted(key)
                        {
                            add(&mut out, name, key.span);
                        }
                    }
                    StmtKind::Assign { targets, .. } => {
                        for t in targets {
                            if let ExprKind::Index { object, key } = &t.kind
                                && on_it(object)
                                && let Some(name) = dotted(key)
                            {
                                add(&mut out, name, key.span);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}
