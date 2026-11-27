//! # Rhue
//!
//! Luau-flavoured scripting you can embed in Rust.
//!
//! ```
//! use rhue::{Engine, Value};
//!
//! let mut engine = Engine::new();
//! engine.set_global("name", "Aalto");
//! let values = engine.eval(r#"
//!     local function counter()
//!         local n = 0
//!         return function() n += 1 return n end
//!     end
//!     local next = counter()
//!     next() next()
//!     return next(), "hello, " .. name
//! "#).unwrap();
//! assert_eq!(values, [Value::Number(3.0), Value::from("hello, Aalto")]);
//!
//! let err = engine.eval_expr("1 + true").unwrap_err();
//! assert_eq!(err.to_string(), "attempt to perform arithmetic (add) on boolean");
//! ```
//!
//! ## Calling Rust from scripts, and scripts from Rust
//!
//! ```
//! use rhue::{Context, Engine, Function, Variadic};
//!
//! let mut engine = Engine::new();
//! // parameters and results convert on their own, a wrong type is a
//! // script error ("invalid argument #1 to 'add' (number expected, got string)").
//! engine.register("add", |a: f64, b: f64| a + b);
//! engine.register("sum", |xs: Variadic<f64>| xs.iter().sum::<f64>());
//! // returning Err raises an error in the script
//! engine.register("half", |n: i64| -> Result<i64, String> {
//!     if n % 2 == 0 { Ok(n / 2) } else { Err(format!("{n} is odd")) }
//! });
//! // take `&mut Context` first to call back into scripts
//! engine.register("twice", |cx: &mut Context, f: Function, x: f64| -> Result<f64, rhue::ScriptError> {
//!     let once: f64 = cx.call(&f, x)?;
//!     cx.call(&f, once)
//! });
//!
//! assert_eq!(engine.eval_as::<f64>("return add(1, 2) + sum(1, 2, 3)").unwrap(), 9.0);
//! assert_eq!(engine.eval_as::<f64>("return twice(function(x) return x * 3 end, 2)").unwrap(), 18.0);
//!
//! engine.eval("function greet(who) return 'hi ' .. who end").unwrap();
//! let greeting: String = engine.call_global("greet", "Zae").unwrap();
//! assert_eq!(greeting, "hi Zae");
//! ```
//!
//! Rust types turn into script objects with [`UserData`] (usually derived).
//!
//! ## Types
//!
//! Scripts opt into Luau-style type checking with `--!strict` (or
//! `--!nonstrict`). Registered functions get declared with types from
//! their Rust signatures, and a chunk with type errors doesn't run.
//!
//! ```
//! use rhue::{Engine, ErrorKind};
//!
//! let mut engine = Engine::new();
//! engine.register("add", |a: f64, b: f64| a + b);
//! assert_eq!(engine.global_type("add").unwrap(), "(number, number) -> number");
//!
//! let err = engine.eval("--!strict\nlocal n: number = add(1, 'two')").unwrap_err();
//! assert_eq!(err.kind, ErrorKind::Type);
//! assert_eq!(err.to_string(), "Type '\"two\"' could not be converted into 'number'");
//!
//! let n: f64 = engine.eval_as("--!strict\nlocal function sq(x: number): number return x * x end return sq(add(1, 2))").unwrap();
//! assert_eq!(n, 9.0);
//! ```
//!
//! ## Running untrusted scripts
//!
//! An engine can cap how many instructions a call can run and how much
//! memory scripts can hold. Going over either is a normal error and the
//! engine's still usable after. Each script can get its own globals too.
//!
//! ```
//! use rhue::Engine;
//!
//! let mut engine = Engine::new();
//! engine.set_instruction_limit(Some(100_000));
//! engine.set_memory_limit(Some(1 << 20));
//!
//! let err = engine.eval("while true do end").unwrap_err();
//! assert!(err.to_string().contains("instruction limit"));
//! let err = engine.eval("local t = {} for i = 1, 1e7 do t[i] = i end").unwrap_err();
//! assert!(err.to_string().contains("memory"));
//!
//! // plugins in separate environments can't see each other's globals
//! let a = engine.create_environment();
//! let b = engine.create_environment();
//! engine.eval_in(&a, "secret = 42").unwrap();
//! assert_eq!(engine.eval_in(&b, "return secret").unwrap(), [rhue::Value::Nil]);
//!
//! // but they share `math` and `string`, so lock those down
//! engine.freeze_libraries();
//! assert!(engine.eval_in(&a, "math.floor = nil").is_err());
//! ```
//!
//! ## Modules and compiled chunks
//!
//! `require` finds modules the host gives it, from source or compiled ahead
//! of time. Compiled chunks load without parsing or type-checking, go
//! through the bytecode verifier, and keep their types.
//!
//! ```
//! use rhue::Engine;
//!
//! let mut engine = Engine::new();
//! engine.add_module("geometry", "--!strict
//!     local M = {}
//!     function M.area(w: number, h: number): number return w * h end
//!     return M");
//! let bytes = engine.compile_to_bytes("stats", "return { mean = function(a, b) return (a + b) / 2 end }").unwrap();
//! engine.add_module_bytes("stats", &bytes).unwrap();
//!
//! let n: f64 = engine.eval_as("return require('geometry').area(3, 4) + require('stats').mean(2, 4)").unwrap();
//! assert_eq!(n, 15.0);
//!
//! // the module's types reach scripts that require it
//! assert!(engine.check("main.rhue", "--!strict\nlocal s: string = require('geometry').area(1, 2)").is_err());
//! ```
//!
//! ## Scripts that wait
//!
//! [`Engine::spawn`] runs a script as a task the host resumes, and
//! [`Engine::open_tasks`] adds Luau's `task` library, driven by a clock
//! the host moves forward with [`Engine::step`].
//!
//! ```
//! use rhue::{Engine, TaskState, Value};
//!
//! let mut engine = Engine::new();
//! engine.open_tasks();
//! let mut task = engine.spawn("door", "
//!     local opened = coroutine.yield('waiting for a key')
//!     task.wait(1.5)
//!     return 'opened with ' .. opened
//! ").unwrap();
//! assert_eq!(task.state(), TaskState::Suspended(vec![Value::from("waiting for a key")]));
//!
//! engine.resume(&mut task, vec![Value::from("brass key")]).unwrap();
//! assert!(!task.is_finished()); // now in task.wait
//! engine.step(1.0);
//! assert!(!task.is_finished());
//! engine.step(1.0);
//! assert_eq!(task.state(), TaskState::Finished(vec![Value::from("opened with brass key")]));
//! ```

