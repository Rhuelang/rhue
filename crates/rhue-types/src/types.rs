//! The type representation.

use std::cell::OnceCell;
use std::fmt;
use std::rc::Rc;

pub use rhue_parser::ast::Access;
use std::sync::atomic::{AtomicU32, Ordering};

use rhue_parser::ast::TypeExpr;

/// A type. Cheap to clone: compound types are behind `Rc`.
#[derive(Clone, Debug)]
pub enum Type {
    /// Opts out of checking: compatible with everything, both ways.
    Any,
    /// Every value is an `unknown`, but nothing can be done with one until
    /// it's refined (`type(x) == "number"`).
    Unknown,
    /// No value (the result of `error(...)`).
    Never,
    /// The type of something that already produced an error. Behaves like
    /// `any` so one mistake doesn't cascade.
    Error,
    Nil,
    Boolean,
    Number,
    String,
    /// `true` / `false` as types.
    BoolLit(bool),
    /// `"on"` as a type.
    StrLit(Rc<str>),
    Function(Rc<FnType>),
    Table(Rc<TableType>),
    /// A host (userdata) type, by name. Its members live in the [`Env`](crate::Env).
    Class(Rc<str>),
    /// Normalised: flat, no duplicates, at least two members.
    Union(Rc<[Type]>),
    /// Used for overloaded functions.
    Intersection(Rc<[Type]>),
    /// A generic parameter (`T` inside `function f<T>(x: T)`).
    Generic(Rc<GenericVar>),
    /// A reference to a type alias, expanded on demand (aliases can be
    /// recursive: `type List = { value: number, next: List? }`).
    Named(Rc<NamedType>),
}

#[derive(Debug)]
pub struct GenericVar {
    pub name: Rc<str>,
    pub id: u32,
    /// A generic type pack (`T...`): stands for any number of types, and
    /// only ever appears as a variadic (a function's `...` or a result
    /// pack's tail).
    pub pack: bool,
}

static NEXT_GENERIC: AtomicU32 = AtomicU32::new(0);

impl GenericVar {
    pub fn fresh(name: Rc<str>) -> Rc<Self> {
        Self::new(name, false)
    }

    pub fn new(name: Rc<str>, pack: bool) -> Rc<Self> {
        Rc::new(GenericVar {
            name,
            id: NEXT_GENERIC.fetch_add(1, Ordering::Relaxed),
            pack,
        })
    }

    /// `T` or `T...`.
    pub fn display(&self) -> String {
        if self.pack {
            format!("{}...", self.name)
        } else {
            self.name.to_string()
        }
    }
}

/// A type alias as declared: its body is resolved when a use is expanded.
#[derive(Debug)]
pub struct AliasDef {
    pub name: Rc<str>,
    /// Its parameters as written (types, then packs; maybe with defaults).
    pub generics: Vec<rhue_parser::ast::GenericParam>,
    pub body: TypeExpr,
    /// How many alias scopes were open where it was declared.
    pub scope_depth: usize,
    /// How many generic scopes were open there (aliases inside a generic
    /// function can mention its parameters).
    pub generic_depth: usize,
    /// For an alias imported with a module's types (see
    /// [`Env::import`](crate::Env::import)): the aliases its body can name,
    /// which are all it can name (besides host types). Expanding it in any
    /// checker resolves the body there instead of in the checker's scopes.
    pub closed: Option<std::rc::Weak<ClosedScope>>,
}

/// The aliases imported together with one module's (or chunk's) types,
/// by name.
pub type ClosedScope = std::cell::RefCell<crate::HashMap<Rc<str>, Rc<AliasDef>>>;

#[derive(Debug)]
pub struct NamedType {
    pub alias: Rc<AliasDef>,
    /// One per type parameter (defaults filled in).
    pub args: Vec<Type>,
    /// One per pack parameter.
    pub packs: Vec<Pack>,
    pub expanded: OnceCell<Type>,
}

#[derive(Clone, Debug, Default)]
pub struct Pack {
    pub types: Vec<Type>,
    /// `...T`: any number of further values.
    pub variadic: Option<Type>,
}

impl Pack {
    pub fn one(t: Type) -> Self {
        Pack {
            types: vec![t],
            variadic: None,
        }
    }

    pub fn empty() -> Self {
        Pack::default()
    }

    /// Any number of `any`s.
    pub fn any() -> Self {
        Pack {
            types: Vec::new(),
            variadic: Some(Type::Any),
        }
    }

