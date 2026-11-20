//! Debug hooks. The host can ask to be called when execution hits a new line,
//! enters a Lua function, or returns from one, and look at the stack from
//! inside the hook (each frame's function, line, locals and upvalues).
//! Debuggers build breakpoints and stepping on top of this.
//!
//! **Cost.** The dispatch loop reads the hook mask into locals once per frame
//! switch, so with no hook each instruction pays one predictable branch on a
//! local. With a line hook each instruction also looks up its line (a binary
//! search over the source's line table), and the hook only runs when the line
//! changes or a jump goes backwards.
//!
//! **Re-entrancy.** While the hook runs it's taken out of the VM, so scripts
//! the hook calls (to evaluate something, say) don't trigger it. A hook that
//! returns `Err` stops the script with that message, and scripts can't catch
//! it.

use std::rc::Rc;

use rhue_common::SourceFile;

use crate::heap::Upvalue;
use crate::{Frame, NativeError, Thrown, Value, Vm};

/// What the hook is told about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DebugEvent {
    /// About to run the first instruction of a new line (or the same line again
    /// after a backward jump), 1-based.
    Line(u32),
    /// A Lua function got entered (natives aren't reported).
    Call,
    /// A Lua function is about to return.
    Return,
}

/// Which events the hook wants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct HookMask {
    pub line: bool,
    pub call: bool,
    pub ret: bool,
}

impl HookMask {
    /// Nothing, turn on what you want with the methods below.
    pub const fn new() -> Self {
        Self {
            line: false,
            call: false,
            ret: false,
        }
    }

    /// Tell the hook about every new line.
    pub const fn line(mut self, on: bool) -> Self {
        self.line = on;
        self
    }

    /// Tell the hook when a function gets entered.
    pub const fn call(mut self, on: bool) -> Self {
        self.call = on;
        self
    }

    /// Tell the hook when a function is about to return.
    pub const fn ret(mut self, on: bool) -> Self {
        self.ret = on;
        self
    }
}

/// The host's hook. `Err(message)` stops the script.
pub type DebugHook = Box<dyn FnMut(&mut DebugCtx<'_>, DebugEvent) -> Result<(), String>>;

/// One frame of the stack, the way a debugger shows it.
#[derive(Clone, Debug)]
pub struct FrameInfo {
    /// The function's name (`<main>` for a chunk, `<anonymous>` if it doesn't
    /// have one).
    pub function: String,
    pub source: Rc<SourceFile>,
    /// The line it's on (or for a caller, the line of the call).
    pub line: u32,
}

/// What a hook can look at.
pub struct DebugCtx<'a> {
    pub(crate) vm: &'a mut Vm,
}

#[derive(Default)]
pub(crate) struct HookState {
    pub(crate) hook: Option<DebugHook>,
    pub(crate) mask: HookMask,
    /// The last line event: (prototype, frame depth, line, pc).
    pub(crate) last_line: Option<(*const rhue_bytecode::Proto, usize, u32, usize)>,
    /// A return event already reported at (depth, pc). The instruction
    /// re-dispatches after the hook and shouldn't report again.
    pub(crate) reported: Option<(usize, usize, DebugEvent)>,
}

impl Vm {
    /// Installs a debug hook for the events in `mask` (or removes it).
    pub fn set_debug_hook(&mut self, mask: HookMask, hook: Option<DebugHook>) {
        self.hooks.mask = if hook.is_some() {
            mask
        } else {
            HookMask::default()
        };
        self.hooks.hook = hook;
        self.hooks.last_line = None;
        self.hooks.reported = None;
    }

    pub(crate) fn hook_mask(&self) -> HookMask {
        self.hooks.mask
    }

    /// Before the instruction at `pc` of `proto`, with a hook installed: the
    /// event to report there, if any (a new line, or a return).
    pub(crate) fn hook_due(
        &mut self,
        proto: &rhue_bytecode::Proto,
        pc: usize,
    ) -> Option<DebugEvent> {
        let mask = self.hooks.mask;
        if mask.line
            && let Some(line) = self.line_due(proto, pc)
        {
            return Some(DebugEvent::Line(line));
        }
        if mask.ret && proto.code.get(pc).and_then(|i| i.op()) == Some(rhue_bytecode::Op::Return) {
            let key = (self.frames.len(), pc, DebugEvent::Return);
            if self.hooks.reported == Some(key) {
                self.hooks.reported = None;
                return None;
            }
            self.hooks.reported = Some(key);
            return Some(DebugEvent::Return);
        }
        None
    }

