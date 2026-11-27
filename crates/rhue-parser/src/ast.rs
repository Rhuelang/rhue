use std::fmt;
use std::rc::Rc;

use rhue_common::Span;

/// One occurrence of a name, a declaration or a use. The parser hands these out
/// densely from 0, so later passes can keep side tables in a `Vec` indexed by
/// `NameId` instead of hashing AST nodes.
pub type NameId = u32;

#[derive(Clone, Debug, PartialEq)]
pub struct Name {
    pub text: Rc<str>,
    pub id: NameId,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub body: Block,
    pub span: Span,
    /// How many `NameId`s got handed out, sizes the resolver's tables.
    pub name_count: u32,
    /// The `--!strict` / `--!nonstrict` / `--!nocheck` directive, if any.
    pub mode: Option<(TypeMode, Span)>,
}

/// How hard a chunk gets type-checked, set by a directive comment at the top of
/// the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypeMode {
    /// `--!nocheck`: nothing's checked.
    NoCheck,
    /// `--!nonstrict`: anything unannotated is `any`, only definite mismatches
    /// are errors.
    NonStrict,
    /// `--!strict`: also unknown globals, maybe-nil values and so on.
    Strict,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub ret: Option<Return>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Return {
    pub values: Vec<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    /// `local a, b = 1, 2` (or `local a: number = 1`)
    Local {
        names: Vec<Name>,
        /// One per name, `None` where it's unannotated.
        types: Vec<Option<TypeExpr>>,
        values: Vec<Expr>,
    },
    /// `a, b = b, a`
    Assign {
        targets: Vec<Expr>,
        values: Vec<Expr>,
    },
    /// `x += 1`, `s ..= "!"`
    CompoundAssign {
        op: BinOp,
        op_span: Span,
        target: Expr,
        value: Expr,
    },
    Do(Block),
    While {
        cond: Expr,
        body: Block,
    },
    /// The condition can see the body's locals, same as Lua.
    Repeat {
        body: Block,
        cond: Expr,
    },
    If {
        branches: Vec<(Expr, Block)>,
        else_block: Option<Block>,
    },
    NumericFor {
        var: Name,
        var_type: Option<Box<TypeExpr>>,
        start: Expr,
        limit: Expr,
        step: Option<Expr>,
        body: Block,
    },
    Break,
    Continue,
    /// A call used as a statement (results get thrown away).
    Call(Expr),
    /// `function name(...) end`, assigns a new function to `name`.
    Function {
        target: Expr,
        func: Rc<FunctionBody>,
    },
    /// `local function name(...) end`. `name` is in scope inside the body so
    /// the function can call itself.
    LocalFunction {
        name: Name,
        func: Rc<FunctionBody>,
    },
    /// `for k, v in explist do ... end`. The list gives an iterator function, a
    /// state and a starting control value. Luau's generalized iteration (`for
    /// k, v in t`) also takes a table directly.
    GenericFor {
        names: Vec<Name>,
        types: Vec<Option<TypeExpr>>,
        exprs: Vec<Expr>,
        body: Block,
    },
    /// `type Name<T> = ...` or `export type ...`. Does nothing at runtime.
    TypeAlias(Rc<TypeAlias>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeAlias {
    pub name: Rc<str>,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub ty: TypeExpr,
    pub exported: bool,
}

/// Parameters and body, shared by every way of writing a function.
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionBody {
    /// `<T, U>` after the name.
    pub generics: Vec<GenericParam>,
    pub params: Vec<Name>,
    /// One per parameter, `None` where it's unannotated (always for the
    /// implicit `self` of a `function a:m()` method).
    pub param_types: Vec<Option<TypeExpr>>,
    pub is_vararg: bool,
    /// `...: T`
    pub vararg_type: Option<TypeExpr>,
    /// `): T` or `): (A, B)`
    pub return_type: Option<TypePack>,
    /// Declared with `:`, so `params[0]` is the implicit `self`.
    pub is_method: bool,
    pub body: Block,
    /// The name it was declared with, for tracebacks (`None` if anonymous).
    pub name: Option<Rc<str>>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Nil,
    Bool(bool),
    Number(f64),
    String(Rc<str>),
    /// `` `text {expr} text` `` with at least one hole (no holes is just an
    /// [`ExprKind::String`]).
    Interp(Vec<InterpPart>),
    Name(Name),
    /// A parenthesised expression. It stays a node because `(f())` truncates
    /// multiple results to one.
    Group(Box<Expr>),
    Unary {
        op: UnOp,
        op_span: Span,
        operand: Box<Expr>,
    },
    Binary {
        op: BinOp,
        op_span: Span,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Luau's `if a then b elseif c then d else e` expression.
    IfElse {
        branches: Vec<(Expr, Expr)>,
        else_expr: Box<Expr>,
    },
    /// `f(a, b)` or `f "str"`.
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// `...` inside a vararg function.
    Vararg,
    /// `function(...) ... end`
    Function(Rc<FunctionBody>),
    /// `t[k]`, or `t.name` (`key` is a string literal then).
    Index {
        object: Box<Expr>,
        key: Box<Expr>,
    },
    /// `obj:name(args)`, calls `obj.name` with `obj` as the first argument.
    MethodCall {
        object: Box<Expr>,
        method: Rc<str>,
        method_span: Span,
        args: Vec<Expr>,
    },
    /// `{ a, b, name = c, [k] = d }`
    Table(Vec<TableItem>),
    /// `expr :: Type`, a type assertion. At runtime it's just `expr`.
    Cast {
        expr: Box<Expr>,
        ty: Box<TypeExpr>,
    },
}

// ─ Types ─────────────────────────────────────────────────────────────────────

/// A type written in the source.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeExpr {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    /// `number`, `Map<string, T>`, `nil`, a generic parameter.
    Name {
        name: Rc<str>,
        args: Vec<TypeExpr>,
    },
    /// `"on"`, `true`
    Singleton(Singleton),
    /// `typeof(expr)`
    Typeof(Box<Expr>),
    /// `T?`
    Optional(Box<TypeExpr>),
    Union(Vec<TypeExpr>),
    Intersection(Vec<TypeExpr>),
    /// `{ x: number, [string]: boolean }`, or `{T}` (an indexer from number).
    Table {
        props: Vec<TableProp>,
        indexer: Option<Box<(TypeExpr, TypeExpr)>>,
    },
    /// `T...`, a generic type pack, wherever a variadic type can go (`...:
    /// T...`, `(A, T...) -> T...`).
    GenericPack(Rc<str>),
    /// An explicit type pack passed to a type alias: `(A, B)`, `()`, `(...T)`.
    Pack(TypePack),
    /// `<T>(a: T, ...number) -> (T, string)`
    Function {
        generics: Vec<GenericParam>,
        params: Vec<FnParam>,
        variadic: Option<Box<TypeExpr>>,
        returns: TypePack,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Singleton {
    Bool(bool),
    String(Rc<str>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TableProp {
    pub name: Rc<str>,
    pub ty: TypeExpr,
    pub span: Span,
    /// `read name: T` / `write name: T` (both if neither).
    pub access: Access,
}

/// What a table type's property allows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Access {
    #[default]
    ReadWrite,
    /// `read x: T`: readable, can't be assigned.
    Read,
    /// `write x: T`: assignable, can't be read.
    Write,
}

impl Access {
    pub fn can_read(self) -> bool {
        self != Access::Write
    }

    pub fn can_write(self) -> bool {
        self != Access::Read
    }

    /// `read ` / `write ` / nothing, as written before a property.
    pub fn prefix(self) -> &'static str {
        match self {
            Access::ReadWrite => "",
            Access::Read => "read ",
            Access::Write => "write ",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnParam {
    /// Parameter names in function types are just documentation.
    pub name: Option<Rc<str>>,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenericParam {
    pub name: Rc<str>,
    pub span: Span,
    /// `T...`: a generic type pack (any number of types).
    pub pack: bool,
    /// `T = string` (type aliases only). For a pack it's a [`TypeKind::Pack`]
    /// or [`TypeKind::GenericPack`].
    pub default: Option<TypeExpr>,
}

/// A list of types: function results, `(A, B)`, `...T`, `()`.
#[derive(Clone, Debug, PartialEq)]
pub struct TypePack {
    pub types: Vec<TypeExpr>,
    pub variadic: Option<Box<TypeExpr>>,
    pub span: Span,
}

impl fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            TypeKind::Name { name, args } => {
                f.write_str(name)?;
                if !args.is_empty() {
                    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                    write!(f, "<{}>", args.join(", "))?;
                }
                Ok(())
            }
            TypeKind::Singleton(Singleton::Bool(b)) => write!(f, "{b}"),
            TypeKind::Singleton(Singleton::String(s)) => write!(f, "{s:?}"),
            TypeKind::Typeof(e) => write!(f, "typeof({e})"),
            TypeKind::Optional(t) => write!(f, "{t}?"),
            TypeKind::GenericPack(name) => write!(f, "{name}..."),
            TypeKind::Pack(p) => write!(f, "{p}"),
            TypeKind::Union(ts) | TypeKind::Intersection(ts) => {
                let sep = if matches!(self.kind, TypeKind::Union(_)) {
                    " | "
                } else {
                    " & "
                };
                let parts: Vec<String> = ts.iter().map(|t| t.to_string()).collect();
                write!(f, "({})", parts.join(sep))
            }
            TypeKind::Table { props, indexer } => {
                let mut parts: Vec<String> = props
                    .iter()
                    .map(|p| format!("{}{}: {}", p.access.prefix(), p.name, p.ty))
                    .collect();
                if let Some(ix) = indexer {
                    parts.push(format!("[{}]: {}", ix.0, ix.1));
                }
                write!(f, "{{{}}}", parts.join(", "))
            }
            TypeKind::Function {
                generics,
                params,
                variadic,
                returns,
            } => {
                if !generics.is_empty() {
                    let g: Vec<String> = generics
                        .iter()
                        .map(|g| format!("{}{}", g.name, if g.pack { "..." } else { "" }))
                        .collect();
                    write!(f, "<{}>", g.join(", "))?;
                }
                let mut ps: Vec<String> = params.iter().map(|p| p.ty.to_string()).collect();
                if let Some(v) = variadic {
                    ps.push(variadic_text(v));
                }
                write!(f, "({}) -> {returns}", ps.join(", "))
            }
        }
    }
}

impl fmt::Display for TypePack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = self.types.iter().map(|t| t.to_string()).collect();
        if let Some(v) = &self.variadic {
            parts.push(variadic_text(v));
        }
        write!(f, "({})", parts.join(", "))
    }
}

/// A variadic slot: `...T`, or a generic pack `T...` as-is.
fn variadic_text(v: &TypeExpr) -> String {
    match &v.kind {
        TypeKind::GenericPack(_) => v.to_string(),
        _ => format!("...{v}"),
    }
}

/// One entry of a table constructor.
#[derive(Clone, Debug, PartialEq)]
pub enum TableItem {
    /// `expr`, stored at the next array index (1, 2, ...). A trailing call or
    /// `...` expands to all its values.
    Positional(Expr),
    /// `name = expr` or `[key] = expr`.
    Keyed { key: Expr, value: Expr },
}

impl ExprKind {
    /// Calls and `...` can give any number of values, everything else gives
    /// exactly one.
    pub fn is_multi(&self) -> bool {
        matches!(
            self,
            ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::Vararg
        )
    }
}

/// A piece of an interpolated string.
#[derive(Clone, Debug, PartialEq)]
pub enum InterpPart {
    Text(Rc<str>),
    /// A hole's expression, its value goes through `tostring`.
    Expr(Expr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    Len,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    IDiv,
    Mod,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// Binding power for every prefix operator. Higher than `* / // %` but lower
/// than `^`, so `-2^2 == -(2^2)` while `-2*3 == (-2)*3`.
pub const UNARY_BP: u8 = 30;

impl BinOp {
    /// `(left, right)` binding powers, Pratt-style. Left-associative operators
    /// have `right = left + 1`, right-associative ones (`..`, `^`) have
    /// `right < left`.
    ///
    /// Luau's precedence, lowest to highest:
    /// `or` < `and` < comparison < `..` < `+ -` < `* / // %` < unary < `^`.
    pub fn binding_power(self) -> (u8, u8) {
        use BinOp::*;
        match self {
            Or => (1, 2),
            And => (3, 4),
            Eq | Ne | Lt | Le | Gt | Ge => (5, 6),
            Concat => (9, 8),
            Add | Sub => (10, 11),
            Mul | Div | IDiv | Mod => (20, 21),
            Pow => (41, 40),
        }
    }

    pub fn symbol(self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            IDiv => "//",
            Mod => "%",
            Pow => "^",
            Concat => "..",
            Eq => "==",
            Ne => "~=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            And => "and",
            Or => "or",
        }
    }
}

impl UnOp {
    pub fn symbol(self) -> &'static str {
        match self {
            UnOp::Neg => "-",
            UnOp::Not => "not",
            UnOp::Len => "#",
        }
    }
}

/// S-expression printing, the tests lean on this a lot: `1 + 2 * 3` → `(+ 1 (*
/// 2 3))`.
impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ExprKind::Nil => f.write_str("nil"),
            ExprKind::Bool(b) => write!(f, "{b}"),
            ExprKind::Number(n) => write!(f, "{n}"),
            ExprKind::String(s) => write!(f, "{s:?}"),
            ExprKind::Name(n) => f.write_str(&n.text),
            ExprKind::Group(e) => write!(f, "(group {e})"),
            ExprKind::Unary { op, operand, .. } => write!(f, "({} {operand})", op.symbol()),
            ExprKind::Binary { op, lhs, rhs, .. } => write!(f, "({} {lhs} {rhs})", op.symbol()),
            ExprKind::IfElse {
                branches,
                else_expr,
            } => {
                f.write_str("(if")?;
                for (c, v) in branches {
                    write!(f, " {c} {v}")?;
                }
                write!(f, " {else_expr})")
            }
            ExprKind::Call { callee, args } => {
                write!(f, "(call {callee}")?;
                for a in args {
                    write!(f, " {a}")?;
                }
                f.write_str(")")
            }
            ExprKind::Vararg => f.write_str("..."),
            ExprKind::Interp(parts) => {
                f.write_str("(interp")?;
                for part in parts {
                    match part {
                        InterpPart::Text(t) => write!(f, " {t:?}")?,
                        InterpPart::Expr(e) => write!(f, " {e}")?,
                    }
                }
                f.write_str(")")
            }
            ExprKind::Function(func) => {
                let mut params: Vec<&str> = func.params.iter().map(|p| &*p.text).collect();
                if func.is_vararg {
                    params.push("...");
                }
                write!(f, "(function ({}))", params.join(" "))
            }
            ExprKind::Index { object, key } => write!(f, "(index {object} {key})"),
            ExprKind::Cast { expr, ty } => write!(f, "(:: {expr} {ty})"),
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                write!(f, "(method {object} {method}")?;
                for a in args {
                    write!(f, " {a}")?;
                }
                f.write_str(")")
            }
            ExprKind::Table(items) => {
                f.write_str("(table")?;
                for item in items {
                    match item {
                        TableItem::Positional(e) => write!(f, " {e}")?,
                        TableItem::Keyed { key, value } => write!(f, " [{key}]={value}")?,
                    }
                }
                f.write_str(")")
            }
        }
    }
}

