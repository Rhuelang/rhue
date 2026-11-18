//! The checker: one pass over the AST, statement by statement.
//!
//! **Modes.** In `--!strict`, locals and function results get the types of
//! what initialises them, reading an unknown global is an error, and using a
//! value that may be `nil` (indexing, calling, arithmetic) is an error until
//! it's refined. In `--!nonstrict`, anything unannotated is `any`, so only
//! annotated code and literals are checked.
//!
//! **Refinements.** Conditions narrow locals inside the branches they guard
//! (`if x then`, `x ~= nil`, `type(x) == "string"`, `not`, `and`, `or`), and
//! an `if` whose body always exits narrows the rest of the block
//! (`if not x then return end`). Narrowings are kept in layers; assigning to
//! a local drops them, loops drop those for locals the loop body assigns, and
//! a nested function never trusts its enclosing function's narrowings
//! (something else may change the variable before it runs).

use crate::{HashMap, HashSet};
use std::rc::Rc;

use rhue_common::{Diagnostic, Span};
use rhue_compiler::resolve::{Binding, LocalId, Resolution};
use rhue_parser::ast::*;

use crate::env::Env;
use crate::types::*;

/// Stop collecting after this many errors.
pub const MAX_TYPE_ERRORS: usize = 50;

pub(crate) struct FuncCtx {
    /// The annotated result types, if any.
    pub declared: Option<Pack>,
    /// Inferred from `return` statements when nothing is declared.
    pub returns: Vec<Pack>,
    pub vararg: Type,
}

/// What a narrowing applies to: a local, or a field path from one
/// (`p.next.value` is `(p, ["next", "value"])`).
pub(crate) type RefKey = (LocalId, Rc<[Rc<str>]>);

pub(crate) fn root(id: LocalId) -> RefKey {
    (id, Rc::from(Vec::new()))
}

/// Narrowed types, valid in the region of code that pushed them.
#[derive(Clone)]
pub(crate) struct Layer {
    /// `funcs.len()` when pushed: layers from an enclosing function are
    /// ignored inside nested ones.
    pub depth: usize,
    pub map: HashMap<RefKey, Type>,
}

#[derive(Default)]
pub(crate) struct Refine {
    pub truthy: Vec<(RefKey, Type)>,
    pub falsy: Vec<(RefKey, Type)>,
}

pub(crate) struct Checker<'a> {
    pub env: &'a Env,
    pub res: Option<&'a Resolution>,
    pub strict: bool,
    pub errors: Vec<Diagnostic>,
    pub locals: HashMap<LocalId, Type>,
    pub layers: Vec<Layer>,
    pub alias_scopes: Vec<HashMap<Rc<str>, Rc<AliasDef>>>,
    pub generic_scopes: Vec<HashMap<Rc<str>, Type>>,
    /// While expanding an alias: how much of each scope stack it can see.
    pub alias_view: Option<usize>,
    pub generic_view: Option<usize>,
    pub funcs: Vec<FuncCtx>,
    /// Globals this chunk assigns, with their types once known.
    pub chunk_globals: HashMap<Rc<str>, Type>,
    /// Modules `require`d by name whose types the environment doesn't
    /// have yet.
    pub requires: Vec<Rc<str>>,
    /// Globals assigned anywhere in the chunk (they exist, even if the
    /// assignment comes later in the source).
    pub assigned_globals: HashSet<Rc<str>>,
    /// Function declarations seen in the pre-pass, replaced when checked.
    pub provisional: HashSet<Rc<str>>,
    pub expanding: Vec<*const NamedType>,
    /// What instances of each class (by the local holding it) look like,
    /// from the functions on it that return one: `self` in its methods.
    pub instances: HashMap<LocalId, Rc<TableType>>,
    /// Instance fields added by assignment rather than by a constructor:
    /// assigning one something new widens it instead of being an error.
    pub added_fields: HashSet<(LocalId, Rc<str>)>,
    /// When set, every expression's and declared name's type, by span
    /// (for editor tooling).
    pub types: Option<Vec<(Span, Type)>>,
    /// When set, every name that refers to a local (for editor tooling:
    /// definitions and references).
    pub refs: Option<std::cell::RefCell<Vec<(Span, LocalId)>>>,
    /// Locals declared without a value or annotation (strict mode): their
    /// type follows what's assigned (see [`FlowLocal`]).
    pub flow: HashMap<LocalId, FlowLocal>,
    /// Flow locals assigned in a loop being checked: inside it they read as
    /// `any` (a later iteration may have assigned anything).
    pub loop_any: Vec<LocalId>,
    /// One log per `if` statement and block being checked: each key the
    /// code inside narrowed or forgot, with what it was before. Joins merge
    /// only these keys (merging every live narrowing at every `if` made
    /// long functions quadratic).
    touched: Vec<HashMap<RefKey, Option<Type>>>,
    /// Checking a loop body silently, to learn what it assigns to flow
    /// locals (see `check_loop`).
    prepass: bool,
}

/// Whether a `break` in `body` (not in a loop nested in it) leaves it.
fn breaks_out(body: &Block) -> bool {
    body.stmts.iter().any(|s| match &s.kind {
        StmtKind::Break => true,
        StmtKind::If {
            branches,
            else_block,
        } => {
            branches.iter().any(|(_, b)| breaks_out(b))
                || else_block.as_ref().is_some_and(breaks_out)
        }
        StmtKind::Do(b) => breaks_out(b),
        _ => false,
    })
}

/// What a branch of an `if` changed (each key's type at its end), and what
/// its condition, with the earlier ones false, said going in.
type BranchEnd = (HashMap<RefKey, Option<Type>>, Vec<(RefKey, Type)>);

/// A local declared without a value or annotation, in strict mode. It
/// starts as `nil`; each assignment adds the value's type to its declared
/// type (the union of everything assigned, and `nil`) and narrows it to the
/// value, so code that follows the assignments sees what they put there.
#[derive(Clone, Debug)]
pub(crate) struct FlowLocal {
    /// `funcs.len()` where it's declared: a closure reading it reads `any`
    /// (it may run after assignments it can't see).
    pub depth: usize,
    /// The declaration, whose recorded type (for hovers) is the union.
    pub span: Span,
}

/// What checking a chunk found.
pub struct CheckOutput {
    pub errors: Vec<Diagnostic>,
    /// The globals the chunk assigns and their types (`any` when unknown),
    /// for checking later chunks run in the same engine.
    pub globals: Vec<(Rc<str>, Type)>,
    /// What the chunk returns (its first value; `true` if it returns
    /// nothing, as `require` sees it): a module's type.
    pub returns: Type,
    /// Modules it `require`s whose types aren't known yet.
    pub requires: Vec<Rc<str>>,
    /// `globals` and `returns` as type syntax with the aliases they need,
    /// nothing unrolled (what a module exports; see [`Env::import`]).
    pub export: crate::Exported,
}