#![warn(missing_docs)]

mod asyncs;
mod chunk;
mod context;
mod convert;
mod debug;
mod environment;
// compiled either way, `expression_type` is built on the analysis
mod hints;
mod modules;
mod native;
#[cfg(feature = "tooling")]
mod outline;
#[cfg(feature = "serde")]
pub mod serde;
mod tasks;
mod userdata;
mod value;
mod vector;

use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use rhue_bytecode::Proto;
use std::sync::Arc;

pub use asyncs::InterruptHandle;
pub use chunk::Chunk;
pub use context::{Context, ScriptError};
pub use convert::{
    ArgError, Args, ConversionError, FromArg, FromValue, FromValues, IntoValue, IntoValues,
    Variadic,
};
pub use debug::{DebugEvent, Debugger, HookMask, StackFrame};
pub use environment::Environment;
#[cfg(feature = "tooling")]
pub use hints::{HintKind, InlayHint};
#[cfg(feature = "tooling")]
pub use modules::Analysis;
pub use modules::Reloaded;
pub use native::{IntoAsyncNative, IntoNative, IntoNativeResult, WithContext};
#[cfg(feature = "tooling")]
pub use outline::{
    Base, Exports, FieldUse, Outline, SemanticToken, Symbol, SymbolKind, TokenKind, outline,
};
pub use rhue_bytecode::is_chunk;
pub use rhue_common::{Diagnostic, Severity, SourceFile, Span};
pub use rhue_compiler::CompileOptions;
pub use rhue_derive::UserData;
pub use rhue_parser::ast::TypeMode;
pub use tasks::{Task, TaskState};
pub use userdata::{Class, ClassBuilder, IntoHandleMethod, IntoMethod, TypeBuilder, Ud, UserData};
pub use value::{AnyUserData, Buffer, Function, Table, Thread, Value};
pub use vector::VectorBuilder;

/// Memory statistics; see [`Engine::heap_stats`].
pub use rhue_vm::{HeapStats, ThreadEnd};

/// Where `require(name)` looks for a module under `dir`, in order: `a.b` is
/// `dir/a/b.rhue`, then `dir/a/b/init.rhue`. The CLI, the language server and
/// the debugger all find modules this way, so a
/// [loader](Engine::set_module_loader) that uses it agrees with them. It only
/// builds the paths, nothing gets read.
///
/// It doesn't check the name either: `require("/etc/passwd")` would get
/// `/etc/passwd.rhue`. Scripts pick the name, so check it with
/// [`is_module_name`] first.
///
/// ```
/// use std::path::Path;
///
/// let [file, init] = rhue::module_paths(Path::new("scripts"), "npc.brain");
/// assert_eq!(file, Path::new("scripts/npc/brain.rhue"));
/// assert_eq!(init, Path::new("scripts/npc/brain/init.rhue"));
/// ```
pub fn module_paths(dir: &std::path::Path, name: &str) -> [std::path::PathBuf; 2] {
    let rel: std::path::PathBuf = name.split('.').collect();
    [
        dir.join(rel.with_extension("rhue")),
        dir.join(rel.join("init.rhue")),
    ]
}

/// Whether `name` is a module name that [`module_paths`] keeps inside its
/// folder: parts split by `.`, none of them empty, and nothing that a path
/// would treat specially (`/`, `\\`, `:`).
///
/// ```
/// assert!(rhue::is_module_name("npc.brain"));
/// assert!(!rhue::is_module_name("/etc/passwd"));
/// assert!(!rhue::is_module_name("..secrets"));
/// assert!(!rhue::is_module_name("C:\\x"));
/// ```
pub fn is_module_name(name: &str) -> bool {
    name.split('.')
        .all(|part| !part.is_empty() && !part.contains(['/', '\\', ':', '\0']))
}

/// Which phase an [`Error`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Lexing or parsing failed.
    Syntax,
    /// The source parsed but couldn't be compiled.
    Compile,
    /// Type checking (`--!strict` or `--!nonstrict`) found errors, so the
    /// script didn't run.
    Type,
    /// The script raised an error while running.
    Runtime,
    /// Bytes given to [`Engine::load`] aren't a compiled chunk this version can
    /// run (wrong format, damaged, or the verifier rejected it).
    Load,
}