    /// The type of value `i`: `nil` past the end of a fixed pack. A
    /// value from a generic pack (`T...`) could be anything, nil included.
    pub fn get(&self, i: usize) -> Type {
        match self.types.get(i) {
            Some(t) => t.clone(),
            None => match &self.variadic {
                Some(v) if v.pack_var().is_some() => Type::Unknown,
                Some(v) => v.clone(),
                None => Type::Nil,
            },
        }
    }

    /// The first value, as a single expression uses it.
    pub fn first(&self) -> Type {
        self.get(0)
    }
}

#[derive(Clone, Debug)]
pub struct FnType {
    pub generics: Vec<Rc<GenericVar>>,
    pub params: Vec<Type>,
    pub variadic: Option<Type>,
    pub rets: Pack,
}

impl FnType {
    /// `(...any) -> ...any`
    pub fn any() -> Self {
        FnType {
            generics: Vec::new(),
            params: Vec::new(),
            variadic: Some(Type::Any),
            rets: Pack::any(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TableType {
    /// In declaration order.
    pub props: Vec<(Rc<str>, Type)>,
    pub indexer: Option<(Type, Type)>,
    /// Built from a table literal: assigning a new field to the local that
    /// holds it adds the field.
    pub unsealed: bool,
    /// Set by `setmetatable`: fields this table lacks are looked up through
    /// the metatable's `__index`, and operators use its metamethods.
    pub meta: Option<Meta>,
    /// `T.__index = T` (the usual class): this table is its own `__index`.
    pub index_self: bool,
    /// May have fields it doesn't list, which read and write as `any`:
    /// `self` inside a class's methods (an instance whose fields beyond
    /// the constructor's aren't known).
    pub open: bool,
    /// Properties that are only readable or only writable (`read x: T`,
    /// `write y: T`); every other property is both.
    pub access: Vec<(Rc<str>, Access)>,
}

/// A table's metatable, as the checker knows it.
#[derive(Clone, Debug)]
pub enum Meta {
    /// The table held by a local, looked up when needed: a class table
    /// keeps gaining methods after instances are made with it.
    Local { id: u32, name: Rc<str> },
    /// Any other metatable, by its type at the `setmetatable` call.
    Type(Type),
}

impl fmt::Display for Meta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Meta::Local { name, .. } => f.write_str(name),
            Meta::Type(t) => write!(f, "{t}"),
        }
    }
}

impl TableType {
    /// What property `name` allows.
    pub fn access(&self, name: &str) -> Access {
        self.access
            .iter()
            .find(|(n, _)| &**n == name)
            .map_or(Access::ReadWrite, |(_, a)| *a)
    }

    pub fn prop(&self, name: &str) -> Option<&Type> {
        self.props
            .iter()
            .find(|(n, _)| &**n == name)
            .map(|(_, t)| t)
    }

    /// `{}` from a literal: nothing known yet, so it fits any table type.
    pub fn is_empty_literal(&self) -> bool {
        self.unsealed && self.props.is_empty() && self.indexer.is_none()
    }
}

impl Type {
    pub fn function(f: FnType) -> Type {
        Type::Function(Rc::new(f))
    }

    pub fn table(t: TableType) -> Type {
        Type::Table(Rc::new(t))
    }

    /// `{T}`
    pub fn array(elem: Type) -> Type {
        Type::table(TableType {
            props: Vec::new(),
            indexer: Some((Type::Number, elem)),
            ..TableType::default()
        })
    }

    pub fn optional(t: Type) -> Type {
        union([t, Type::Nil])
    }

    /// `any` and the error type switch checking off.
    /// The generic pack this is, if it's one (`T...`).
    pub fn pack_var(&self) -> Option<&Rc<GenericVar>> {
        match self {
            Type::Generic(g) if g.pack => Some(g),
            _ => None,
        }
    }

    pub fn is_any(&self) -> bool {
        matches!(self, Type::Any | Type::Error)
    }

    /// Structural identity, for de-duplicating unions (not subtyping).
    pub fn same(&self, other: &Type) -> bool {
        use Type::*;
        match (self, other) {
            (Any, Any) | (Unknown, Unknown) | (Never, Never) | (Error, Error) => true,
            (Nil, Nil) | (Boolean, Boolean) | (Number, Number) | (String, String) => true,
            (BoolLit(a), BoolLit(b)) => a == b,
            (StrLit(a), StrLit(b)) => a == b,
            (Function(a), Function(b)) => Rc::ptr_eq(a, b),
            (Table(a), Table(b)) => Rc::ptr_eq(a, b),
            (Class(a), Class(b)) => a == b,
            (Generic(a), Generic(b)) => a.id == b.id,
            (Named(a), Named(b)) => {
                Rc::ptr_eq(&a.alias, &b.alias)
                    && a.args.len() == b.args.len()
                    && a.args.iter().zip(&b.args).all(|(x, y)| x.same(y))
            }
            (Union(a), Union(b)) | (Intersection(a), Intersection(b)) => {
                a.len() == b.len() && a.iter().all(|x| b.iter().any(|y| x.same(y)))
            }
            _ => false,
        }
    }