/// What editor tooling wants from a check (see [`analyze_chunk`]).
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    /// The type of every expression and declared name the checker saw, by
    /// span (innermost spans are the most specific).
    pub types: Vec<(Span, Type)>,
    /// For the expression ending at the requested offset: its members, by
    /// name (what `obj.` / `obj:` can complete to).
    pub members: Option<Vec<(Rc<str>, Type)>>,
    /// Every name that refers to a local, with the local's id (see
    /// `Resolution::locals` for where it's declared).
    pub refs: Vec<(Span, LocalId)>,
}

/// Type-checks a resolved chunk.
pub fn check_chunk(chunk: &Chunk, res: &Resolution, mode: TypeMode, env: &Env) -> CheckOutput {
    let mut c = Checker::new(env, Some(res), mode == TypeMode::Strict);
    check_inner(&mut c, chunk, res, mode, env)
}

/// Like [`check_chunk`], and also records every expression's type, and
/// (with `members_at`) the members of the expression that ends at that
/// byte offset. Types are always inferred as `--!strict` would (so tooling
/// has them even in unchecked chunks); errors are reported as `mode` asks.
pub fn analyze_chunk(
    chunk: &Chunk,
    res: &Resolution,
    mode: TypeMode,
    env: &Env,
    members_at: Option<u32>,
) -> (CheckOutput, Analysis) {
    let mut c = Checker::new(env, Some(res), true);
    c.types = Some(Vec::new());
    c.refs = Some(std::cell::RefCell::new(Vec::new()));
    let strict = check_inner(&mut c, chunk, res, TypeMode::Strict, env);
    let out = if mode == TypeMode::Strict {
        strict
    } else {
        check_chunk(chunk, res, mode, env)
    };
    let types = c.types.take().unwrap_or_default();
    let members = members_at.map(|at| {
        let target = types
            .iter()
            .filter(|(s, _)| s.end == at)
            .max_by_key(|(s, _)| s.end - s.start)
            .map(|(_, t)| t.clone());
        target.map(|t| c.members_of(&t)).unwrap_or_default()
    });
    let mut refs = c.refs.take().map(|r| r.into_inner()).unwrap_or_default();
    refs.sort_by_key(|(s, _)| (s.start, s.end));
    refs.dedup();
    (
        out,
        Analysis {
            types,
            members,
            refs,
        },
    )
}

fn check_inner(
    c: &mut Checker<'_>,
    chunk: &Chunk,
    res: &Resolution,
    mode: TypeMode,
    env: &Env,
) -> CheckOutput {
    let mut assigned = HashSet::default();
    walk_block(&chunk.body, &mut |s| {
        collect_global_assignments(s, res, &mut assigned)
    });
    if mode == TypeMode::NoCheck {
        return CheckOutput {
            errors: Vec::new(),
            globals: assigned
                .into_iter()
                .filter(|g| !env.is_host_global(g))
                .map(|g| (g, Type::Any))
                .collect(),
            returns: Type::Any,
            requires: Vec::new(),
            export: Default::default(),
        };
    }
    c.strict = mode == TypeMode::Strict;
    c.assigned_globals = assigned;
    c.prototype_global_functions(&chunk.body);
    c.funcs.push(FuncCtx {
        declared: None,
        returns: Vec::new(),
        vararg: Type::Any,
    });
    // The chunk's own scope stays open until its types are exported (they
    // may name its aliases).
    c.alias_scopes.push(HashMap::default());
    c.generic_scopes.push(HashMap::default());
    c.layers.push(Layer {
        depth: c.funcs.len(),
        map: HashMap::default(),
    });
    c.block_body(&chunk.body);
    let mut raw_globals: Vec<(Rc<str>, Type)> = Vec::new();
    for name in c.assigned_globals.clone() {
        if env.is_host_global(&name) {
            continue;
        }
        let t = c.chunk_globals.get(&name).cloned().unwrap_or(Type::Any);
        raw_globals.push((name, t));
    }
    raw_globals.sort_by(|a, b| a.0.cmp(&b.0));
    let main = c
        .funcs
        .first()
        .map(|f| f.returns.clone())
        .unwrap_or_default();
    let raw_returns = if main.is_empty() {
        Type::BoolLit(true)
    } else {
        let firsts: Vec<Type> = main.iter().map(|p| p.get(0)).collect();
        let t = union(firsts);
        if t.is_any() || matches!(t, Type::Nil) {
            t
        } else if t.can_be_nil() {
            // A module that may return nil gives `true` instead.
            union([t.strip_nil(), Type::BoolLit(true)])
        } else {
            t
        }
    };
    let raw_returns = if matches!(raw_returns, Type::Nil) {
        Type::BoolLit(true)
    } else {
        raw_returns
    };
    let errors = c.errors.len();
    let export = {
        let mut x = crate::export::Exporter::new(c);
        let returns = x.write(&raw_returns);
        let globals = raw_globals
            .iter()
            .map(|(n, t)| (n.to_string(), x.write(t)))
            .collect();
        let mut aliases = x.finish();
        aliases.reverse();
        crate::Exported {
            aliases,
            returns,
            globals,
        }
    };
    let globals = raw_globals
        .iter()
        .map(|(n, t)| (n.clone(), c.detach(t, 3)))
        .collect();
    let returns = if raw_returns.is_any() {
        raw_returns
    } else {
        c.detach(&raw_returns, 4)
    };
    // Exporting only reads what checking found.
    c.errors.truncate(errors);
    c.layers.pop();
    c.generic_scopes.pop();
    c.alias_scopes.pop();
    CheckOutput {
        errors: std::mem::take(&mut c.errors),
        globals,
        returns,
        requires: std::mem::take(&mut c.requires),
        export,
    }
}

fn collect_global_assignments(s: &Stmt, res: &Resolution, out: &mut HashSet<Rc<str>>) {
    let mut add = |e: &Expr| {
        if let ExprKind::Name(n) = &e.kind
            && res.binding(n) == Binding::Global
        {
            out.insert(n.text.clone());
        }
    };
    match &s.kind {
        StmtKind::Assign { targets, .. } => targets.iter().for_each(&mut add),
        StmtKind::CompoundAssign { target, .. } | StmtKind::Function { target, .. } => add(target),
        _ => {}
    }
}