// ─ Definitions Files ─────────────────────────────────────────────────────────

/// A `.d.rhue` file, what a host has, in Luau's declaration syntax (see
/// [`parse_definitions`](crate::parse_definitions)).
#[derive(Clone, Debug, PartialEq)]
pub struct Definitions {
    pub items: Vec<Declaration>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Declaration {
    /// `type Name<T> = ...`, which scripts can use too.
    Alias(Rc<TypeAlias>),
    /// `declare class Name extends Parent ... end`
    Class(ClassDecl),
    /// `declare name: T`, or `declare function name(...): R` (with `ty` the
    /// function's type). A function's name can be `Table.field`, which adds
    /// a field to global table `Table`.
    Global {
        name: Rc<str>,
        span: Span,
        ty: TypeExpr,
        /// `@class(a)` before a `declare function`: parameter `a` names a
        /// class.
        class_param: Option<(Rc<str>, Span)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClassDecl {
    pub name: Rc<str>,
    pub span: Span,
    pub parent: Option<(Rc<str>, Span)>,
    pub members: Vec<ClassItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClassItem {
    /// `Name: T`, or `read Name: T` for one scripts can't assign.
    Prop {
        name: Rc<str>,
        span: Span,
        ty: TypeExpr,
        access: Access,
    },
    /// `function Name(self, a: T): R`. `ty` is the function's type with the
    /// object as its first parameter.
    Method {
        name: Rc<str>,
        span: Span,
        ty: TypeExpr,
        /// `@class(a)` before it: parameter `a` names a class.
        class_param: Option<(Rc<str>, Span)>,
    },
    /// `[string]: T`, what reading a name that isn't a member gives.
    Index {
        span: Span,
        key: TypeExpr,
        value: TypeExpr,
    },
}
