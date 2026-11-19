//! The engine's state that lives with the VM (so natives can reach it): the
//! type environment, compile settings, and modules for `require`.
//!
//! **Modules.** `require("name")` finds a module's source among the ones the
//! host added ([`Engine::add_module`](crate::Engine::add_module)) or asks the
//! host's loader
//! ([`Engine::set_module_loader`](crate::Engine::set_module_loader)), compiles
//! it (type-checking it as its directive says), runs it once with its name as
//! `...`, and caches what it returned (`true` if nothing). A module that
//! requires itself, directly or not, is an error naming the cycle.
//!
//! **Typed modules.** When a checked chunk calls `require` with a literal name,
//! the checker reports the names it has no type for. The engine checks those
//! modules (compiling them, and keeping the result for the later `require`) and
//! checks the chunk again, so `local m = require("m")` gets the module's real
//! type.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use rhue_bytecode::Proto;
use rhue_common::SourceFile;
use rhue_compiler::CompileOptions;
use rhue_parser::ast::{Chunk, TypeMode};
use rhue_types::{CheckOutput, Env, Type};

use crate::{Context, Error, ErrorKind, ScriptError, Value};

/// Finds a module's source by name (`None` if there's no such module).
pub(crate) type Loader = Rc<dyn Fn(&str) -> Option<String>>;

pub(crate) struct HostState {
    pub env: Env,
    pub options: CompileOptions,
    pub default_mode: TypeMode,
    pub sources: HashMap<String, String>,
    pub loader: Option<Loader>,
    /// Modules compiled before their `require` (while checking a chunk that
    /// requires them).
    pub compiled: HashMap<String, Rc<Proto>>,
    /// Modules given as compiled chunks (`add_module_bytes`).
    pub binary: HashMap<String, Rc<Proto>>,
    /// `require`'s cache, module name → value, in a VM table the pin keeps
    /// alive.
    pub loaded: Option<(rhue_vm::TableRef, rhue_vm::Pin)>,
    /// Modules running right now, outermost first (to report cycles).
    pub loading: Vec<String>,
    /// Modules being type-checked for another chunk (a cycle there just makes
    /// the type `any`).
    pub checking: HashSet<String>,
}

impl HostState {
    pub fn new(env: Env) -> Self {
        HostState {
            env,
            options: CompileOptions::default(),
            default_mode: TypeMode::NoCheck,
            sources: HashMap::new(),
            loader: None,
            compiled: HashMap::new(),
            binary: HashMap::new(),
            loaded: None,
            loading: Vec::new(),
            checking: HashSet::new(),
        }
    }

    fn source_of(&self, name: &str) -> Option<String> {
        self.sources
            .get(name)
            .cloned()
            .or_else(|| self.loader.as_ref().and_then(|l| l(name)))
    }
}

pub(crate) fn host(vm: &mut rhue_vm::Vm) -> &mut HostState {
    let slot = vm.host_data();
    if !slot.as_ref().is_some_and(|d| d.is::<HostState>()) {
        *slot = Some(Box::new(HostState::new(Env::new())));
    }
    slot.as_mut()
        .and_then(|d| d.downcast_mut::<HostState>())
        .expect("set above")
}

/// Type-checks a resolved chunk in `mode`, checking any modules it `require`s
/// that don't have a type yet first.
pub(crate) fn check(
    vm: &mut rhue_vm::Vm,
    chunk: &Chunk,
    res: &rhue_compiler::resolve::Resolution,
    mode: TypeMode,
) -> CheckOutput {
    let mut asked: HashSet<Rc<str>> = HashSet::new();
    loop {
        let out = rhue_types::check_chunk(chunk, res, mode, &host(vm).env);
        let new: Vec<Rc<str>> = out
            .requires
            .iter()
            .filter(|n| asked.insert((*n).clone()))
            .cloned()
            .collect();
        if new.is_empty() {
            return out;
        }
        for name in new {
            let ty = module_type(vm, &name);
            host(vm).env.define_module(&name, ty);
        }
    }
}