impl<'a> Checker<'a> {
    pub fn new(env: &'a Env, res: Option<&'a Resolution>, strict: bool) -> Self {
        Checker {
            env,
            res,
            strict,
            errors: Vec::new(),
            locals: HashMap::default(),
            layers: Vec::new(),
            alias_scopes: vec![HashMap::default()],
            generic_scopes: Vec::new(),
            alias_view: None,
            generic_view: None,
            funcs: Vec::new(),
            chunk_globals: HashMap::default(),
            requires: Vec::new(),
            assigned_globals: HashSet::default(),
            provisional: HashSet::default(),
            expanding: Vec::new(),
            instances: HashMap::default(),
            added_fields: HashSet::default(),
            types: None,
            refs: None,
            flow: HashMap::default(),
            loop_any: Vec::new(),
            touched: Vec::new(),
            prepass: false,
        }
    }

    /// A checker that only resolves host declarations.
    pub fn for_declarations(env: &'a Env) -> Self {
        Checker::new(env, None, true)
    }

    pub fn resolve_declaration(mut self, t: &TypeExpr) -> Result<Type, Vec<Diagnostic>> {
        let ty = self.resolve_type(t);
        if self.errors.is_empty() {
            Ok(ty)
        } else {
            Err(self.errors)
        }
    }

    // --- errors ------------------------------------------------------------

    pub fn error(&mut self, span: Span, message: impl Into<String>) {
        self.error_labeled(span, message, "type error here");
    }

    pub fn error_labeled(
        &mut self,
        span: Span,
        message: impl Into<String>,
        label: impl Into<String>,
    ) {
        if self.errors.len() < MAX_TYPE_ERRORS {
            self.errors
                .push(Diagnostic::error(message, span).with_label(label));
        }
    }

    pub fn mismatch(&mut self, span: Span, got: &Type, want: &Type) {
        let why = self.explain(got, want);
        self.error_labeled(
            span,
            format!("Type '{got}' could not be converted into '{want}'{why}"),
            format!("this is '{got}'"),
        );
    }

    /// For tables, which field is the problem. Fields of simple types are
    /// blamed before nested tables (in a recursive type, the nested field
    /// fails only because of a simple one).
    fn explain(&mut self, got: &Type, want: &Type) -> String {
        let (Type::Table(g), Type::Table(w)) = (self.expand(got), self.expand(want)) else {
            return String::new();
        };
        for simple_only in [true, false] {
            for (name, wt) in &w.props {
                let nested = |t: &Type| {
                    t.members()
                        .iter()
                        .any(|m| matches!(m, Type::Table(_) | Type::Named(_)))
                };
                if simple_only && nested(wt) {
                    continue;
                }
                let (ga, wa) = (g.access(name), w.access(name));
                if g.prop(name).is_some() && (wa.can_read() && !ga.can_read()) {
                    return format!(": field '{name}' is write-only");
                }
                if g.prop(name).is_some() && (wa.can_write() && !ga.can_write()) {
                    return format!(": field '{name}' is read-only");
                }
                match g.prop(name) {
                    None if !self.sub(&Type::Nil, wt) && g.indexer.is_none() => {
                        return format!(": missing field '{name}'");
                    }
                    Some(gt) if !self.sub(gt, wt) => {
                        return format!(": field '{name}' is '{gt}', expected '{wt}'");
                    }
                    _ => {}
                }
            }
        }
        String::new()
    }

    /// Checks `got` against `want`, reporting a mismatch at `span`.
    pub fn expect(&mut self, span: Span, got: &Type, want: &Type) -> bool {
        if self.sub(got, want) {
            true
        } else {
            self.mismatch(span, got, want);
            false
        }
    }

    // --- locals and narrowing ----------------------------------------------------

    pub fn local_id(&self, name: &Name) -> Option<LocalId> {
        match self.res?.binding(name) {
            Binding::Local(id) => {
                if let Some(refs) = &self.refs {
                    refs.borrow_mut().push((name.span, id));
                }
                Some(id)
            }
            Binding::Global => None,
        }
    }

    /// A local's current (possibly narrowed) type.
    /// The name local `id` was declared with.
    pub fn local_name(&self, id: LocalId) -> Option<Rc<str>> {
        Some(self.res?.locals.get(id as usize)?.name.clone())
    }

    pub fn local_type(&self, id: LocalId) -> Type {
        if let Some(f) = self.flow.get(&id) {
            if self.funcs.len() > f.depth {
                return Type::Any;
            }
            if let Some(t) = self.narrowed(&root(id)) {
                return t;
            }
            if self.loop_any.contains(&id) {
                return Type::Any;
            }
        }
        self.narrowed(&root(id))
            .unwrap_or_else(|| self.locals.get(&id).cloned().unwrap_or(Type::Any))
    }

    /// An assignment of `value` to flow local `id` (see [`FlowLocal`]).
    fn assign_flow(&mut self, id: LocalId, value: &Type) {
        let Some(f) = self.flow.get(&id).cloned() else {
            return;
        };
        let value = value.widen();
        let escapes = self.funcs.len() > f.depth
            || value.is_any()
            || matches!(self.expand(&value), Type::Table(_));
        let declared = if escapes {
            // A closure assigning it (other code can't tell when), or a
            // table (whose type grows with its fields): back to `any`.
            self.flow.remove(&id);
            Type::Any
        } else {
            let so_far = self.locals.get(&id).cloned().unwrap_or(Type::Nil);
            union([so_far, value.clone()])
        };
        self.locals.insert(id, declared.clone());
        if let Some(types) = &mut self.types
            && let Some(entry) = types.iter_mut().find(|(s, _)| *s == f.span)
        {
            entry.1 = declared;
        }
        self.invalidate(id);
        if !escapes {
            self.narrow(root(id), value);
        }
    }

    /// The narrowed type of a local or field path, if any.
    pub fn narrowed(&self, key: &RefKey) -> Option<Type> {
        let depth = self.funcs.len();
        for layer in self.layers.iter().rev() {
            if layer.depth != depth {
                break;
            }
            if let Some(t) = layer.map.get(key) {
                return Some(t.clone());
            }
        }
        None
    }

    pub fn declare(&mut self, name: &Name, ty: Type) {
        self.record(name.span, &ty);
        if let Some(id) = self.local_id(name) {
            self.locals.insert(id, ty);
            self.invalidate(id);
        }
    }

    /// Remembers a span's type, if tooling asked for types.
    pub fn record(&mut self, span: Span, ty: &Type) {
        if let Some(types) = &mut self.types {
            types.push((span, ty.clone()));
        }
    }

    /// Narrows a local or path for the rest of the current layer.
    pub fn narrow(&mut self, key: RefKey, ty: Type) {
        let depth = self.funcs.len();
        if self.layers.last().is_some_and(|top| top.depth == depth) {
            self.touch(&key);
            if let Some(top) = self.layers.last_mut() {
                top.map.insert(key, ty);
            }
        }
    }

