//! Debug hooks for hosts. Get told when a script hits a new line, enters
//! or leaves a function, and look at the stack while it's stopped there.
//! Breakpoints and stepping are a few lines on top (that's how `rhue-dap`
//! is built).
//!
//! ```
//! use std::{cell::RefCell, rc::Rc};
//! use rhue::{DebugEvent, Engine, HookMask};
//!
//! let mut engine = Engine::new();
//! let seen = Rc::new(RefCell::new(Vec::new()));
//! let log = seen.clone();
//! engine.set_debug_hook(HookMask::new().line(true), move |dbg, event| {
//!     if event == DebugEvent::Line(3) {
//!         // stopped on line 3, what's in scope?
//!         for (name, value) in dbg.locals(0) {
//!             log.borrow_mut().push(format!("{name} = {value}"));
//!         }
//!     }
//!     Ok(())
//! });
//! engine.eval("local a = 1\nlocal b = a + 1\nprint(a, b)").unwrap();
//! assert_eq!(*seen.borrow(), ["a = 1", "b = 2"]);
//! ```

pub use rhue_vm::{DebugEvent, HookMask};

use crate::{Context, Value};

/// One frame of the stack.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct StackFrame {
    /// The function's name, `<main>` for a chunk, `<anonymous>` if it doesn't
    /// have one.
    pub function: String,
    /// The chunk name its source was compiled under.
    pub source: String,
    /// The line it's on (for a caller, the line of its call), 1-based.
    pub line: u32,
}

/// What a debug hook can look at, the stack of the paused script.
pub struct Debugger<'a, 'b> {
    pub(crate) ctx: &'a mut rhue_vm::DebugCtx<'b>,
}

impl Debugger<'_, '_> {
    /// How many script frames are on the stack (frame 0 is the one running).
    pub fn depth(&self) -> usize {
        self.ctx.depth()
    }

    /// Frame `level` of the stack (0 is the one running), if there is one.
    pub fn frame(&self, level: usize) -> Option<StackFrame> {
        self.ctx.frame(level).map(|f| StackFrame {
            function: f.function,
            source: f.source.name.to_string(),
            line: f.line,
        })
    }

    /// Every frame, innermost first.
    pub fn stack(&self) -> Vec<StackFrame> {
        (0..self.depth()).filter_map(|l| self.frame(l)).collect()
    }

    /// The locals in scope in frame `level`, in declaration order.
    pub fn locals(&mut self, level: usize) -> Vec<(String, Value)> {
        let raw = self.ctx.locals(level);
        let cx = Context::new(self.ctx.vm());
        raw.into_iter()
            .map(|(n, v)| (n.to_string(), cx.lift(v)))
            .collect()
    }

    /// The upvalues frame `level`'s function closes over.
    pub fn upvalues(&mut self, level: usize) -> Vec<(String, Value)> {
        let raw = self.ctx.upvalues(level);
        let cx = Context::new(self.ctx.vm());
        raw.into_iter()
            .map(|(n, v)| (n.to_string(), cx.lift(v)))
            .collect()
    }

    /// Evaluates `expr` (an expression, or a list of them) as if it were
    /// written in frame `level`. The frame's locals and upvalues are in scope,
    /// then globals. Returns its values, or the syntax or runtime error as
    /// text.
    ///
    /// Locals get passed in by value, so `x = 5` changes a copy (a table they
    /// hold is shared though, so `t.x = 5` sticks). Statements work too
    /// (`print(x)`, `for ...`), returning nothing unless they `return`. The
    /// hook doesn't get called for code run this way.
    pub fn eval(&mut self, level: usize, expr: &str) -> Result<Vec<Value>, String> {
        let mut names: Vec<std::rc::Rc<str>> = Vec::new();
        let mut values = Vec::new();
        let is_name = |n: &str| {
            n.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        // locals shadow upvalues with the same name
        let mut visible = self.ctx.locals(level);
        visible.reverse();
        visible.extend(self.ctx.upvalues(level));
        for (n, v) in visible {
            if is_name(&n) && !names.contains(&n) {
                names.push(n);
                values.push(v);
            }
        }
        let prelude = if names.is_empty() {
            String::new()
        } else {
            let list: Vec<&str> = names.iter().map(|n| &**n).collect();
            format!("local {} = ...; ", list.join(", "))
        };
        let vm = self.ctx.vm();
        // an expression first, statements if that isn't one
        let proto = crate::modules::compile(vm, "<eval>", &format!("{prelude}return {expr}"))
            .or_else(|first| {
                crate::modules::compile(vm, "<eval>", &format!("{prelude}{expr}"))
                    .map_err(|_| first)
            })
            .map_err(|e| e.to_string())?
            .0;
        let f = vm.load(&proto);
        match vm.try_call(f, &values) {
            Ok(rets) => {
                let cx = Context::new(vm);
                Ok(rets.into_iter().map(|v| cx.lift(v)).collect())
            }
            Err(thrown) => {
                Err(crate::ScriptError(crate::context::Repr::Thrown(Box::new(thrown))).message())
            }
        }
    }

    /// A context for reading tables (`cx.pairs(&t)`), calling functions and so
    /// on. Scripts run from here don't trigger the hook.
    pub fn context(&mut self) -> Context<'_> {
        Context::new(self.ctx.vm())
    }
}