    /// The members of a union, or the type itself.
    pub fn members(&self) -> Vec<Type> {
        match self {
            Type::Union(ts) => ts.to_vec(),
            t => vec![t.clone()],
        }
    }

    pub fn can_be_nil(&self) -> bool {
        match self {
            Type::Nil | Type::Unknown => true,
            Type::Union(ts) => ts.iter().any(|t| t.can_be_nil()),
            _ => false,
        }
    }

    /// The type without `nil`.
    pub fn strip_nil(&self) -> Type {
        match self {
            Type::Union(ts) => union(ts.iter().filter(|t| !matches!(t, Type::Nil)).cloned()),
            Type::Nil => Type::Never,
            t => t.clone(),
        }
    }

    /// What's left when a value of this type is truthy.
    pub fn truthy(&self) -> Type {
        match self {
            Type::Nil | Type::BoolLit(false) => Type::Never,
            Type::Boolean => Type::BoolLit(true),
            Type::Union(ts) => union(ts.iter().map(|t| t.truthy())),
            t => t.clone(),
        }
    }

    /// What's left when a value of this type is falsy (`nil` or `false`).
    pub fn falsy(&self) -> Type {
        match self {
            Type::Nil | Type::BoolLit(false) => self.clone(),
            Type::Boolean => Type::BoolLit(false),
            Type::Any | Type::Error => self.clone(),
            Type::Unknown => union([Type::Nil, Type::BoolLit(false)]),
            Type::Union(ts) => union(ts.iter().map(|t| t.falsy())),
            _ => Type::Never,
        }
    }

    /// The type a variable gets from this initial value: literals widen
    /// (`local s = "a"` is a `string`, not `"a"`).
    pub fn widen(&self) -> Type {
        match self {
            Type::BoolLit(_) => Type::Boolean,
            Type::StrLit(_) => Type::String,
            Type::Union(ts) => union(ts.iter().map(|t| t.widen())),
            t => t.clone(),
        }
    }
}

/// Builds a normalised union.
pub fn union(types: impl IntoIterator<Item = Type>) -> Type {
    let mut out: Vec<Type> = Vec::new();
    let add = |t: Type, out: &mut Vec<Type>| {
        if !out.iter().any(|o| o.same(&t)) {
            out.push(t);
        }
    };
    for t in types {
        match t {
            Type::Union(ts) => ts.iter().for_each(|t| add(t.clone(), &mut out)),
            Type::Never => {}
            t => add(t, &mut out),
        }
    }
    if let Some(e) = out.iter().find(|t| matches!(t, Type::Error | Type::Any)) {
        return e.clone();
    }
    if out.iter().any(|t| matches!(t, Type::Unknown)) {
        return Type::Unknown;
    }
    // Literals are absorbed by their base type; `true | false` is `boolean`.
    let has = |out: &[Type], f: fn(&Type) -> bool| out.iter().any(f);
    if has(&out, |t| matches!(t, Type::String)) {
        out.retain(|t| !matches!(t, Type::StrLit(_)));
    }
    if has(&out, |t| matches!(t, Type::BoolLit(true)))
        && has(&out, |t| matches!(t, Type::BoolLit(false)))
    {
        out.retain(|t| !matches!(t, Type::BoolLit(_)));
        out.push(Type::Boolean);
    }
    if has(&out, |t| matches!(t, Type::Boolean)) {
        out.retain(|t| !matches!(t, Type::BoolLit(_)));
    }
    match out.len() {
        0 => Type::Never,
        1 => out.pop().unwrap(),
        _ => Type::Union(out.into()),
    }
}

pub fn intersection(types: impl IntoIterator<Item = Type>) -> Type {
    let mut out: Vec<Type> = Vec::new();
    for t in types {
        match t {
            Type::Intersection(ts) => out.extend(ts.iter().cloned()),
            Type::Unknown => {}
            t if !out.iter().any(|o| o.same(&t)) => out.push(t),
            _ => {}
        }
    }
    match out.len() {
        0 => Type::Unknown,
        1 => out.pop().unwrap(),
        _ => Type::Intersection(out.into()),
    }
}

// --- display, in Luau's notation -------------------------------------------------

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Any => f.write_str("any"),
            Type::Unknown => f.write_str("unknown"),
            Type::Never => f.write_str("never"),
            Type::Error => f.write_str("*error-type*"),
            Type::Nil => f.write_str("nil"),
            Type::Boolean => f.write_str("boolean"),
            Type::Number => f.write_str("number"),
            Type::String => f.write_str("string"),
            Type::BoolLit(b) => write!(f, "{b}"),
            Type::StrLit(s) => write!(f, "\"{s}\""),
            Type::Function(func) => write!(f, "{func}"),
            Type::Table(t) => write!(f, "{t}"),
            Type::Class(name) => f.write_str(name),
            Type::Generic(g) => f.write_str(&g.display()),
            Type::Named(n) => {
                f.write_str(&n.alias.name)?;
                if !n.args.is_empty() || !n.packs.is_empty() {
                    let mut args: Vec<String> = n.args.iter().map(|a| a.to_string()).collect();
                    args.extend(n.packs.iter().map(|p| match (&p.types[..], &p.variadic) {
                        ([], Some(v)) if v.pack_var().is_some() => v.to_string(),
                        _ => {
                            let mut parts: Vec<String> =
                                p.types.iter().map(|t| t.to_string()).collect();
                            if let Some(v) = &p.variadic {
                                parts.push(variadic_text(v));
                            }
                            format!("({})", parts.join(", "))
                        }
                    }));
                    write!(f, "<{}>", args.join(", "))?;
                }
                Ok(())
            }
            Type::Union(ts) => {
                let non_nil: Vec<&Type> = ts.iter().filter(|t| !matches!(t, Type::Nil)).collect();
                if non_nil.len() == 1 && ts.len() == 2 {
                    return write!(f, "{}?", Paren(non_nil[0]));
                }
                let parts: Vec<String> = ts.iter().map(|t| Paren(t).to_string()).collect();
                f.write_str(&parts.join(" | "))
            }
            Type::Intersection(ts) => {
                let parts: Vec<String> = ts.iter().map(|t| Paren(t).to_string()).collect();
                f.write_str(&parts.join(" & "))
            }
        }
    }
}