    /// Notes in the open touch logs that `key` is about to change.
    fn touch(&mut self, key: &RefKey) {
        if self.touched.iter().all(|log| log.contains_key(key)) {
            return;
        }
        let before = self.narrowed(key);
        for log in &mut self.touched {
            log.entry(key.clone()).or_insert_with(|| before.clone());
        }
    }

    /// Forgets every narrowing of `id` and its fields (it was assigned).
    pub fn invalidate(&mut self, id: LocalId) {
        self.forget(|k| k.0 == id);
    }

    /// Forgets narrowings of `key` and of paths through it (it was
    /// assigned).
    fn invalidate_path(&mut self, key: &RefKey) {
        self.forget(|k| {
            k.0 == key.0 && k.1.len() >= key.1.len() && k.1[..key.1.len()] == key.1[..]
        });
    }

    fn forget(&mut self, gone: impl Fn(&RefKey) -> bool) {
        if !self.touched.is_empty() {
            let depth = self.funcs.len();
            let keys: Vec<RefKey> = self
                .layers
                .iter()
                .rev()
                .take_while(|l| l.depth == depth)
                .flat_map(|l| l.map.keys().filter(|k| gone(k)).cloned())
                .collect();
            for k in keys {
                self.touch(&k);
            }
        }
        for layer in &mut self.layers {
            layer.map.retain(|k, _| !gone(k));
        }
    }

    /// Whether narrowing to a value of type `t` is safe: tables are
    /// mutable, so knowing what one held when assigned says little later.
    fn narrowable(&mut self, t: &Type) -> bool {
        !t.is_any() && !matches!(self.expand(t), Type::Table(_))
    }

    /// The local and field path an expression names, if it's that simple.
    pub fn path_of(&self, e: &Expr) -> Option<RefKey> {
        match &e.kind {
            ExprKind::Name(n) => self.local_id(n).map(root),
            ExprKind::Group(inner) => self.path_of(inner),
            ExprKind::Index { object, key } => {
                let ExprKind::String(field) = &key.kind else {
                    return None;
                };
                let (id, path) = self.path_of(object)?;
                let mut path = path.to_vec();
                path.push(field.clone());
                Some((id, path.into()))
            }
            _ => None,
        }
    }

    pub fn with_layer<R>(&mut self, refs: &[(RefKey, Type)], f: impl FnOnce(&mut Self) -> R) -> R {
        self.layers.push(Layer {
            depth: self.funcs.len(),
            map: refs.iter().cloned().collect(),
        });
        let r = f(self);
        self.layers.pop();
        r
    }