/// Checks (and compiles) module `name` to learn its type. `any` if it can't be
/// found, doesn't compile, or is already being checked.
fn module_type(vm: &mut rhue_vm::Vm, name: &str) -> Type {
    let state = host(vm);
    if state.checking.contains(name) {
        return Type::Any;
    }
    let Some(source) = state.source_of(name) else {
        return Type::Any;
    };
    state.checking.insert(name.to_string());
    let result = compile(vm, name, &source);
    host(vm).checking.remove(name);
    match result {
        Ok((proto, ty)) => {
            host(vm).compiled.insert(name.to_string(), proto);
            ty
        }
        Err(_) => Type::Any,
    }
}

/// Parses, checks and compiles a chunk, and also returns its type as a module.
pub(crate) fn compile(
    vm: &mut rhue_vm::Vm,
    name: &str,
    source: &str,
) -> Result<(Rc<Proto>, Type), Error> {
    compile_typed(vm, name, source).map(|c| (c.proto, c.returns))
}

/// A compiled chunk and the types it exports.
pub(crate) struct Compiled {
    pub proto: Rc<Proto>,
    /// Its type as a module.
    pub returns: Type,
    /// The globals it defines.
    pub globals: Vec<(Rc<str>, Type)>,
    /// The same as type syntax, with the aliases they need (what gets saved).
    pub export: Option<rhue_types::Exported>,
}

pub(crate) fn compile_typed(
    vm: &mut rhue_vm::Vm,
    name: &str,
    source: &str,
) -> Result<Compiled, Error> {
    let file = SourceFile::new(name, source);
    let chunk =
        rhue_parser::parse_chunk(source).map_err(|d| Error::new(ErrorKind::Syntax, d, &file))?;
    compile_ast(vm, &chunk, &file)
}

/// The types saved with a compiled chunk, as type syntax: exported with aliases
/// when the chunk got checked, simplified otherwise.
pub(crate) fn chunk_types(c: &Compiled) -> rhue_bytecode::ChunkTypes {
    match &c.export {
        Some(x) => rhue_bytecode::ChunkTypes {
            aliases: x.aliases.clone(),
            returns: x.returns.clone(),
            globals: x.globals.clone(),
        },
        None => rhue_bytecode::ChunkTypes {
            aliases: Vec::new(),
            returns: rhue_types::portable(&c.returns),
            globals: c
                .globals
                .iter()
                .map(|(n, t)| (n.to_string(), rhue_types::portable(t)))
                .collect(),
        },
    }
}

/// Declares what a loaded chunk's saved types say (its globals, unless the host
/// declared them) and returns its type as a module. A type that doesn't resolve
/// here (say a host type this engine doesn't have) is `any`.
pub(crate) fn declare_chunk_types(vm: &mut rhue_vm::Vm, types: &rhue_bytecode::ChunkTypes) -> Type {
    let env = &mut host(vm).env;
    let exported = rhue_types::Exported {
        aliases: types.aliases.clone(),
        returns: types.returns.clone(),
        globals: types.globals.clone(),
    };
    if let Some((returns, globals)) = env.import(&exported) {
        for (name, t) in globals {
            env.define_script_global(&name, t);
        }
        return returns;
    }
    // something didn't resolve here, so each type on its own, `any` where that
    // fails too
    for (name, ty) in &types.globals {
        let t = env.parse_type(ty).unwrap_or(Type::Any);
        env.define_script_global(name, t);
    }
    env.parse_type(&types.returns).unwrap_or(Type::Any)
}

pub(crate) fn compile_ast(
    vm: &mut rhue_vm::Vm,
    chunk: &Chunk,
    file: &Rc<SourceFile>,
) -> Result<Compiled, Error> {
    let res = rhue_compiler::resolve::resolve(chunk)
        .map_err(|d| Error::new(ErrorKind::Compile, d, file))?;
    let mode = chunk.mode.map_or(host(vm).default_mode, |(m, _)| m);
    let checked = check(vm, chunk, &res, mode);
    if !checked.errors.is_empty() {
        return Err(Error::new(ErrorKind::Type, checked.errors, file));
    }
    let options = host(vm).options;
    let proto = rhue_compiler::compile_resolved(chunk, &res, file, options)
        .map(Rc::new)
        .map_err(|d| Error::new(ErrorKind::Compile, d, file))?;
    let env = &mut host(vm).env;
    // what the chunk exports, nothing unrolled (recursive types and classes
    // intact), or the detached types if that can't be read back
    let checked_mode = mode != TypeMode::NoCheck;
    let imported = checked_mode.then(|| env.import(&checked.export)).flatten();
    let (returns, globals) = imported.unwrap_or((checked.returns, checked.globals));
    for (name, ty) in &globals {
        env.define_script_global(name, ty.clone());
    }
    Ok(Compiled {
        proto,
        returns,
        globals,
        export: checked_mode.then_some(checked.export),
    })
}