/// Parenthesises function types (and nested unions) inside `|`, `&`, `?`.
struct Paren<'a>(&'a Type);

impl fmt::Display for Paren<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Type::Function(_) | Type::Union(_) | Type::Intersection(_) => write!(f, "({})", self.0),
            t => write!(f, "{t}"),
        }
    }
}

impl fmt::Display for FnType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.generics.is_empty() {
            let g: Vec<String> = self.generics.iter().map(|g| g.display()).collect();
            write!(f, "<{}>", g.join(", "))?;
        }
        let mut ps: Vec<String> = self.params.iter().map(|p| p.to_string()).collect();
        if let Some(v) = &self.variadic {
            ps.push(variadic_text(v));
        }
        write!(f, "({}) -> ", ps.join(", "))?;
        match (&self.rets.types[..], &self.rets.variadic) {
            ([one], None) => write!(f, "{}", Paren(one)),
            ([], Some(v)) => f.write_str(&variadic_text(v)),
            _ => write!(f, "{}", self.rets),
        }
    }
}

impl fmt::Display for Pack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let ([one], None) = (&self.types[..], &self.variadic) {
            return write!(f, "{one}");
        }
        let mut parts: Vec<String> = self.types.iter().map(|t| t.to_string()).collect();
        if let Some(v) = &self.variadic {
            parts.push(variadic_text(v));
        }
        write!(f, "({})", parts.join(", "))
    }
}

/// A variadic slot as written: `...T`, or a generic pack `T...` as is.
fn variadic_text(v: &Type) -> String {
    match v.pack_var() {
        Some(g) => g.display(),
        None => format!("...{v}"),
    }
}

impl fmt::Display for TableType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_fields(f)?;
        if let Some(m) = &self.meta {
            write!(f, " with metatable {m}")?;
        }
        Ok(())
    }
}

impl TableType {
    fn fmt_fields(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.props.is_empty() && !self.index_self && !self.open {
            match &self.indexer {
                None => return f.write_str("{}"),
                Some((Type::Number, v)) => return write!(f, "{{{v}}}"),
                _ => {}
            }
        }
        let mut parts: Vec<String> = Vec::new();
        if self.index_self {
            parts.push("__index: <self>".to_string());
        }
        parts.extend(
            self.props
                .iter()
                .map(|(n, t)| format!("{}{n}: {t}", self.access(n).prefix())),
        );
        if let Some((k, v)) = &self.indexer {
            parts.push(format!("[{k}]: {v}"));
        }
        if self.open {
            parts.push("...".to_string());
        }
        write!(f, "{{ {} }}", parts.join(", "))
    }
}