    /// Drops narrowings of locals that `body` assigns: a loop runs it again
    /// after those assignments.
    /// The locals a loop body assigns: what a later iteration may have
    /// changed by the time an earlier statement runs again.
    fn assigned_in(&self, body: &Block) -> Vec<LocalId> {
        let Some(res) = self.res else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        walk_block(body, &mut |s| {
            let mut add = |e: &Expr| {
                if let ExprKind::Name(n) = &e.kind
                    && let Binding::Local(id) = res.binding(n)
                {
                    ids.push(id);
                }
            };
            match &s.kind {
                StmtKind::Assign { targets, .. } => targets.iter().for_each(&mut add),
                StmtKind::CompoundAssign { target, .. } => add(target),
                _ => {}
            }
        });
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Checks a loop whose body is `body`, with `run` checking one
    /// iteration (condition, variables, body).
    ///
    /// Narrowings of what the body assigns don't hold at the top of the
    /// loop. A flow local it assigns is first typed by checking the body
    /// silently (diagnostics, recorded types and narrowings rolled back),
    /// as often as that keeps adding types to it (at most three times), so
    /// the real pass sees everything an iteration can leave in it. If it
    /// doesn't settle, or this is already a silent pass, it's `any` inside
    /// the loop instead.
    fn check_loop(&mut self, body: &Block, run: &dyn Fn(&mut Self)) {
        let ids = self.assigned_in(body);
        for &id in &ids {
            self.invalidate(id);
        }
        let flow: Vec<LocalId> = ids
            .into_iter()
            .filter(|id| self.flow.contains_key(id))
            .collect();
        let mut fallback = !flow.is_empty() && self.prepass;
        if !flow.is_empty() && !self.prepass {
            let union_of = |c: &Self| -> Vec<String> {
                flow.iter()
                    .map(|id| c.locals.get(id).map_or(String::new(), |t| t.to_string()))
                    .collect()
            };
            fallback = true;
            for _ in 0..3 {
                let before = union_of(self);
                let errors = self.errors.len();
                let types = self.types.as_ref().map(Vec::len);
                let refs = self.refs.as_ref().map(|r| r.borrow().len());
                let layers = self.layers.clone();
                let touched = self.touched.clone();
                self.prepass = true;
                run(self);
                self.prepass = false;
                self.errors.truncate(errors);
                if let (Some(t), Some(n)) = (&mut self.types, types) {
                    t.truncate(n);
                }
                if let (Some(r), Some(n)) = (&self.refs, refs) {
                    r.borrow_mut().truncate(n);
                }
                self.layers = layers;
                self.touched = touched;
                for &id in &flow {
                    self.invalidate(id);
                }
                if union_of(self) == before {
                    fallback = false;
                    break;
                }
            }
        }
        let n = if fallback {
            let n = flow.len();
            self.loop_any.extend(flow);
            n
        } else {
            0
        };
        run(self);
        let keep = self.loop_any.len() - n;
        self.loop_any.truncate(keep);
    }

    /// Like [`block`](Self::block), returning what it changed: for each key
    /// it narrowed or forgot, its type at the end (`None`: nothing known
    /// beyond the declared type).
    pub fn block_flow(&mut self, block: &Block) -> HashMap<RefKey, Option<Type>> {
        self.alias_scopes.push(HashMap::default());
        self.generic_scopes.push(HashMap::default());
        self.layers.push(Layer {
            depth: self.funcs.len(),
            map: HashMap::default(),
        });
        self.touched.push(HashMap::default());
        self.block_body(block);
        let log = self.touched.pop().expect("pushed above");
        let end = log.into_keys().map(|k| {
            let t = self.narrowed(&k);
            (k, t)
        });
        let end = end.collect();
        self.layers.pop();
        self.generic_scopes.pop();
        self.alias_scopes.pop();
        end
    }

    // --- globals -------------------------------------------------------------------

    /// Gives global functions declared anywhere in the chunk their annotated
    /// signature up front, so they can be called before their declaration
    /// is reached (mutual recursion).
    fn prototype_global_functions(&mut self, body: &Block) {
        let Some(res) = self.res else { return };
        let mut found = Vec::new();
        walk_block(body, &mut |s| {
            if let StmtKind::Function { target, func } = &s.kind
                && let ExprKind::Name(n) = &target.kind
                && res.binding(n) == Binding::Global
            {
                found.push((n.text.clone(), func.clone()));
            }
        });
        // (Host globals keep their declared types: these are checked
        // against them instead.)
        for (name, func) in found {
            if self.env.is_host_global(&name) {
                continue;
            }
            let sig = self.signature(&func);
            self.chunk_globals.insert(name.clone(), Type::function(sig));
            self.provisional.insert(name);
        }
    }

    pub fn global_type(&mut self, name: &Name) -> Type {
        if let Some(t) = self.chunk_globals.get(&name.text) {
            return t.clone();
        }
        if let Some(t) = self.env.global(&name.text) {
            return t.clone();
        }
        if self.assigned_globals.contains(&name.text) {
            return Type::Any;
        }
        if self.strict {
            self.error(name.span, format!("Unknown global '{}'", name.text));
            if let Some(best) = self.suggest_global(&name.text, name.span)
                && let Some(d) = self
                    .errors
                    .last_mut()
                    .filter(|d| d.primary.span == name.span)
            {
                d.help = Some(format!("did you mean '{best}'?"));
                d.fixes.push(rhue_common::Fix {
                    title: format!("Change to '{best}'"),
                    edits: vec![(name.span, best)],
                });
            }
            return Type::Error;
        }
        Type::Any
    }

    fn assign_global(&mut self, name: &Name, value: &Type, span: Span) {
        if self.env.is_host_global(&name.text) {
            if let Some(t) = self.env.global(&name.text).cloned() {
                self.expect(span, value, &t);
            }
            return;
        }
        if self.provisional.remove(&name.text) {
            self.chunk_globals.insert(name.text.clone(), value.clone());
            return;
        }
        // This chunk's own type for it, else what an earlier chunk gave it
        // (a new `function` declaration may change that; see `provisional`).
        let known = self
            .chunk_globals
            .get(&name.text)
            .or_else(|| self.env.global(&name.text))
            .cloned();
        match known {
            Some(t) => {
                self.expect(span, value, &t);
            }
            None => {
                let t = if self.strict {
                    value.widen()
                } else {
                    Type::Any
                };
                self.chunk_globals.insert(name.text.clone(), t);
            }
        }
    }

    // --- blocks and statements -------------------------------------------------------

    pub fn block(&mut self, block: &Block) {
        self.alias_scopes.push(HashMap::default());
        self.generic_scopes.push(HashMap::default());
        self.layers.push(Layer {
            depth: self.funcs.len(),
            map: HashMap::default(),
        });
        self.block_body(block);
        self.layers.pop();
        self.generic_scopes.pop();
        self.alias_scopes.pop();
    }

    /// A block's statements, in the current scope. Type aliases are visible
    /// throughout their block (so they can refer to each other).
    fn block_body(&mut self, block: &Block) {
        for stmt in &block.stmts {
            if let StmtKind::TypeAlias(alias) = &stmt.kind {
                self.declare_alias(alias);
            }
        }
        for stmt in &block.stmts {
            stacker::maybe_grow(64 * 1024, 1024 * 1024, || self.stmt(stmt));
        }
        if let Some(ret) = &block.ret {
            self.return_stmt(ret);
        }
    }

    fn declare_alias(&mut self, alias: &TypeAlias) {
        let scope = self.alias_scopes.last_mut().expect("a scope is open");
        if scope.contains_key(&alias.name) {
            let span = alias.name_span;
            self.error(span, format!("Redefinition of type '{}'", alias.name));
            return;
        }
        let depth = self.alias_scopes.len();
        let def = Rc::new(AliasDef {
            name: alias.name.clone(),
            generics: alias.generics.clone(),
            body: alias.ty.clone(),
            scope_depth: depth,
            generic_depth: self.generic_scopes.len(),
            closed: None,
        });
        self.alias_scopes
            .last_mut()
            .expect("a scope is open")
            .insert(alias.name.clone(), def);
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Local {
                names,
                types,
                values,
            } => self.local_stmt(names, types, values),
            StmtKind::Assign { targets, values }
                if targets.len() == 1
                    && values.len() == 1
                    && self.class_index(&targets[0], &values[0]) => {}
            StmtKind::Assign { targets, values } => {
                let hints: Vec<Option<Type>> =
                    targets.iter().map(|t| self.target_hint(t)).collect();
                let pack = self.exprs_pack(values, &hints);
                for (i, t) in targets.iter().enumerate() {
                    let span = values.get(i).or(values.last()).map_or(t.span, |v| v.span);
                    self.assign_to(t, &pack.get(i), span);
                }
            }
            StmtKind::CompoundAssign {
                op,
                op_span,
                target,
                value,
            } => {
                let lhs = self.expr(target);
                let rhs = self.expr(value);
                let result = self.binary_type(*op, &lhs, &rhs, target.span, value.span, *op_span);
                self.assign_to(target, &result, stmt.span);
            }
            StmtKind::Call(call) => {
                self.expr_pack(call);
                self.assert_narrowing(call);
            }
            StmtKind::Function { target, func } => self.function_stmt(target, func),
            StmtKind::LocalFunction { name, func } => {
                // Its signature is visible inside its own body (recursion).
                let sig = self.signature(func);
                self.declare(name, Type::function(sig));
                let f = self.function(func, None, None);
                self.declare(name, Type::function(f));
            }
            StmtKind::Do(body) => {
                // What the block assigned is still true after it.
                for (k, t) in self.block_flow(body) {
                    match t {
                        Some(t) => self.narrow(k, t),
                        None => self.forget(|j| *j == k),
                    }
                }
            }
            StmtKind::While { cond, body } => {
                self.check_loop(body, &|c| {
                    let r = c.condition(cond);
                    c.with_layer(&r.truthy, |c| c.block(body));
                });
            }
            StmtKind::Repeat { body, cond } => {
                // The body runs at least once and the loop ends when the
                // condition holds: unless something breaks out early, what
                // the last iteration left (and the condition) is known after.
                let after: std::cell::RefCell<Vec<(RefKey, Option<Type>)>> = Default::default();
                self.check_loop(body, &|c| {
                    // The condition sees the body's locals: one scope for both.
                    c.alias_scopes.push(HashMap::default());
                    c.generic_scopes.push(HashMap::default());
                    c.with_layer(&[], |c| {
                        c.touched.push(HashMap::default());
                        c.block_body(body);
                        let r = c.condition(cond);
                        let log = c.touched.pop().expect("pushed above");
                        let mut end: Vec<(RefKey, Option<Type>)> = log
                            .into_keys()
                            .map(|k| (k.clone(), c.narrowed(&k)))
                            .collect();
                        end.extend(r.truthy.into_iter().map(|(k, t)| (k, Some(t))));
                        *after.borrow_mut() = end;
                    });
                    c.generic_scopes.pop();
                    c.alias_scopes.pop();
                });
                if !breaks_out(body) {
                    // (Narrowing a local declared in the body is harmless:
                    // nothing can name it any more.)
                    for (k, t) in after.into_inner() {
                        if let Some(t) = t {
                            self.narrow(k, t);
                        }
                    }
                }
            }
            StmtKind::If {
                branches,
                else_block,
            } => self.if_stmt(branches, else_block.as_ref()),
            StmtKind::NumericFor {
                var,
                var_type,
                start,
                limit,
                step,
                body,
            } => {
                for e in [Some(start), Some(limit), step.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    let t = self.expr(e);
                    self.expect(e.span, &t, &Type::Number);
                }
                let ty = match var_type {
                    Some(te) => {
                        let t = self.resolve_type(te);
                        self.expect(te.span, &Type::Number, &t);
                        t
                    }
                    None => Type::Number,
                };
                self.check_loop(body, &|c| {
                    c.with_layer(&[], |c| {
                        c.declare(var, ty.clone());
                        c.block(body);
                    });
                });
            }
            StmtKind::GenericFor {
                names,
                types,
                exprs,
                body,
            } => {
                let span = exprs[0].span.to(exprs.last().unwrap().span);
                let vars = self.iteration_types(exprs, names.len(), span);
                self.check_loop(body, &|c| {
                    c.with_layer(&[], |c| {
                        for (i, n) in names.iter().enumerate() {
                            let got = vars.get(i).cloned().unwrap_or(Type::Nil);
                            let ty = match &types[i] {
                                Some(te) => {
                                    let t = c.resolve_type(te);
                                    c.expect(te.span, &got, &t);
                                    t
                                }
                                None if c.strict => got,
                                None => Type::Any,
                            };
                            c.declare(n, ty);
                        }
                        c.block(body);
                    });
                });
            }
            StmtKind::TypeAlias(alias) => self.check_alias(alias),
            StmtKind::Break | StmtKind::Continue => {}
        }
    }