/// What editor tooling learns about a chunk (see
/// [`Engine::analyze`](crate::Engine::analyze)).
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Analysis {
    /// Syntax, name and type errors (types as the chunk's directive, or the
    /// engine's default mode, says).
    pub diagnostics: Vec<rhue_common::Diagnostic>,
    /// The type of each expression and declared name, by span.
    pub types: Vec<(rhue_common::Span, String)>,
    /// What can come after `expr.` / `expr:` for the expression ending at the
    /// offset asked about, with each member's type.
    pub members: Option<Vec<(String, String)>>,
    /// Each name that refers to a local, and where that local is declared.
    pub refs: Vec<(rhue_common::Span, rhue_common::Span)>,
    /// Every local the chunk declares, and where.
    pub locals: Vec<(String, rhue_common::Span)>,
    /// The globals scripts can see, with their types.
    pub globals: Vec<(String, String)>,
    /// Inferred types the source doesn't spell out, for inlay hints.
    pub hints: Vec<crate::InlayHint>,
}

pub(crate) fn analyze(vm: &mut rhue_vm::Vm, source: &str, members_at: Option<u32>) -> Analysis {
    let globals = |vm: &mut rhue_vm::Vm| {
        let mut g: Vec<(String, String)> = host(vm)
            .env
            .globals()
            .map(|(n, t)| (n.to_string(), t.to_string()))
            .collect();
        g.sort();
        g
    };
    let chunk = match rhue_parser::parse_chunk(source) {
        Ok(c) => c,
        Err(d) => {
            return Analysis {
                diagnostics: d,
                globals: globals(vm),
                ..Analysis::default()
            };
        }
    };
    let res = match rhue_compiler::resolve::resolve(&chunk) {
        Ok(r) => r,
        Err(d) => {
            return Analysis {
                diagnostics: d,
                globals: globals(vm),
                ..Analysis::default()
            };
        }
    };
    let mode = chunk.mode.map_or(host(vm).default_mode, |(m, _)| m);
    // learn the types of required modules first
    check(vm, &chunk, &res, mode);
    let (out, an) = rhue_types::analyze_chunk(&chunk, &res, mode, &host(vm).env, members_at);
    let mut globals = globals(vm);
    for (n, t) in &out.globals {
        if !globals.iter().any(|(g, _)| **g == **n) {
            globals.push((n.to_string(), t.to_string()));
        }
    }
    let hints = crate::hints::hints(&chunk.body, source, &an.types);
    Analysis {
        hints,
        diagnostics: out.errors,
        types: an.types.iter().map(|(s, t)| (*s, t.to_string())).collect(),
        members: an.members.map(|m| {
            m.iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect()
        }),
        refs: an
            .refs
            .iter()
            .map(|&(s, id)| (s, res.locals[id as usize].span))
            .collect(),
        locals: res
            .locals
            .iter()
            .map(|l| (l.name.to_string(), l.span))
            .collect(),
        globals,
    }
}

/// The `require` global.
pub(crate) fn require(cx: &mut Context<'_>, name: String) -> Result<Value, ScriptError> {
    let key = cx.vm.string(&name);
    let cache = loaded_table(cx.vm);
    let cached = cx.vm.heap().table(cache).get(key);
    if cached != rhue_vm::Value::Nil {
        return Ok(cx.lift(cached));
    }
    let state = host(cx.vm);
    if let Some(at) = state.loading.iter().position(|m| *m == name) {
        let mut cycle: Vec<&str> = state.loading[at..].iter().map(String::as_str).collect();
        cycle.push(&name);
        return Err(ScriptError::new(format!(
            "cyclic module dependency: {}",
            cycle.join(" -> ")
        )));
    }
    let proto = match state
        .compiled
        .remove(&name)
        .or_else(|| state.binary.get(&name).cloned())
    {
        Some(p) => p,
        None => {
            let Some(source) = state.source_of(&name) else {
                return Err(ScriptError::new(format!("module '{name}' not found")));
            };
            compile(cx.vm, &name, &source)
                .map(|(p, _)| p)
                .map_err(|e| {
                    ScriptError::new(format!(
                        "error loading module '{name}': {}: {e}",
                        e.location()
                    ))
                })?
        }
    };
    host(cx.vm).loading.push(name.clone());
    let f = cx.vm.load(&proto);
    let name_value = cx.vm.string(&name);
    let result = cx.vm.try_call(f, &[name_value]);
    host(cx.vm).loading.pop();
    let values = result.map_err(|r| ScriptError(crate::context::Repr::Thrown(Box::new(r))))?;
    let value = match values.first().copied() {
        None | Some(rhue_vm::Value::Nil) => rhue_vm::Value::Bool(true),
        Some(v) => v,
    };
    let key = cx.vm.string(&name);
    let _ = cx.vm.heap_mut().with_table(cache, |t| t.set(key, value));
    Ok(cx.lift(value))
}