/// An error from compiling or running a script. It keeps a copy of the source
/// its spans point into, so it can render itself with [`Error::render`] and can
/// be sent across threads.
#[derive(Clone, Debug)]
pub struct Error {
    /// What went wrong: syntax, names, types, or a runtime error.
    pub kind: ErrorKind,
    /// Every problem found, with spans into the source (at least one).
    pub diagnostics: Vec<Diagnostic>,
    source_name: Arc<str>,
    source: Arc<str>,
}

impl Error {
    fn new(kind: ErrorKind, diagnostics: Vec<Diagnostic>, source: &SourceFile) -> Self {
        debug_assert!(!diagnostics.is_empty());
        Error {
            kind,
            diagnostics,
            source_name: Arc::from(&*source.name),
            source: Arc::from(&*source.text),
        }
    }

    /// `name:line` of the first diagnostic.
    fn location(&self) -> String {
        let start = self.diagnostic().primary.span.start as usize;
        let line = self.source[..start.min(self.source.len())]
            .bytes()
            .filter(|&b| b == b'\n')
            .count()
            + 1;
        format!("{}:{line}", self.source_name)
    }

    fn from_runtime(e: &rhue_vm::RuntimeError) -> Self {
        Error::new(ErrorKind::Runtime, vec![e.to_diagnostic()], &e.source)
    }

    /// Whether the script got stopped through an [`InterruptHandle`].
    pub fn is_interrupted(&self) -> bool {
        self.kind == ErrorKind::Runtime
            && self
                .diagnostics
                .first()
                .is_some_and(|d| d.message == rhue_vm::INTERRUPTED)
    }

    /// Whether a thread got killed or cancelled (see
    /// [`Context::kill_thread`]) rather than failing.
    pub fn is_killed(&self) -> bool {
        self.kind == ErrorKind::Runtime
            && self
                .diagnostics
                .first()
                .is_some_and(|d| d.message == rhue_vm::KILLED || d.message == rhue_vm::CANCELLED)
    }

    /// An error from Rust-side work (a conversion, a native) or a script.
    fn from_script(e: ScriptError) -> Self {
        match e.0 {
            context::Repr::Thrown(r) => Error::from_runtime(r.error()),
            _ => Error::new(
                ErrorKind::Runtime,
                vec![Diagnostic::error(e.message(), Span::default())],
                &SourceFile::new("[host]", ""),
            ),
        }
    }

    /// The first (most relevant) diagnostic.
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostics[0]
    }

    /// The name of the source file the error points into. For a runtime error
    /// inside a function defined by an earlier `eval`, that's the earlier
    /// source, not the one being run.
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Pretty-prints every diagnostic against the original source.
    pub fn render(&self, color: bool) -> String {
        if self.kind == ErrorKind::Load {
            // there's no source to point into
            return format!("error: {}\n", self.diagnostic().message);
        }
        self.diagnostics
            .iter()
            .map(|d| d.render(&self.source_name, &self.source, color))
            .collect()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.diagnostic().message)
    }
}

impl std::error::Error for Error {}

/// The entry point for embedding Rhue.
///
/// Globals (functions included) stick around between calls, so a bunch of
/// `eval`s act like one session (the REPL relies on this). Locals don't, each
/// chunk gets its own scope.
pub struct Engine {
    vm: rhue_vm::Vm,
    /// The `task` library's scheduler, once [`Engine::open_tasks`] adds it.
    tasks: Option<std::rc::Rc<std::cell::RefCell<tasks::Scheduler>>>,
    /// Futures async natives are waiting on (see [`Engine::eval_async`]).
    asyncs: std::rc::Rc<std::cell::RefCell<asyncs::AsyncState>>,
}