    fn local_stmt(&mut self, names: &[Name], types: &[Option<TypeExpr>], values: &[Expr]) {
        let declared: Vec<Option<Type>> = types
            .iter()
            .map(|t| t.as_ref().map(|te| self.resolve_type(te)))
            .collect();
        let pack = self.exprs_pack(values, &declared);
        for (i, name) in names.iter().enumerate() {
            let has_value = !values.is_empty() && (i < pack.types.len() || pack.variadic.is_some());
            let value = pack.get(i);
            let span = values
                .get(i)
                .or(values.last())
                .map_or(name.span, |v| v.span);
            let ty = match &declared[i] {
                Some(d) => {
                    let ok = !has_value || self.expect(span, &value, d);
                    self.declare(name, d.clone());
                    // `local x: number? = 5` is known to be a number for now.
                    if ok
                        && has_value
                        && !d.is_any()
                        && self.narrowable(&value)
                        && let Some(id) = self.local_id(name)
                    {
                        self.narrow(root(id), value);
                    }
                    continue;
                }
                // `local x` (strict): typed by what's assigned to it.
                None if self.strict && values.is_empty() => {
                    self.declare(name, Type::Nil);
                    if let Some(id) = self.local_id(name) {
                        self.flow.insert(
                            id,
                            FlowLocal {
                                depth: self.funcs.len(),
                                span: name.span,
                            },
                        );
                    }
                    continue;
                }
                None if !self.strict || !has_value => Type::Any,
                // `local x = nil` is the gradual "fill it in later": `any`.
                None if matches!(values.get(i).map(|v| &v.kind), Some(ExprKind::Nil)) => Type::Any,
                None => value.widen(),
            };
            self.declare(name, ty);
        }
    }

    /// The type a value assigned to `target` must have, as a hint for
    /// literals.
    fn target_hint(&mut self, target: &Expr) -> Option<Type> {
        match &target.kind {
            ExprKind::Name(n) => match self.local_id(n) {
                Some(id) if self.flow.contains_key(&id) => None,
                Some(id) => self.locals.get(&id).cloned(),
                None => self
                    .chunk_globals
                    .get(&n.text)
                    .or_else(|| self.env.global(&n.text))
                    .cloned(),
            },
            _ => None,
        }
    }

    pub fn assign_to(&mut self, target: &Expr, value: &Type, span: Span) {
        match &target.kind {
            ExprKind::Name(n) => match self.local_id(n) {
                Some(id) if self.flow.contains_key(&id) => self.assign_flow(id, value),
                Some(id) => {
                    let declared = self.locals.get(&id).cloned().unwrap_or(Type::Any);
                    let ok = self.expect(span, value, &declared);
                    self.invalidate(id);
                    if ok && !declared.is_any() && self.narrowable(value) {
                        self.narrow(root(id), value.clone());
                    }
                }
                None => self.assign_global(n, value, span),
            },
            ExprKind::Index { object, key } => {
                let ot = self.expr(object);
                match &key.kind {
                    ExprKind::String(s) => {
                        let from = self.errors.len();
                        self.assign_prop(object, &ot, s, value, span);
                        self.suggest_member(from, &ot, s, key.span);
                        self.suggest_assert(from, span, object.span);
                    }
                    _ => {
                        let kt = self.expr(key);
                        self.assign_indexed(object, &ot, &kt, value, span);
                    }
                }
                if let Some(path) = self.path_of(target) {
                    self.invalidate_path(&path);
                    if self.narrowable(value) {
                        self.narrow(path, value.clone());
                    }
                }
            }
            _ => {}
        }
    }