fn loaded_table(vm: &mut rhue_vm::Vm) -> rhue_vm::TableRef {
    if let Some((t, _)) = &host(vm).loaded {
        return *t;
    }
    let t = vm.heap_mut().alloc_table(rhue_vm::Table::default());
    let pin = vm
        .pin(rhue_vm::Value::Table(t))
        .expect("tables can be pinned");
    host(vm).loaded = Some((t, pin));
    t
}

// ─ Hot Reload ────────────────────────────────────────────────────────────────

/// What a reload did (see [`Engine::reload_module`](crate::Engine::reload_module)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reloaded {
    /// Whether the module had been `require`d. If not, nothing ran, the new
    /// source just waits for the first `require`.
    pub was_loaded: bool,
    /// Old functions that run the new code now.
    pub functions: usize,
    /// Old tables the new ones got merged into.
    pub tables: usize,
    /// Upvalues (module locals and the like) that kept their old values.
    pub upvalues: usize,
}

/// Swaps module `name` over to `source`. Compiles and checks it first, so a
/// broken edit changes nothing. If the module was already `require`d, the new
/// code runs once and gets merged into what the old code made.
pub(crate) fn reload(vm: &mut rhue_vm::Vm, name: &str, source: &str) -> Result<Reloaded, Error> {
    if host(vm).loading.iter().any(|m| m == name) {
        return Err(Error::from_script(ScriptError::new(format!(
            "module '{name}' is still loading"
        ))));
    }
    let (proto, ty) = compile(vm, name, source)?;
    let state = host(vm);
    if state.sources.contains_key(name) {
        state.sources.insert(name.to_string(), source.to_string());
    }
    state.binary.remove(name);
    state.env.define_module(name, ty);
    let cache = loaded_table(vm);
    let key = vm.string(name);
    let old = vm.heap().table(cache).get(key);
    if old == rhue_vm::Value::Nil {
        host(vm).compiled.insert(name.to_string(), proto);
        return Ok(Reloaded::default());
    }
    host(vm).compiled.remove(name);
    host(vm).loading.push(name.to_string());
    let f = vm.load(&proto);
    let name_value = vm.string(name);
    let result = vm.try_call(f, &[name_value]);
    host(vm).loading.pop();
    let values = result.map_err(|r| Error::from_runtime(r.error()))?;
    let new = match values.first().copied() {
        None | Some(rhue_vm::Value::Nil) => rhue_vm::Value::Bool(true),
        Some(v) => v,
    };
    let (keep, stats) = vm.hot_swap(old, new);
    let key = vm.string(name);
    let _ = vm.heap_mut().with_table(cache, |t| t.set(key, keep));
    Ok(Reloaded {
        was_loaded: true,
        functions: stats.functions,
        tables: stats.tables,
        upvalues: stats.upvalues,
    })
}

/// Names of the modules `require` has loaded so far, sorted.
pub(crate) fn loaded_names(vm: &mut rhue_vm::Vm) -> Vec<String> {
    let cache = loaded_table(vm);
    let mut names = Vec::new();
    let mut key = rhue_vm::Value::Nil;
    while let Ok(Some((k, _))) = vm.heap().table(cache).next(key) {
        if let rhue_vm::Value::String(s) = k {
            names.push(vm.heap().get(s).to_string());
        }
        key = k;
    }
    names.sort();
    names
}

/// The source module `name` would load from right now: what was added, or
/// what the loader says.
pub(crate) fn current_source(vm: &mut rhue_vm::Vm, name: &str) -> Option<String> {
    host(vm).source_of(name)
}