    /// The line to report before `pc` of `proto`, if it's a new one (or a
    /// backward jump).
    fn line_due(&mut self, proto: &rhue_bytecode::Proto, pc: usize) -> Option<u32> {
        let ptr: *const rhue_bytecode::Proto = proto;
        let span = proto.spans.get(pc).copied().unwrap_or_default();
        let line = proto.source.line(span.start);
        let depth = self.frames.len();
        let fire = match self.hooks.last_line {
            Some((p, d, l, last_pc)) if p == ptr && d == depth => l != line || pc < last_pc,
            _ => true,
        };
        self.hooks.last_line = Some((ptr, depth, line, pc));
        fire.then_some(line)
    }

    /// Reports `event` (`f` parked at `pc`), then re-dispatch from `pc`.
    pub(crate) fn hook_event(
        &mut self,
        f: &mut Frame,
        pc: usize,
        event: DebugEvent,
    ) -> Result<(), Thrown> {
        self.run_hook(f, pc, event)
    }

    /// Reports entering `f`, then re-dispatch.
    pub(crate) fn call_event(&mut self, f: &mut Frame) -> Result<(), Thrown> {
        self.run_hook(f, 0, DebugEvent::Call)
    }

    /// Parks `f` (so the hook sees it on the stack, at `pc`) and runs the hook.
    fn run_hook(&mut self, f: &mut Frame, pc: usize, event: DebugEvent) -> Result<(), Thrown> {
        let Some(mut hook) = self.hooks.hook.take() else {
            return Ok(());
        };
        f.pc = pc + 1;
        let placeholder = self.placeholder_frame(f.closure);
        self.frames.push(std::mem::replace(f, placeholder));
        let result = hook(&mut DebugCtx { vm: self }, event);
        *f = self.frames.pop().expect("the hooked frame");
        f.pc = pc;
        // the hook might've replaced itself, keep the new one if so
        if self.hooks.hook.is_none() {
            self.hooks.hook = Some(hook);
        }
        result.map_err(|m| {
            let mut t = self.native_throw(f, pc, NativeError::Message(m));
            t.catchable = false;
            t
        })
    }
}

impl DebugCtx<'_> {
    /// The VM (to display values, read tables, ...).
    pub fn vm(&mut self) -> &mut Vm {
        self.vm
    }

    /// Lua frames on the running thread's stack, innermost first.
    fn frames(&self) -> impl Iterator<Item = &Frame> {
        self.vm
            .frames
            .iter()
            .rev()
            .filter(|f| !Rc::ptr_eq(&f.cl, &self.vm.placeholder))
    }

    /// How many Lua frames are on the stack.
    pub fn depth(&self) -> usize {
        self.frames().count()
    }

    /// Frame `level` (0 = the one executing).
    pub fn frame(&self, level: usize) -> Option<FrameInfo> {
        let f = self.frames().nth(level)?;
        let pc = f.pc.saturating_sub(1);
        let span = f.cl.proto.spans.get(pc).copied().unwrap_or_default();
        Some(FrameInfo {
            function: if f.is_main {
                "<main>".to_string()
            } else {
                f.cl.proto.display_name().to_string()
            },
            source: f.cl.proto.source.clone(),
            line: f.cl.proto.source.line(span.start),
        })
    }

    /// The locals in scope in frame `level`, in declaration order (a local
    /// shadowed by a later one with the same name gets left out).
    #[expect(
        clippy::cast_possible_truncation,
        reason = "pcs stay far under u32, code is capped"
    )]
    pub fn locals(&self, level: usize) -> Vec<(Rc<str>, Value)> {
        let Some(f) = self.frames().nth(level) else {
            return Vec::new();
        };
        let pc = f.pc.saturating_sub(1) as u32;
        let live: Vec<_> =
            f.cl.proto
                .locals
                .iter()
                .filter(|l| l.start_pc <= pc && pc < l.end_pc)
                .collect();
        live.iter()
            .enumerate()
            .filter(|(i, l)| !live[i + 1..].iter().any(|m| m.name == l.name))
            .map(|(_, l)| {
                let v = self
                    .vm
                    .stack
                    .get(f.base + l.reg as usize)
                    .copied()
                    .unwrap_or(Value::Nil);
                (l.name.clone(), v)
            })
            .collect()
    }

    /// The upvalues of frame `level`'s function, by name.
    pub fn upvalues(&self, level: usize) -> Vec<(Rc<str>, Value)> {
        let Some(f) = self.frames().nth(level) else {
            return Vec::new();
        };
        f.cl.proto
            .upvalues
            .iter()
            .zip(f.cl.upvalues.iter())
            .map(|(desc, u)| {
                let v = match u.get() {
                    Upvalue::Closed(v) => v,
                    Upvalue::Open { index, thread } if thread == self.vm.current => {
                        self.vm.stack[index as usize]
                    }
                    Upvalue::Open { index, thread } => {
                        self.vm.heap.thread(thread).state.stack[index as usize]
                    }
                };
                (desc.name.clone(), v)
            })
            .collect()
    }
}