    fn function_stmt(&mut self, target: &Expr, func: &Rc<FunctionBody>) {
        match &target.kind {
            ExprKind::Name(n) => {
                let hint = match self.target_hint(target) {
                    Some(Type::Function(f)) if !self.provisional.contains(&n.text) => Some(f),
                    _ => None,
                };
                let f = self.function(func, hint.as_deref(), None);
                self.assign_to(target, &Type::function(f), func.span);
            }
            ExprKind::Index { object, key } => {
                let ot = self.expr(object);
                let ExprKind::String(name) = &key.kind else {
                    return;
                };
                // A field declared with a function type types the definition.
                let hint = match self.expand(&ot) {
                    Type::Table(t) => match t.prop(name) {
                        Some(Type::Function(f)) => Some(f.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let class = match &object.kind {
                    ExprKind::Name(n) if self.is_class(&ot) => {
                        self.local_id(n).map(|id| (id, n.text.clone()))
                    }
                    _ => None,
                };
                // In a class's methods `self` is an instance. In a plain
                // table's, it's often an object built elsewhere with the
                // method copied in (`{ items = {}, add = T.add }`), so it's
                // the table's type but open: other fields read as `any`.
                let self_ty = func.is_method.then(|| match &class {
                    Some((id, name)) => self.instance_type(*id, name),
                    None => match self.expand(&ot) {
                        Type::Table(t) => Type::table(TableType {
                            open: true,
                            ..(*t).clone()
                        }),
                        _ => ot.clone(),
                    },
                });
                let f = self.function(func, hint.as_deref(), self_ty);
                if let Some((id, _)) = class {
                    self.note_instance(id, &f.rets.get(0));
                }
                self.assign_prop(object, &ot, name, &Type::function(f), func.span);
            }
            _ => {}
        }
    }

    fn if_stmt(&mut self, branches: &[(Expr, Block)], else_block: Option<&Block>) {
        // Every key a branch changes, with what it was before the `if`.
        self.touched.push(HashMap::default());
        // Each condition is checked knowing the earlier ones were false.
        let mut falsy_layers = 0;
        let mut first_falsy = Vec::new();
        // For each branch that carries on past the `if`: what it changed,
        // and what its condition (and the earlier ones' being false) said.
        let mut ends: Vec<BranchEnd> = Vec::new();
        let mut falsy_so_far: Vec<(RefKey, Type)> = Vec::new();
        for (i, (cond, body)) in branches.iter().enumerate() {
            let r = self.condition(cond);
            let end = self.with_layer(&r.truthy, |c| c.block_flow(body));
            if !always_exits(body, true) {
                let mut known = falsy_so_far.clone();
                known.extend(r.truthy.iter().cloned());
                ends.push((end, known));
            }
            if i == 0 {
                first_falsy = r.falsy.clone();
            }
            falsy_so_far.extend(r.falsy.iter().cloned());
            self.layers.push(Layer {
                depth: self.funcs.len(),
                map: r.falsy.into_iter().collect(),
            });
            falsy_layers += 1;
        }
        // The `else` (written or not) runs with every condition false.
        let (else_end, else_exits) = match else_block {
            Some(b) => (self.block_flow(b), always_exits(b, true)),
            None => (HashMap::default(), false),
        };
        let log = self.touched.pop().expect("pushed above");
        // What an untouched key is in the `else`: the layers say (the
        // falsy ones are still in place).
        let else_view: HashMap<RefKey, Option<Type>> = log
            .keys()
            .map(|k| {
                (
                    k.clone(),
                    else_end.get(k).cloned().unwrap_or_else(|| self.narrowed(k)),
                )
            })
            .collect();
        for _ in 0..falsy_layers {
            self.layers.pop();
        }
        // Where the branches join, a key some branch changed is the union of
        // what each left it as.
        let reaching = ends.len() + usize::from(!else_exits);
        if reaching > 0 {
            let mut keys: Vec<&RefKey> = log.keys().collect();
            keys.sort();
            for key in keys {
                let declared = key
                    .1
                    .is_empty()
                    .then(|| self.locals.get(&key.0).cloned())
                    .flatten();
                let before = log[key].clone().or_else(|| declared.clone());
                let mut parts = Vec::new();
                for (end, known) in &ends {
                    let t = match end.get(key) {
                        Some(t) => t.clone().or_else(|| declared.clone()),
                        None => known
                            .iter()
                            .rev()
                            .find(|(k, _)| k == key)
                            .map(|(_, t)| t.clone())
                            .or_else(|| before.clone()),
                    };
                    parts.push(t);
                }
                if !else_exits {
                    parts.push(else_view[key].clone().or_else(|| declared.clone()));
                }
                let Some(parts) = parts.into_iter().collect::<Option<Vec<Type>>>() else {
                    continue;
                };
                let merged = union(parts);
                let declared_any = key.1.is_empty() && declared.as_ref().is_none_or(|d| d.is_any());
                if declared_any || merged.is_any() {
                    continue;
                }
                if self.flow.contains_key(&key.0) || self.narrowable(&merged) {
                    self.narrow(key.clone(), merged);
                }
            }
        }
        // `if not x then return end`: past this point, `x` is known.
        if branches.len() == 1 && else_block.is_none() && always_exits(&branches[0].1, true) {
            for (key, t) in first_falsy {
                self.narrow(key, t);
            }
        }
    }

    fn return_stmt(&mut self, ret: &Return) {
        let declared = self.funcs.last().and_then(|f| f.declared.clone());
        let hints: Vec<Option<Type>> = match &declared {
            Some(p) => (0..ret.values.len()).map(|i| Some(p.get(i))).collect(),
            None => Vec::new(),
        };
        let pack = self.exprs_pack(&ret.values, &hints);
        match declared {
            Some(want) => {
                if !self.pack_sub(&pack, &want) {
                    self.error(
                        ret.span,
                        format!("Type pack '{pack}' could not be converted into '{want}'"),
                    );
                }
            }
            // (The main chunk's returns are its type as a module.)
            None => {
                if let Some(f) = self.funcs.last_mut() {
                    f.returns.push(pack);
                }
            }
        }
    }

    fn check_alias(&mut self, alias: &TypeAlias) {
        let Some(def) = self
            .alias_scopes
            .last()
            .and_then(|s| s.get(&alias.name))
            .cloned()
        else {
            return;
        };
        // Expanding it once reports unknown names and direct recursion.
        let mut args = Vec::new();
        let mut packs = Vec::new();
        for g in &def.generics {
            let var = Type::Generic(GenericVar::new(g.name.clone(), g.pack));
            if g.pack {
                packs.push(Pack {
                    types: Vec::new(),
                    variadic: Some(var),
                });
            } else {
                args.push(var);
            }
        }
        // And each default once, as a use of the alias would.
        self.check_alias_defaults(&def);
        let named = Type::Named(Rc::new(NamedType {
            alias: def,
            args,
            packs,
            expanded: Default::default(),
        }));
        self.expand(&named);
    }

    /// `assert(x)` makes `x` truthy for the rest of the block.
    fn assert_narrowing(&mut self, call: &Expr) {
        let ExprKind::Call { callee, args } = &call.kind else {
            return;
        };
        if let (ExprKind::Name(n), Some(first)) = (&callee.kind, args.first())
            && n.text.as_ref() == "assert"
            && self.local_id(n).is_none()
        {
            let r = self.condition_quiet(first);
            for (key, t) in r.truthy {
                self.narrow(key, t);
            }
        }
    }

    /// Exports a type for later chunks: aliases from this chunk are expanded
    /// (a few levels deep; deeper is `any`), since their scope is gone.
    pub fn detach(&mut self, t: &Type, depth: u32) -> Type {
        match t {
            Type::Named(_) if depth == 0 => Type::Any,
            Type::Named(_) => {
                let e = self.expand(t);
                self.detach(&e, depth - 1)
            }
            Type::Function(f) => Type::function(FnType {
                generics: f.generics.clone(),
                params: f.params.iter().map(|p| self.detach(p, depth)).collect(),
                variadic: f.variadic.as_ref().map(|v| self.detach(v, depth)),
                rets: Pack {
                    types: f.rets.types.iter().map(|r| self.detach(r, depth)).collect(),
                    variadic: f.rets.variadic.as_ref().map(|v| self.detach(v, depth)),
                },
            }),
            Type::Table(tt) => {
                // A metatable held by a local becomes a snapshot of that
                // local's type, so methods keep working in other chunks.
                let meta = tt.meta.as_ref().map(|m| {
                    if depth == 0 {
                        return Meta::Type(Type::Any);
                    }
                    let mt = match m {
                        Meta::Local { id, .. } => self.locals.get(id).cloned().unwrap_or(Type::Any),
                        Meta::Type(t) => t.clone(),
                    };
                    Meta::Type(self.detach(&mt, depth - 1))
                });
                Type::table(TableType {
                    props: tt
                        .props
                        .iter()
                        .map(|(n, p)| (n.clone(), self.detach(p, depth)))
                        .collect(),
                    indexer: tt
                        .indexer
                        .as_ref()
                        .map(|(k, v)| (self.detach(k, depth), self.detach(v, depth))),
                    unsealed: false,
                    meta,
                    index_self: tt.index_self,
                    open: tt.open,
                    access: tt.access.clone(),
                })
            }
            Type::Union(ts) => union(ts.iter().map(|m| self.detach(m, depth)).collect::<Vec<_>>()),
            Type::Intersection(ts) => {
                intersection(ts.iter().map(|m| self.detach(m, depth)).collect::<Vec<_>>())
            }
            t => t.clone(),
        }
    }
}

/// Whether running `block` never reaches its end: it returns, raises an
/// error, or (when `loops` is set) breaks or continues.
pub(crate) fn always_exits(block: &Block, loops: bool) -> bool {
    if block.ret.is_some() {
        return true;
    }
    match block.stmts.last().map(|s| &s.kind) {
        Some(StmtKind::If {
            branches,
            else_block: Some(e),
        }) => branches.iter().all(|(_, b)| always_exits(b, loops)) && always_exits(e, loops),
        Some(StmtKind::Do(b)) => always_exits(b, loops),
        Some(StmtKind::Break | StmtKind::Continue) => loops,
        Some(StmtKind::Call(e)) => matches!(&e.kind, ExprKind::Call { callee, .. }
            if matches!(&callee.kind, ExprKind::Name(n) if n.text.as_ref() == "error")),
        _ => false,
    }
}

// --- a statement walker (into nested blocks and function bodies) ------------------

pub(crate) fn walk_block(b: &Block, f: &mut dyn FnMut(&Stmt)) {
    for s in &b.stmts {
        walk_stmt(s, f);
    }
    if let Some(r) = &b.ret {
        r.values.iter().for_each(|e| walk_expr(e, f));
    }
}

fn walk_stmt(s: &Stmt, f: &mut dyn FnMut(&Stmt)) {
    f(s);
    match &s.kind {
        StmtKind::Local { values, .. } => values.iter().for_each(|e| walk_expr(e, f)),
        StmtKind::Assign { targets, values } => {
            targets.iter().chain(values).for_each(|e| walk_expr(e, f))
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target, f);
            walk_expr(value, f);
        }
        StmtKind::Do(b) => walk_block(b, f),
        StmtKind::While { cond, body } | StmtKind::Repeat { body, cond } => {
            walk_expr(cond, f);
            walk_block(body, f);
        }
        StmtKind::If {
            branches,
            else_block,
        } => {
            for (c, b) in branches {
                walk_expr(c, f);
                walk_block(b, f);
            }
            if let Some(b) = else_block {
                walk_block(b, f);
            }
        }
        StmtKind::NumericFor {
            start,
            limit,
            step,
            body,
            ..
        } => {
            walk_expr(start, f);
            walk_expr(limit, f);
            if let Some(s) = step {
                walk_expr(s, f);
            }
            walk_block(body, f);
        }
        StmtKind::GenericFor { exprs, body, .. } => {
            exprs.iter().for_each(|e| walk_expr(e, f));
            walk_block(body, f);
        }
        StmtKind::Call(e) => walk_expr(e, f),
        StmtKind::Function { target, func } => {
            walk_expr(target, f);
            walk_block(&func.body, f);
        }
        StmtKind::LocalFunction { func, .. } => walk_block(&func.body, f),
        StmtKind::TypeAlias(_) | StmtKind::Break | StmtKind::Continue => {}
    }
}

fn walk_expr(e: &Expr, f: &mut dyn FnMut(&Stmt)) {
    match &e.kind {
        ExprKind::Function(func) => walk_block(&func.body, f),
        ExprKind::Interp(parts) => {
            for part in parts {
                if let InterpPart::Expr(x) = part {
                    walk_expr(x, f);
                }
            }
        }
        ExprKind::Group(x)
        | ExprKind::Unary { operand: x, .. }
        | ExprKind::Cast { expr: x, .. } => walk_expr(x, f),
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        ExprKind::IfElse {
            branches,
            else_expr,
        } => {
            for (c, v) in branches {
                walk_expr(c, f);
                walk_expr(v, f);
            }
            walk_expr(else_expr, f);
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, f);
            args.iter().for_each(|a| walk_expr(a, f));
        }
        ExprKind::MethodCall { object, args, .. } => {
            walk_expr(object, f);
            args.iter().for_each(|a| walk_expr(a, f));
        }
        ExprKind::Index { object, key } => {
            walk_expr(object, f);
            walk_expr(key, f);
        }
        ExprKind::Table(items) => {
            for item in items {
                match item {
                    TableItem::Positional(v) => walk_expr(v, f),
                    TableItem::Keyed { key, value } => {
                        walk_expr(key, f);
                        walk_expr(value, f);
                    }
                }
            }
        }
        ExprKind::Nil
        | ExprKind::Bool(_)
        | ExprKind::Number(_)
        | ExprKind::String(_)
        | ExprKind::Name(_)
        | ExprKind::Vararg => {}
    }
}