/// The type-checking environment, kept with the VM so natives that set up
/// userdata types can declare them.
pub(crate) fn type_env(vm: &mut rhue_vm::Vm) -> &mut rhue_types::Env {
    &mut modules::host(vm).env
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// An engine with the standard library: base functions (`print`, `type`,
    /// `tostring`, `tonumber`, `pairs`, `ipairs`, `pcall`, `error`, ...) and
    /// the `table`, `string` and `math` libraries.
    pub fn new() -> Self {
        let mut vm = rhue_vm::Vm::new();
        *vm.host_data() = Some(Box::new(modules::HostState::new(rhue_types::Env::new())));
        let mut engine = Engine {
            vm,
            tasks: None,
            asyncs: Default::default(),
        };
        engine.register("require", modules::require);
        engine
    }

    /// An engine with no globals at all.
    pub fn bare() -> Self {
        let mut vm = rhue_vm::Vm::bare();
        *vm.host_data() = Some(Box::new(modules::HostState::new(rhue_types::Env::bare())));
        Engine {
            vm,
            tasks: None,
            asyncs: Default::default(),
        }
    }

    /// How chunks without a `--!strict` / `--!nonstrict` / `--!nocheck`
    /// directive get type-checked. The default is [`TypeMode::NoCheck`] so
    /// scripts opt in, [`TypeMode::Strict`] makes it opt-out.
    pub fn set_default_type_mode(&mut self, mode: TypeMode) {
        modules::host(&mut self.vm).default_mode = mode;
    }

    /// What the type checker thinks a global's type is, in Luau syntax (`None`
    /// if it doesn't know anything about it). Handy for checking what a
    /// registered function got declared as.
    pub fn global_type(&mut self, name: &str) -> Option<String> {
        type_env(&mut self.vm).global(name).map(|t| t.to_string())
    }

    /// Tells the type checker a global's type (in Luau syntax), for globals the
    /// host sets without [`register`](Self::register) or
    /// [`set_global`](Self::set_global): `engine.declare("config", "{ debug:
    /// boolean }")`.
    pub fn declare(&mut self, name: &str, ty: &str) -> Result<(), Error> {
        type_env(&mut self.vm)
            .declare_global(name, ty)
            .map_err(|d| Error::new(ErrorKind::Syntax, d, &SourceFile::new("<declaration>", ty)))
    }

    /// Tells the checker what reading a name that isn't a member of class
    /// `class` gives, for a class with an index hook
    /// ([`ClassBuilder::index`]): `declare_index("Node", "Node")` makes
    /// `world.Box` a `Node` in strict scripts. Classes under it get it too.
    pub fn declare_index(&mut self, class: &str, ty: &str) -> Result<(), Error> {
        type_env(&mut self.vm)
            .declare_index(class, ty)
            .map_err(|d| Error::new(ErrorKind::Syntax, d, &SourceFile::new("<declaration>", ty)))
    }

    /// Tells the checker argument `arg` (0 is the first after the object) of
    /// class `class`'s method `method` names a class, as `@class(param)` does
    /// in a definitions file. A string literal naming a class there makes a
    /// method that returns a class return that one
    /// (`FindFirstChildOfClass("Box")` is a `Box?`), and makes one that
    /// returns a boolean narrow the object in an `if` (`if n:IsA("Box")
    /// then`). A literal that isn't a class is an error in strict scripts.
    /// The method has to be the class's own and take a string there.
    pub fn declare_class_param(
        &mut self,
        class: &str,
        method: &str,
        arg: usize,
    ) -> Result<(), Error> {
        type_env(&mut self.vm)
            .declare_class_param(class, method, arg)
            .map_err(|m| {
                let d = Diagnostic::error(m, Span::new(0, 0));
                Error::new(
                    ErrorKind::Type,
                    vec![d],
                    &SourceFile::new("<declaration>", ""),
                )
            })
    }

    /// The same as [`declare_class_param`](Self::declare_class_param) for a
    /// global function, or a function in a global table: `path` is `spawn`
    /// or `Instance.new`, and `arg` counts from 0. Then
    /// `Instance.new("Box")` is a `Box` to the checker.
    pub fn declare_global_class_param(&mut self, path: &str, arg: usize) -> Result<(), Error> {
        type_env(&mut self.vm)
            .declare_global_class_param(path, arg)
            .map_err(|m| {
                let d = Diagnostic::error(m, Span::new(0, 0));
                Error::new(
                    ErrorKind::Type,
                    vec![d],
                    &SourceFile::new("<declaration>", ""),
                )
            })
    }

    /// Everything the host declared, as a definitions file (`.d.rhue`) the
    /// standalone tools can read: `rhue --check --definitions` and
    /// `rhue-lsp`. Classes, their members, globals, and aliases from loaded
    /// definitions files, each sorted by name so it diffs cleanly. Parameter
    /// names aren't kept, so they come out as `arg1`, `arg2` and so on. The
    /// standard library is left out. Nothing needs to have run first.
    pub fn definitions(&mut self) -> String {
        type_env(&mut self.vm).definitions()
    }

    /// Tells the type checker what a definitions file (`.d.rhue`) declares,
    /// for checking scripts written for a host without the host. Types only,
    /// nothing gets defined at run time. Its aliases (`type Signal<T...> =
    /// ...`) can be named by scripts too. With any error nothing changes.
    pub fn load_definitions(&mut self, name: &str, source: &str) -> Result<(), Error> {
        let file = SourceFile::new(name, source);
        let defs = rhue_parser::parse_definitions(source)
            .map_err(|d| Error::new(ErrorKind::Syntax, d, &file))?;
        type_env(&mut self.vm)
            .load_definitions(&defs, source)
            .map_err(|d| Error::new(ErrorKind::Type, d, &file))
    }

    /// What a definitions file declares that this engine doesn't have, for
    /// a test in the host's own suite that keeps a checked-in `.d.rhue` in
    /// step: `Node`, `Shape extends Node`, `Node.Name`, `Node.Name can be
    /// set`, `Node:Destroy`, `Node[string]`, `world`. Only names get
    /// compared, not types. Empty means everything's there.
    pub fn check_definitions(&mut self, name: &str, source: &str) -> Result<Vec<String>, Error> {
        let file = SourceFile::new(name, source);
        let defs = rhue_parser::parse_definitions(source)
            .map_err(|d| Error::new(ErrorKind::Syntax, d, &file))?;
        type_env(&mut self.vm)
            .check_definitions(&defs, source)
            .map_err(|d| Error::new(ErrorKind::Type, d, &file))
    }

    /// An engine that compiles with `options` (see [`CompileOptions`]).
    pub fn with_options(options: CompileOptions) -> Self {
        let mut engine = Self::new();
        *engine.options_mut() = options;
        engine
    }

    /// The options new compiles use, to change after it's made.
    pub fn options_mut(&mut self) -> &mut CompileOptions {
        &mut modules::host(&mut self.vm).options
    }

    /// Makes a module available to `require(name)`, from source. Modules added
    /// here get found before the [loader](Self::set_module_loader) is asked.
    pub fn add_module(&mut self, name: &str, source: &str) {
        let state = modules::host(&mut self.vm);
        state.sources.insert(name.to_string(), source.to_string());
        state.compiled.remove(name);
        state.binary.remove(name);
    }

    /// Swaps module `name` over to new source while scripts keep running (hot
    /// reload).
    ///
    /// The new source gets compiled and checked first, so a broken edit
    /// changes nothing and comes back as an `Err`. If the module hasn't been
    /// `require`d yet, the new source just waits for the first `require`.
    /// Otherwise it runs once and gets merged into what the old code made:
    ///
    /// - functions at the same spot (a field of the module, a field of a table
    ///   in it, a local they capture) run the new code, through every handle to
    ///   them. A call that's already running finishes on the old code
    /// - the module's tables stay the same tables, so `local m = require("m")`
    ///   elsewhere sees the changes. They take the new functions and any new
    ///   keys, keep their old values for the rest (that's the state), and drop
    ///   functions the new code doesn't have
    /// - locals the functions capture keep their values (`local count = 0`
    ///   keeps counting)
    ///
    /// Closures the old code built at runtime keep their code until something
    /// builds them again. Checked code compiled after the reload sees the
    /// module's new type.
    ///
    /// ```
    /// let mut engine = rhue::Engine::new();
    /// engine.add_module("counter", "
    ///     local M = { hits = 0 }
    ///     function M.hit() M.hits += 1 return 'old' end
    ///     return M");
    /// engine.eval("counter = require('counter') counter.hit()").unwrap();
    ///
    /// engine.reload_module("counter", "
    ///     local M = { hits = 0 }
    ///     function M.hit() M.hits += 1 return 'new' end
    ///     return M").unwrap();
    /// let (said, hits): (String, f64) = engine.eval_as("return counter.hit(), counter.hits").unwrap();
    /// assert_eq!((said.as_str(), hits), ("new", 2.0));
    /// ```
    pub fn reload_module(&mut self, name: &str, source: &str) -> Result<Reloaded, Error> {
        modules::reload(&mut self.vm, name, source)
    }

    /// [`reload_module`](Self::reload_module) with whatever source the module
    /// would load from right now: what [`add_module`](Self::add_module) gave,
    /// or what the [loader](Self::set_module_loader) returns. That's the one a
    /// file-watching host calls when a file changes.
    pub fn reload(&mut self, name: &str) -> Result<Reloaded, Error> {
        let Some(source) = modules::current_source(&mut self.vm, name) else {
            return Err(Error::from_script(ScriptError::new(format!(
                "module '{name}' not found"
            ))));
        };
        modules::reload(&mut self.vm, name, &source)
    }

    /// The modules `require` has loaded so far, sorted. Handy for knowing what
    /// to watch.
    pub fn loaded_modules(&mut self) -> Vec<String> {
        modules::loaded_names(&mut self.vm)
    }

    /// Sets how `require` finds modules it hasn't been given. `loader` maps a
    /// module name to its source, or `None` if there's no such module. Rhue
    /// never touches the file system itself, so this is where a host decides
    /// what `require("a.b")` means (a file under a scripts folder, an asset
    /// bundle entry, ...). [`module_paths`] gives the paths the Rhue tools use.
    pub fn set_module_loader(&mut self, loader: impl Fn(&str) -> Option<String> + 'static) {
        modules::host(&mut self.vm).loader = Some(Rc::new(loader));
    }

    /// Caps how many VM instructions one run can execute. Going over is a
    /// runtime error scripts can't catch, it's for stopping runaway scripts.
    /// `None` means no limit. The count's approximate: loops get charged their
    /// body's length per iteration and calls one each, so every loop and
    /// recursion gets stopped but straight-line code is free.
    pub fn set_instruction_limit(&mut self, limit: Option<u64>) {
        self.vm.set_instruction_limit(limit);
    }

    /// Caps how deep script calls can nest before `stack overflow` (a normal
    /// error, `pcall` catches it). The default and the most is 10,000, and
    /// each coroutine gets its own count.
    pub fn set_max_call_depth(&mut self, depth: usize) {
        self.vm.set_max_call_depth(depth);
    }

    /// Caps how much memory scripts can keep allocated, in bytes as the
    /// collector counts them (an approximation of the real heap). Hitting it
    /// stops the script with `not enough memory`, which `pcall` can't catch. A
    /// collection runs first, and strings or tables that wouldn't fit get
    /// refused before they're built. `None` means no limit.
    pub fn set_memory_limit(&mut self, limit: Option<usize>) {
        self.vm.set_memory_limit(limit);
    }

    /// Freezes the library tables (`math`, `string`, `table`, `task` and the
    /// rest) and the metatable strings share. Environments keep their own
    /// globals but share these, so without this one script can do
    /// `math.floor = nil` and break it for everyone. Libraries opened after
    /// this get frozen too, there's no undo, and tables you added yourself are
    /// left alone (`table.freeze` them if you want).
    pub fn freeze_libraries(&mut self) {
        self.vm.freeze_libraries();
    }

    /// How much memory scripts are holding right now, as the collector counts
    /// it.
    pub fn memory_used(&self) -> usize {
        self.vm.memory_used()
    }

    /// Calls `hook` on the debug events in `mask` (see [`Debugger`]): a new
    /// line, entering or leaving a script function. Returning `Err` stops the
    /// script with that message (scripts can't catch it). Replaces any hook set
    /// before.
    pub fn set_debug_hook(
        &mut self,
        mask: HookMask,
        mut hook: impl FnMut(&mut Debugger<'_, '_>, DebugEvent) -> Result<(), String> + 'static,
    ) {
        self.vm.set_debug_hook(
            mask,
            Some(Box::new(move |ctx, event| {
                hook(&mut Debugger { ctx }, event)
            })),
        );
    }

    /// Removes the debug hook.
    pub fn clear_debug_hook(&mut self) {
        self.vm.set_debug_hook(HookMask::default(), None);
    }

    /// Runs a full garbage collection now. (Scripts also collect on their own
    /// as they allocate.)
    pub fn gc(&mut self) {
        self.vm.collect_garbage();
    }

    /// Testing aid: collect garbage before every instruction. Really slow, but
    /// any object the collector fails to see as live gets freed immediately, so
    /// those bugs show up right away.
    #[doc(hidden)]
    pub fn set_gc_stress(&mut self, on: bool) {
        self.vm.heap_mut().set_stress(on);
    }

    /// Live objects and approximate memory in use.
    pub fn heap_stats(&self) -> HeapStats {
        self.vm.heap_stats()
    }

    /// Sends `print` output somewhere other than stdout.
    pub fn on_print(&mut self, output: impl FnMut(&str) + 'static) {
        self.vm.set_output(output);
    }

    /// A [`Context`] for working with tables, functions and userdata directly.
    pub fn context(&mut self) -> Context<'_> {
        Context::new(&mut self.vm)
    }

    /// Sets a global. The type checker learns its type from the Rust type
    /// (`any` for a [`Value`]).
    pub fn set_global<V: IntoValue>(&mut self, name: &str, value: V) {
        V::declare_types(&mut self.vm);
        let ty = V::luau_type();
        userdata::declare(type_env(&mut self.vm).declare_global(name, &ty), &ty);
        self.context().set_global(name, value);
    }

    /// A global, as a [`Value`] (`Value::Nil` if unset).
    pub fn global(&mut self, name: &str) -> Value {
        let v = self.vm.global(name);
        self.context().lift(v)
    }

    /// A global, converted: `engine.get::<f64>("score")`.
    pub fn get<T: FromValue>(&mut self, name: &str) -> Result<T, Error> {
        self.context().global(name).map_err(Error::from_script)
    }

    /// Makes a Rust function callable from scripts as the global `name`.
    ///
    /// Parameters can be any [`FromValue`] type (`f64`, integers, `String`,
    /// `bool`, `Option<T>`, `Vec<T>`, `Table`, `Function`, `Value`, userdata
    /// types, ...), with a [`Variadic<T>`] last to take the rest. The result
    /// can be any [`IntoValues`] (a value, `()`, a tuple for several results)
    /// or a `Result` of one, whose `Err` raises a script error. A first
    /// parameter of `&mut Context` gives access to the VM.
    pub fn register<M, F: IntoNative<M>>(&mut self, name: &str, f: F) {
        F::declare_types(&mut self.vm);
        let sig = F::signature();
        userdata::declare(type_env(&mut self.vm).declare_global(name, &sig), &sig);
        let native = f.into_native(Rc::from(name));
        self.vm.register_native(name, native);
    }

    /// Sets up userdata type `T` (running [`UserData::register`] if it hasn't
    /// yet) and returns a builder for adding methods and properties.
    pub fn register_type<T: UserData>(&mut self) -> TypeBuilder<'_, T> {
        let ty = userdata::type_id_of::<T>(&mut self.vm);
        TypeBuilder {
            vm: &mut self.vm,
            ty,
            _t: PhantomData,
        }
    }

    /// Starts a class whose objects are all handles of host type `H`, for a
    /// host whose objects form a class tree (a `Box` that's a `Shape` that's a
    /// `Node`) behind one Rust type. Give it a parent first if it has one, add
    /// members, then [`finish`](ClassBuilder::finish) it. Objects get made with
    /// [`Context::create_object`].
    ///
    /// ```
    /// # use rhue::{Engine, Value};
    /// # use std::cell::Cell;
    /// struct Handle { size: Cell<f64> }
    ///
    /// let mut engine = Engine::new();
    /// let node = engine.define_class::<Handle>("Node")
    ///     .method("Grow", |h: &Handle, by: f64| h.size.set(h.size.get() + by))
    ///     .finish();
    /// let shape = engine.define_class::<Handle>("Shape")
    ///     .parent(&node)
    ///     .property("Size", |h: &Handle| h.size.get(), |h: &Handle, v: f64| h.size.set(v))
    ///     .finish();
    /// let obj = engine.context().create_object(&shape, Handle { size: Cell::new(1.0) });
    /// engine.set_global("s", obj);
    /// let v = engine.eval("s:Grow(2) return s.Size, typeof(s)").unwrap();
    /// assert_eq!(v, [Value::Number(3.0), Value::from("Node")]);
    /// ```
    pub fn define_class<H: 'static>(&mut self, name: &str) -> ClassBuilder<'_, H> {
        ClassBuilder {
            vm: &mut self.vm,
            name: std::rc::Rc::from(name),
            parent: None,
            id: None,
            _h: PhantomData,
        }
    }

    /// Adds host members to the built-in `vector`: properties like
    /// `v.Magnitude`, methods like `v:Dot(w)`, and a constructor table if
    /// scripts expect one. Reading `v.x` costs the same as before.
    ///
    /// ```
    /// # use rhue::{Engine, Value};
    /// let mut engine = Engine::new();
    /// engine.vector()
    ///     .property("Magnitude", |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt())
    ///     .method("Dot", |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2])
    ///     .constructor("Vector3");
    /// let v = engine.eval("local v = Vector3.new(3, 4) return v.Magnitude, v:Dot(Vector3.one)").unwrap();
    /// assert_eq!(v, [Value::Number(5.0), Value::Number(7.0)]);
    /// ```
    pub fn vector(&mut self) -> VectorBuilder<'_> {
        VectorBuilder { vm: &mut self.vm }
    }

    /// Adds members to a class that's already made. Every class under it gets
    /// them too, except where a child has a member of the same name itself.
    ///
    /// # Panics
    ///
    /// If `class` is from another engine.
    pub fn extend_class<H: 'static>(&mut self, class: &Class<H>) -> ClassBuilder<'_, H> {
        assert_eq!(
            class.vm,
            self.vm.id(),
            "a class from one Engine was used in another"
        );
        let parent = self
            .vm
            .class_parent(class.id)
            .map(|p| (p, std::rc::Rc::from(self.vm.class_name(p))));
        ClassBuilder {
            vm: &mut self.vm,
            name: class.name.clone(),
            parent,
            id: Some(class.id),
            _h: PhantomData,
        }
    }

    /// Calls a script (or native) function with `args` and converts its
    /// results: `let n: f64 = engine.call(&f, (1, 2))?`. Each call gets the
    /// full instruction budget.
    pub fn call<R: FromValues>(
        &mut self,
        func: &Function,
        args: impl IntoValues,
    ) -> Result<R, Error> {
        self.context().call(func, args).map_err(Error::from_script)
    }

    /// Calls any callable value, including a table with `__call`.
    pub fn call_value<R: FromValues>(
        &mut self,
        func: &Value,
        args: impl IntoValues,
    ) -> Result<R, Error> {
        self.context()
            .call_value(func, args)
            .map_err(Error::from_script)
    }

    /// Calls the global function `name`.
    pub fn call_global<R: FromValues>(
        &mut self,
        name: &str,
        args: impl IntoValues,
    ) -> Result<R, Error> {
        self.context()
            .call_global(name, args)
            .map_err(Error::from_script)
    }

    /// Compiles a chunk without running it (to inspect or cache it, say).
    /// Type-checks it first if its directive (or the default mode) says to. The
    /// globals it defines become known to chunks checked later.
    pub fn compile(&mut self, name: &str, source: &str) -> Result<Chunk, Error> {
        let file = SourceFile::new(name, source);
        let chunk = rhue_parser::parse_chunk(source)
            .map_err(|d| Error::new(ErrorKind::Syntax, d, &file))?;
        self.compile_ast(&chunk, &file)
    }

    /// Compiles a single expression as `return <expr>`.
    pub fn compile_expr(&mut self, name: &str, source: &str) -> Result<Chunk, Error> {
        let file = SourceFile::new(name, source);
        let chunk =
            rhue_parser::parse_expr(source).map_err(|d| Error::new(ErrorKind::Syntax, d, &file))?;
        self.compile_ast(&chunk, &file)
    }

    fn compile_ast(
        &mut self,
        chunk: &rhue_parser::ast::Chunk,
        file: &Rc<SourceFile>,
    ) -> Result<Chunk, Error> {
        modules::compile_ast(&mut self.vm, chunk, file).map(|c| Chunk(c.proto))
    }

    /// Analyzes a chunk for editor tooling without compiling or running it: its
    /// errors, the type of every expression, where each local is declared and
    /// used, and (with `members_at`, a byte offset) what can come after `expr.`
    /// for the expression ending there. Types are inferred like `--!strict`
    /// would whatever the chunk's mode, errors are whatever its mode reports.
    /// Modules it `require`s get checked (not run) to learn their types.
    ///
    /// Needs the `tooling` feature, which isn't covered by semver.
    #[cfg(feature = "tooling")]
    pub fn analyze(&mut self, source: &str, members_at: Option<u32>) -> Analysis {
        modules::analyze(&mut self.vm, source, members_at)
    }

    /// The type the checker gives an expression, as text (`number`, `{ name:
    /// string }`...), checked strictly against the engine's globals, like a
    /// REPL's `:type` shows it. A type error in the expression is an `Err`.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "an offset into a short constant string"
    )]
    pub fn expression_type(&mut self, expr: &str) -> Result<String, Error> {
        // bound to a local, whose recorded type is the expression's
        const PREFIX: &str = "--!strict\nlocal __type = (";
        let source = format!("{PREFIX}{expr}\n)");
        let file = SourceFile::new("<expression>", source.as_str());
        let analysis = modules::analyze(&mut self.vm, &source, None);
        if !analysis.diagnostics.is_empty() {
            return Err(Error::new(ErrorKind::Type, analysis.diagnostics, &file));
        }
        let at = PREFIX.find("__type").expect("the local is in the prefix") as u32;
        let name = Span::new(at, at + 6);
        Ok(analysis
            .types
            .iter()
            .rev()
            .find(|(s, _)| *s == name)
            .map_or_else(|| "any".to_string(), |(_, t)| t.clone()))
    }

    /// Type-checks a chunk without running it, as `--!strict` (or its own
    /// directive) says, and returns every type error. Unlike
    /// [`compile`](Self::compile), a chunk with no directive gets checked in
    /// strict mode.
    pub fn check(&mut self, name: &str, source: &str) -> Result<(), Error> {
        let file = SourceFile::new(name, source);
        let chunk = rhue_parser::parse_chunk(source)
            .map_err(|d| Error::new(ErrorKind::Syntax, d, &file))?;
        let res = rhue_compiler::resolve::resolve(&chunk)
            .map_err(|d| Error::new(ErrorKind::Compile, d, &file))?;
        let mode = chunk.mode.map_or(TypeMode::Strict, |(m, _)| m);
        let checked = modules::check(&mut self.vm, &chunk, &res, mode);
        if checked.errors.is_empty() {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Type, checked.errors, &file))
        }
    }

    /// Loads a chunk saved with [`Chunk::to_bytes`] (see also
    /// [`compile_to_bytes`](Self::compile_to_bytes)) without parsing or
    /// type-checking it. The bytes go through the bytecode verifier, so damaged
    /// or hand-made chunks get refused ([`ErrorKind::Load`]) instead of run.
    /// The result can be [`run`](Self::run) as many times as you want.
    pub fn load(&mut self, bytes: &[u8]) -> Result<Chunk, Error> {
        let (proto, types) = self.read_chunk(bytes)?;
        if let Some(types) = &types {
            modules::declare_chunk_types(&mut self.vm, types);
        }
        Ok(Chunk(proto))
    }

    /// Reads and verifies a chunk, without declaring its types.
    fn read_chunk(
        &mut self,
        bytes: &[u8],
    ) -> Result<(Rc<Proto>, Option<rhue_bytecode::ChunkTypes>), Error> {
        let fail = |message: String, source: &Rc<SourceFile>| {
            Error::new(
                ErrorKind::Load,
                vec![Diagnostic::error(message, Span::default())],
                source,
            )
        };
        let (proto, types) = rhue_bytecode::read_chunk(bytes)
            .map_err(|e| fail(e.to_string(), &SourceFile::new("<compiled chunk>", "")))?;
        rhue_bytecode::verify_once(&proto).map_err(|e| {
            fail(
                format!("compiled chunk rejected: {e}"),
                &SourceFile::new(proto.source.name.clone(), ""),
            )
        })?;
        Ok((proto, types))
    }

    /// Compiles a chunk (type-checking it like [`compile`](Self::compile) does)
    /// and serializes it for [`load`](Self::load). The bytes carry the types of
    /// what it returns and the globals it defines, so code checked after
    /// loading it (or `require`ing it, see
    /// [`add_module_bytes`](Self::add_module_bytes)) still sees them.
    pub fn compile_to_bytes(&mut self, name: &str, source: &str) -> Result<Vec<u8>, Error> {
        let compiled = modules::compile_typed(&mut self.vm, name, source)?;
        let types = modules::chunk_types(&compiled);
        Ok(rhue_bytecode::write_chunk(&compiled.proto, Some(&types)))
    }

    /// Makes a compiled module available to `require(name)`, from bytes made by
    /// [`compile_to_bytes`](Self::compile_to_bytes) (verified here). Its saved
    /// type is what checked chunks that require it see, and the globals it
    /// defines get declared.
    pub fn add_module_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<(), Error> {
        let (proto, types) = self.read_chunk(bytes)?;
        let ty = types
            .as_ref()
            .map(|t| modules::declare_chunk_types(&mut self.vm, t));
        let state = modules::host(&mut self.vm);
        state.sources.remove(name);
        state.compiled.remove(name);
        state.binary.insert(name.to_string(), proto);
        if let Some(ty) = ty {
            state.env.define_module(name, ty);
        }
        Ok(())
    }

    /// Runs a compiled chunk and returns what it returned.
    ///
    /// A prototype that didn't come from [`compile`](Self::compile) gets
    /// verified first (once), and rejected with [`ErrorKind::Compile`] if its
    /// bytecode is malformed.
    pub fn run(&mut self, chunk: &Chunk) -> Result<Vec<Value>, Error> {
        let proto = &chunk.0;
        self.verify(proto)?;
        let values = self
            .vm
            .execute(proto)
            .map_err(|e| Error::from_runtime(&e))?;
        let cx = self.context();
        Ok(values.into_iter().map(|v| cx.lift(v)).collect())
    }

    /// Compiles and runs a chunk, returning whatever it `return`s.
    pub fn eval(&mut self, source: &str) -> Result<Vec<Value>, Error> {
        self.eval_named("<eval>", source)
    }

    /// [`eval`](Self::eval) with `name` as the chunk's name in errors and tracebacks.
    pub fn eval_named(&mut self, name: &str, source: &str) -> Result<Vec<Value>, Error> {
        let proto = self.compile(name, source)?;
        self.run(&proto)
    }

    /// Runs the bytecode verifier (once per prototype).
    fn verify(&self, proto: &Rc<Proto>) -> Result<(), Error> {
        rhue_bytecode::verify_once(proto).map_err(|e| {
            Error::new(
                ErrorKind::Compile,
                vec![Diagnostic::error(e.to_string(), proto.span)],
                &proto.source,
            )
        })
    }

    /// Evaluates one expression and returns its (first) value.
    pub fn eval_expr(&mut self, source: &str) -> Result<Value, Error> {
        let proto = self.compile_expr("<expr>", source)?;
        let mut values = self.run(&proto)?;
        Ok(if values.is_empty() {
            Value::Nil
        } else {
            values.swap_remove(0)
        })
    }

    /// Compiles and runs a chunk, converting what it returns:
    /// `engine.eval_as::<(f64, String)>("return 1, 'a'")`.
    pub fn eval_as<R: FromValues>(&mut self, source: &str) -> Result<R, Error> {
        let values = self.eval(source)?;
        let mut cx = self.context();
        R::from_values(values, &mut cx).map_err(|e| {
            Error::from_script(ScriptError::new(format!(
                "invalid result #{} ({})",
                e.index, e.error
            )))
        })
    }
}
