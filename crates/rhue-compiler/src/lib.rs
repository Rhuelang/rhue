//! Compiles the Rhue AST down to register bytecode.
//!
//! This is part of [Rhue](https://docs.rs/rhue)'s guts. If you're embedding
//! Rhue, use the `rhue` crate, that's the API that stays stable.
//!
//! Pipeline: [`resolve`] binds names, [`fold`] folds constants (per expression
//! root), then one codegen pass emits instructions. Each function gets its own
//! `FuncState` and the enclosing ones wait on a stack, which is how upvalue
//! chains get threaded through the functions in between.
//!
//! **Registers.** Locals live at the bottom of the register window in the order
//! they're declared. Temporaries get pushed above them and popped after use
//! (`free_reg`), like Lua. `locals_top` marks where the locals end, so a
//! local's register never gets mistaken for a throwaway temporary.
//!
//! **Jumps.** Conditions compile to *jump lists*. `cond_jump(e, when)` emits
//! code that jumps if `e`'s truthiness equals `when` and falls through
//! otherwise, and hands back the jumps to patch. `and`/`or`/`not` combine these
//! lists, so conditions short-circuit without ever making a boolean.

mod fold;
pub mod resolve;

use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

use rhue_bytecode::{
    ArithOp, Constant, Instr, MAX_TEMPLATE_SIZE, Op, Proto, TableTemplate, UpvalDesc,
};
use rhue_common::{Diagnostic, SourceFile, Span};
use rhue_parser::ast::*;

pub use fold::{fold, fold_number};
use resolve::{Binding, LocalId, Resolution};

/// Registers are addressed by a `u8`, keep a little headroom for calls.
pub const MAX_REGISTERS: u32 = 250;

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct CompileOptions {
    /// Evaluate constant arithmetic at compile time (Luau does this at `-O1`).
    pub fold_constants: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl CompileOptions {
    /// The defaults, with constant folding on.
    pub const fn new() -> Self {
        Self {
            fold_constants: true,
        }
    }

    /// Turns constant folding on or off.
    pub const fn fold_constants(mut self, on: bool) -> Self {
        self.fold_constants = on;
        self
    }
}

/// Resolves and compiles a chunk into its main function prototype.
///
/// `source` gets attached to every prototype so runtime errors render against
/// the right text.
pub fn compile_chunk(
    chunk: &Chunk,
    source: &Rc<SourceFile>,
    options: CompileOptions,
) -> Result<Proto, Vec<Diagnostic>> {
    let resolution = resolve::resolve(chunk)?;
    compile_resolved(chunk, &resolution, source, options)
}

/// Compiles a chunk [`resolve::resolve`] already resolved (the type checker
/// needs the resolution too).
pub fn compile_resolved(
    chunk: &Chunk,
    resolution: &resolve::Resolution,
    source: &Rc<SourceFile>,
    options: CompileOptions,
) -> Result<Proto, Vec<Diagnostic>> {
    let mut main = FuncState::default();
    main.proto.is_vararg = true;
    main.proto.span = chunk.span;
    main.proto.source = source.clone();
    let mut c = Compiler {
        options,
        res: resolution,
        fs: main,
        parents: Vec::new(),
    };
    let end = Span::new(chunk.span.end, chunk.span.end);
    c.block(&chunk.body).map_err(|d| vec![*d])?;
    // falling off the end of a chunk returns nothing
    c.emit(Instr::abc(Op::Return, 0, 0, 0), end);
    c.fs.finish_debug();
    let proto = c.fs.proto;
    // the compiler's output always verifies. if it doesn't that's a bug in
    // here, so report it as an error instead of leaving it for the VM to trip
    // on
    if let Err(e) = rhue_bytecode::verify_once(&proto) {
        if cfg!(debug_assertions) {
            panic!("the compiler produced bytecode that doesn't verify: {e}");
        }
        return Err(vec![*error(
            "internal compiler error",
            chunk.span,
            e.to_string(),
        )]);
    }
    Ok(proto)
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum ConstKey {
    /// Keyed by bits, so `0` and `-0` stay different.
    Number(u64),
    String(Rc<str>),
}

struct LoopState {
    breaks: Vec<usize>,
    continues: Vec<usize>,
    /// Registers at or above these hold locals declared inside the loop.
    /// Leaving through `break`/`continue` has to close any that got captured.
    break_close: u32,
    continue_close: u32,
}

/// Everything the compiler keeps track of for one function.
#[derive(Default)]
struct FuncState {
    proto: Proto,
    free_reg: u32,
    locals_top: u32,
    /// Registers of this function's locals (by resolver id).
    local_regs: HashMap<LocalId, u8>,
    /// Locals in scope right now, innermost last: `(id, register)`.
    active: Vec<(LocalId, u8)>,
    /// For each of `active`, its entry in `proto.locals`.
    active_debug: Vec<usize>,
    constants: HashMap<ConstKey, u16>,
    loops: Vec<LoopState>,
    /// Upvalues by the local they end up pointing at.
    upvalue_ids: Vec<LocalId>,
}

impl FuncState {
    /// Locals still in scope at the end of the function live until its end.
    fn finish_debug(&mut self) {
        let end = self.proto.code.len() as u32;
        for l in &mut self.proto.locals {
            l.end_pc = l.end_pc.min(end);
        }
    }
}

/// Where a variable lives, from the current function's point of view.
#[derive(Clone, Copy)]
enum Var {
    Register(u8),
    Upvalue(u8),
    Global,
}

struct Compiler<'r> {
    options: CompileOptions,
    res: &'r Resolution,
    /// The function being compiled right now.
    fs: FuncState,
    /// Enclosing functions, outermost first.
    parents: Vec<FuncState>,
}

/// Where an assignment stores its value.
#[derive(Clone, Copy)]
enum Place {
    Register(u8),
    Upvalue(u8),
    /// Constant index of the global's name.
    Global(u16),
    Index {
        obj: u8,
        key: Key,
    },
}

#[derive(Clone, Copy)]
enum Key {
    /// Constant index of a string key (`t.name`).
    Const(u8),
    Register(u8),
}

/// How many values an expression list should give.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Exactly(u32),
    /// Every value, including all the results of a trailing call or `...`
    /// (which then sets the VM's `top`).
    All,
}

/// Boxed so `Result`s stay small on the hot (successful) path.
type CResult<T = ()> = Result<T, Box<Diagnostic>>;

fn error(message: impl Into<String>, span: Span, label: impl Into<String>) -> Box<Diagnostic> {
    Box::new(Diagnostic::error(message, span).with_label(label))
}

/// A register holding an operand. `restore` is the `free_reg` to go back to
/// once the instruction using it is emitted.
struct Operand {
    reg: u8,
    restore: u32,
}

impl Compiler<'_> {
    // ─ Emission, Registers, Constants ────────────────────────────────────────

    fn pc(&self) -> usize {
        self.fs.proto.code.len()
    }

    fn emit(&mut self, instr: Instr, span: Span) -> usize {
        self.fs.proto.code.push(instr);
        self.fs.proto.spans.push(span);
        self.fs.proto.code.len() - 1
    }

    fn emit_jump(&mut self, span: Span) -> usize {
        self.emit(Instr::asax(Op::Jmp, 0), span)
    }

    /// Points the jump at `pc` to `target`.
    fn patch(&mut self, pc: usize, target: usize) -> CResult {
        let offset = target as i64 - (pc as i64 + 1);
        let ins = self.fs.proto.code[pc];
        let op = ins.op().expect("compiler only emits valid opcodes");
        let patched = match op {
            Op::Jmp => i32::try_from(offset)
                .ok()
                .filter(|o| o.abs() <= Instr::MAX_SAX)
                .map(|o| Instr::asax(Op::Jmp, o)),
            Op::JmpIf | Op::JmpIfNot | Op::ForPrep | Op::ForLoop | Op::TForLoop => {
                i16::try_from(offset)
                    .ok()
                    .map(|o| Instr::asbx(op, ins.a(), o))
            }
            _ => unreachable!("patching a non-jump {op:?}"),
        };
        match patched {
            Some(i) => {
                self.fs.proto.code[pc] = i;
                Ok(())
            }
            None => Err(error(
                "control structure is too large",
                self.fs.proto.spans[pc],
                "this jump would cross too many instructions",
            )),
        }
    }

    fn patch_here(&mut self, jumps: Vec<usize>) -> CResult {
        let here = self.pc();
        jumps.into_iter().try_for_each(|j| self.patch(j, here))
    }

    fn alloc_reg(&mut self, span: Span) -> CResult<u8> {
        if self.fs.free_reg >= MAX_REGISTERS {
            return Err(error(
                "function is too complex",
                span,
                format!("needs more than {MAX_REGISTERS} registers"),
            )
            .map_help("split up long expressions or reduce the number of locals in scope"));
        }
        let reg = self.fs.free_reg as u8;
        self.fs.free_reg += 1;
        self.fs.proto.max_stack = self.fs.proto.max_stack.max(self.fs.free_reg as u8);
        Ok(reg)
    }

    fn free_to(&mut self, reg: u32) {
        debug_assert!(reg <= self.fs.free_reg && reg >= self.fs.locals_top);
        self.fs.free_reg = reg;
    }

    /// The top temporary can hold intermediate results since nothing else reads
    /// it. A local's register never counts.
    fn is_top_temp(&self, reg: u8) -> bool {
        reg as u32 + 1 == self.fs.free_reg && reg as u32 >= self.fs.locals_top
    }

    fn constant(&mut self, key: ConstKey, value: Constant, span: Span) -> CResult<u16> {
        if let Some(&k) = self.fs.constants.get(&key) {
            return Ok(k);
        }
        let k = u16::try_from(self.fs.proto.constants.len()).map_err(|_| {
            error(
                "too many constants in one function",
                span,
                "the limit is 65536",
            )
        })?;
        self.fs.proto.constants.push(value);
        self.fs.constants.insert(key, k);
        Ok(k)
    }

    fn number_constant(&mut self, n: f64, span: Span) -> CResult<u16> {
        self.constant(ConstKey::Number(n.to_bits()), Constant::Number(n), span)
    }

    fn string_constant(&mut self, s: &Rc<str>, span: Span) -> CResult<u16> {
        self.constant(
            ConstKey::String(s.clone()),
            Constant::String(s.clone()),
            span,
        )
    }

    /// Folds an expression root, if folding is on.
    fn root<'e>(&self, expr: &'e Expr) -> Cow<'e, Expr> {
        if self.options.fold_constants {
            Cow::Owned(fold(expr))
        } else {
            Cow::Borrowed(expr)
        }
    }

    /// Where `name` lives: a register of this function, an upvalue (made on
    /// first use and threaded through every enclosing function), or a global.
    fn var(&mut self, name: &Name) -> CResult<Var> {
        let Binding::Local(id) = self.res.binding(name) else {
            return Ok(Var::Global);
        };
        if let Some(&reg) = self.fs.local_regs.get(&id) {
            return Ok(Var::Register(reg));
        }
        match upvalue_index(&mut self.parents, &mut self.fs, id, self.res) {
            Some(Ok(i)) => Ok(Var::Upvalue(i)),
            Some(Err(())) => Err(error(
                "function uses too many outer variables",
                name.span,
                "the limit is 255 upvalues per function",
            )),
            None => unreachable!(
                "resolver bound `{}` to a local that no enclosing function declares",
                name.text
            ),
        }
    }

    /// Adds a new local to the current scope.
    fn declare_local(&mut self, name: &Name, reg: u8) {
        let Binding::Local(id) = self.res.binding(name) else {
            unreachable!("declarations are locals")
        };
        self.fs.local_regs.insert(id, reg);
        self.fs.active.push((id, reg));
        self.fs.active_debug.push(self.fs.proto.locals.len());
        self.fs.proto.locals.push(rhue_bytecode::LocalVar {
            name: name.text.clone(),
            reg,
            start_pc: self.fs.proto.code.len() as u32,
            end_pc: u32::MAX,
        });
    }

    /// The lowest register among captured locals in `active[from..]`.
    fn captured_from(&self, from: usize) -> Option<u8> {
        self.fs.active[from..]
            .iter()
            .filter(|(id, _)| self.res.is_captured(*id))
            .map(|&(_, r)| r)
            .min()
    }

    /// Emits `CLOSE` for registers at or above `base` if any captured local
    /// lives there.
    fn close_from_reg(&mut self, base: u32, span: Span) {
        let captured = self
            .fs
            .active
            .iter()
            .any(|&(id, r)| r as u32 >= base && self.res.is_captured(id));
        if captured {
            self.emit(Instr::abc(Op::Close, base as u8, 0, 0), span);
        }
    }

    // ─ Blocks & Statements ───────────────────────────────────────────────────

    fn block(&mut self, block: &Block) -> CResult {
        self.scoped(|c| c.block_body(block))
    }

    fn scoped(&mut self, f: impl FnOnce(&mut Self) -> CResult) -> CResult {
        let (free, top, active) = (self.fs.free_reg, self.fs.locals_top, self.fs.active.len());
        let result = f(self);
        // locals captured by closures have to be closed when their scope ends
        if result.is_ok()
            && let Some(reg) = self.captured_from(active)
        {
            let span = self.fs.proto.spans.last().copied().unwrap_or_default();
            self.emit(Instr::abc(Op::Close, reg, 0, 0), span);
        }
        self.fs.active.truncate(active);
        let end = self.fs.proto.code.len() as u32;
        for i in self.fs.active_debug.drain(active..) {
            self.fs.proto.locals[i].end_pc = end;
        }
        self.fs.locals_top = top;
        self.fs.free_reg = free;
        result
    }

    fn block_body(&mut self, block: &Block) -> CResult {
        for stmt in &block.stmts {
            self.stmt(stmt)?;
            debug_assert_eq!(
                self.fs.free_reg, self.fs.locals_top,
                "temporaries leaked past a statement"
            );
        }
        if let Some(ret) = &block.ret {
            self.return_values(&ret.values, ret.span)?;
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> CResult {
        stacker::maybe_grow(64 * 1024, 1024 * 1024, || self.stmt_inner(stmt))
    }

    fn stmt_inner(&mut self, stmt: &Stmt) -> CResult {
        match &stmt.kind {
            StmtKind::Local { names, values, .. } => self.local(names, values),
            // types get erased
            StmtKind::TypeAlias(_) => Ok(()),
            StmtKind::Assign { targets, values } => self.assign(targets, values),
            StmtKind::CompoundAssign {
                op,
                op_span,
                target,
                value,
            } => {
                // `x op= e` is `x = x op (e)`. `x` is a name, so evaluating it
                // twice can't be noticed
                let combined = Expr {
                    span: stmt.span,
                    kind: ExprKind::Binary {
                        op: *op,
                        op_span: *op_span,
                        lhs: Box::new(target.clone()),
                        rhs: Box::new(Expr {
                            span: value.span,
                            kind: ExprKind::Group(Box::new(value.clone())),
                        }),
                    },
                };
                if let ExprKind::Index { .. } = target.kind {
                    return self.compound_index(*op, target, value, stmt.span);
                }
                let combined = self.root(&combined).into_owned();
                self.assign_one(target, &combined)
            }
            StmtKind::Call(call) => {
                let base = self.alloc_reg(call.span)?;
                self.multi_into(call, base, Some(0))?;
                self.free_to(base as u32);
                Ok(())
            }
            StmtKind::Function { target, func } => {
                let tmp = self.alloc_reg(func.span)?;
                self.function(func, tmp)?;
                let place = self.place(target, false)?;
                self.store_place(&place, tmp, target.span)?;
                self.free_to(tmp as u32);
                Ok(())
            }
            StmtKind::GenericFor {
                names, exprs, body, ..
            } => self.generic_for(names, exprs, body, stmt.span),
            StmtKind::LocalFunction { name, func } => {
                // declare it first so the body can refer to itself (as an
                // upvalue)
                let reg = self.alloc_reg(name.span)?;
                self.fs.locals_top = self.fs.free_reg;
                self.declare_local(name, reg);
                self.function(func, reg)
            }
            StmtKind::Do(body) => self.block(body),
            StmtKind::If {
                branches,
                else_block,
            } => self.if_stmt(branches, else_block.as_ref()),
            StmtKind::While { cond, body } => self.while_stmt(cond, body),
            StmtKind::Repeat { body, cond } => self.repeat_stmt(body, cond),
            StmtKind::NumericFor {
                var,
                start,
                limit,
                step,
                body,
                ..
            } => self.numeric_for(var, start, limit, step.as_ref(), body, stmt.span),
            StmtKind::Break => {
                let base = self
                    .fs
                    .loops
                    .last()
                    .expect("resolver checked `break` is in a loop")
                    .break_close;
                self.close_from_reg(base, stmt.span);
                let j = self.emit_jump(stmt.span);
                self.fs
                    .loops
                    .last_mut()
                    .expect("checked above")
                    .breaks
                    .push(j);
                Ok(())
            }
            StmtKind::Continue => {
                let base = self
                    .fs
                    .loops
                    .last()
                    .expect("resolver checked `continue` is in a loop")
                    .continue_close;
                self.close_from_reg(base, stmt.span);
                let j = self.emit_jump(stmt.span);
                self.fs
                    .loops
                    .last_mut()
                    .expect("checked above")
                    .continues
                    .push(j);
                Ok(())
            }
        }
    }

    fn local(&mut self, names: &[Name], values: &[Expr]) -> CResult {
        let base = self.fs.free_reg;
        let span = names[0].span;
        self.explist(values, Want::Exactly(names.len() as u32), span)?;
        self.fs.free_reg = base + names.len() as u32;
        self.fs.locals_top = self.fs.free_reg;
        for (i, name) in names.iter().enumerate() {
            self.declare_local(name, (base + i as u32) as u8);
        }
        Ok(())
    }

    /// Compiles `values` into consecutive registers starting at `free_reg`,
    /// allocating them. With `Want::Exactly(n)` exactly `n` registers end up
    /// allocated (missing values are nil, extras get evaluated and dropped).
    /// With `Want::All` a trailing call or `...` keeps all its results, and
    /// whatever uses them reads up to `top`. Returns the first register.
    fn explist(&mut self, values: &[Expr], want: Want, span: Span) -> CResult<u8> {
        let base = self.fs.free_reg;
        let n = values.len() as u32;
        for (i, value) in values.iter().enumerate() {
            let i = i as u32;
            let is_last = i + 1 == n;
            let value = self.root(value);
            if is_last && value.kind.is_multi() {
                let results = match want {
                    Want::All => None,
                    // `local a, b, c = f()` asks `f` for 3 results
                    Want::Exactly(w) => Some(w.saturating_sub(i)),
                };
                if results == Some(0) {
                    // extra value, still evaluated for its side effects
                    let reg = self.alloc_reg(value.span)?;
                    self.multi_into(&value, reg, Some(0))?;
                    self.free_to(reg as u32);
                } else {
                    let reg = self.alloc_reg(value.span)?;
                    self.multi_into(&value, reg, results)?;
                    if let Some(r) = results {
                        // the results take up reg..reg+r
                        for _ in 1..r {
                            self.alloc_reg(value.span)?;
                        }
                    }
                }
                continue;
            }
            let reg = self.alloc_reg(value.span)?;
            self.expr_into(&value, reg)?;
        }
        if let Want::Exactly(w) = want {
            // pad with nil, or drop extras
            while self.fs.free_reg < base + w {
                let reg = self.alloc_reg(span)?;
                self.emit(Instr::abc(Op::LoadNil, reg, 0, 0), span);
            }
            self.fs.free_reg = base + w;
        }
        Ok(base as u8)
    }

    /// Compiles a call, method call or `...` at register `base` (has to be the
    /// top of the stack), asking for `results` values (`None` = all of them).
    fn multi_into(&mut self, expr: &Expr, base: u8, results: Option<u32>) -> CResult {
        match &expr.kind {
            ExprKind::Call { callee, args } => self.call(callee, args, base, results, expr.span),
            ExprKind::MethodCall {
                object,
                method,
                method_span,
                args,
            } => self.method_call(object, method, *method_span, args, base, results, expr.span),
            ExprKind::Vararg => {
                let b = match results {
                    None => 0,
                    Some(r) => u8::try_from(r + 1)
                        .map_err(|_| error("too many values", expr.span, "the limit is 254"))?,
                };
                self.emit(Instr::abc(Op::Vararg, base, b, 0), expr.span);
                Ok(())
            }
            _ => unreachable!("only calls and `...` are multi-valued"),
        }
    }

    /// `obj:m(args)`. `SELF` puts `obj.m` at `base` and `obj` at `base + 1`,
    /// the arguments follow, and `CALL` passes `obj` as `self`.
    #[allow(clippy::too_many_arguments)]
    fn method_call(
        &mut self,
        object: &Expr,
        method: &Rc<str>,
        method_span: Span,
        args: &[Expr],
        base: u8,
        results: Option<u32>,
        span: Span,
    ) -> CResult {
        debug_assert!(
            self.is_top_temp(base),
            "a call's base must be the top register"
        );
        let self_reg = self.alloc_reg(span)?;
        let object = self.root(object);
        let obj = self.operand(&object, Some(self_reg))?;
        let k = self.string_constant(method, method_span)?;
        match u8::try_from(k) {
            Ok(k) => {
                self.emit(Instr::abc(Op::SelfOp, base, obj.reg, k), span);
            }
            Err(_) => {
                // rare, more than 255 constants. do SELF's job by hand
                if obj.reg != self_reg {
                    self.emit(Instr::abc(Op::Move, self_reg, obj.reg, 0), span);
                }
                self.emit(Instr::abx(Op::LoadK, base, k), method_span);
                self.emit(Instr::abc(Op::GetTable, base, self_reg, base), span);
            }
        }
        self.free_to(self_reg as u32 + 1);
        let multi_tail = args.last().is_some_and(|a| a.kind.is_multi());
        self.explist(
            args,
            if multi_tail {
                Want::All
            } else {
                Want::Exactly(args.len() as u32)
            },
            span,
        )?;
        let b = if multi_tail {
            0
        } else {
            u8::try_from(args.len() + 2)
                .map_err(|_| error("too many arguments", span, "the limit is 253"))?
        };
        let c = match results {
            None => 0,
            Some(r) => u8::try_from(r + 1)
                .map_err(|_| error("too many results requested", span, "the limit is 254"))?,
        };
        self.emit(Instr::abc(Op::Call, base, b, c), span);
        self.free_to(base as u32 + 1);
        Ok(())
    }

    /// `{ ... }` into `base` (the top register). Positional values get gathered
    /// above the table and flushed 50 at a time with `SETLIST`, keyed fields
    /// get stored as they come, so source order is kept.
    ///
    /// When fields have constant string keys, the `NEWTABLE` turns into a
    /// `DUPTABLE` whose template already has those keys, so each `SETFIELD`
    /// fills a slot instead of adding one. The template stops at the first
    /// computed key so keys still go in in source order.
    fn table_constructor(&mut self, items: &[TableItem], base: u8, span: Span) -> CResult {
        const FLUSH: u32 = 50;
        let positional = items
            .iter()
            .filter(|i| matches!(i, TableItem::Positional(_)))
            .count();
        let keyed = items.len() - positional;
        let hint = |n: usize| n.min(255) as u8;
        let new_table = self.fs.proto.code.len();
        self.emit(
            Instr::abc(Op::NewTable, base, hint(positional), hint(keyed)),
            span,
        );
        let mut template: Vec<u32> = Vec::new();
        let mut templating = true;
        let mut stored: u32 = 0;
        let mut pending: u32 = 0;
        for (i, item) in items.iter().enumerate() {
            match item {
                TableItem::Positional(value) => {
                    let value = self.root(value);
                    let is_last = i + 1 == items.len();
                    let reg = self.alloc_reg(value.span)?;
                    if is_last && value.kind.is_multi() {
                        self.multi_into(&value, reg, None)?;
                        self.set_list(base, 0, stored, value.span)?;
                        pending = 0;
                        self.free_to(base as u32 + 1);
                        continue;
                    }
                    self.expr_into(&value, reg)?;
                    pending += 1;
                    if pending == FLUSH {
                        self.set_list(base, pending, stored, value.span)?;
                        stored += pending;
                        pending = 0;
                        self.free_to(base as u32 + 1);
                    }
                }
                TableItem::Keyed { key, value } => {
                    let mark = self.fs.free_reg;
                    let key = self.root(key);
                    let value = self.root(value);
                    match self.field_constant(&key)? {
                        Some(k) => {
                            // a nil field stays out, it isn't stored so the key
                            // goes in when it's first assigned
                            let is_nil = matches!(value.kind, ExprKind::Nil);
                            if templating && !is_nil && !template.contains(&(k as u32)) {
                                template.push(k as u32);
                            }
                            let v = self.operand(&value, None)?;
                            self.emit(Instr::abc(Op::SetField, base, k, v.reg), value.span);
                        }
                        None => {
                            templating = false;
                            let k = self.operand(&key, None)?;
                            let v = self.operand(&value, None)?;
                            self.emit(Instr::abc(Op::SetTable, base, k.reg, v.reg), value.span);
                        }
                    }
                    self.free_to(mark);
                }
            }
        }
        if pending > 0 {
            self.set_list(base, pending, stored, span)?;
        }
        if !template.is_empty()
            && let Ok(t) = u16::try_from(self.fs.proto.templates.len())
        {
            let size = |n: usize| n.min(MAX_TEMPLATE_SIZE as usize) as u32;
            self.fs.proto.templates.push(TableTemplate {
                array: size(positional),
                hash: size(keyed),
                keys: template,
            });
            self.fs.proto.code[new_table] = Instr::abx(Op::DupTable, base, t);
        }
        self.free_to(base as u32 + 1);
        Ok(())
    }

    fn set_list(&mut self, table: u8, count: u32, already: u32, span: Span) -> CResult {
        let start = i32::try_from(already)
            .ok()
            .filter(|&s| s <= Instr::MAX_SAX)
            .ok_or_else(|| {
                error(
                    "table constructor is too large",
                    span,
                    "too many positional values",
                )
            })?;
        self.emit(Instr::abc(Op::SetList, table, count as u8, 0), span);
        self.emit(Instr::asax(Op::Data, start), span);
        Ok(())
    }

    /// `for a, b in explist do ... end`. Three hidden registers (iterator,
    /// state, control) sit below the variables. `TFORCALL` calls the iterator
    /// with its arguments copied into the variables' registers, so that
    /// region's at least three registers wide.
    fn generic_for(&mut self, names: &[Name], exprs: &[Expr], body: &Block, span: Span) -> CResult {
        self.scoped(|c| {
            let base = c.explist(exprs, Want::Exactly(3), span)?;
            let var_base = base as u32 + 3;
            let region = (names.len() as u32).max(3);
            for _ in 0..region {
                c.alloc_reg(span)?;
            }
            c.fs.locals_top = c.fs.free_reg;
            for (i, name) in names.iter().enumerate() {
                c.declare_local(name, (var_base + i as u32) as u8);
            }
            let nvars = u8::try_from(names.len())
                .map_err(|_| error("too many loop variables", span, "the limit is 255"))?;
            let to_call = c.emit_jump(span);
            let body_start = c.pc();
            let state = c.in_loop(var_base, var_base + region, |c| c.block(body))?;
            c.patch_here(state.continues)?;
            c.patch_here(vec![to_call])?;
            // fresh variables every iteration, close captured ones
            let captured = names
                .iter()
                .any(|n| matches!(c.res.binding(n), Binding::Local(id) if c.res.is_captured(id)));
            if captured {
                c.emit(Instr::abc(Op::Close, var_base as u8, 0, 0), span);
            }
            c.emit(Instr::abc(Op::TForCall, base, 0, nvars), span);
            let back = c.emit(Instr::asbx(Op::TForLoop, base, 0), span);
            c.patch(back, body_start)?;
            c.patch_here(state.breaks)
        })
    }

    /// `R(base) := callee`, arguments above it, then `CALL`. Results land at
    /// `base`, and `free_reg` is left at `base + 1`.
    fn call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        base: u8,
        results: Option<u32>,
        span: Span,
    ) -> CResult {
        debug_assert!(
            self.is_top_temp(base),
            "a call's base must be the top register"
        );
        let callee = self.root(callee);
        self.expr_into(&callee, base)?;
        let multi_tail = args.last().is_some_and(|a| a.kind.is_multi());
        self.explist(
            args,
            if multi_tail {
                Want::All
            } else {
                Want::Exactly(args.len() as u32)
            },
            span,
        )?;
        let b = if multi_tail {
            0
        } else {
            u8::try_from(args.len() + 1)
                .map_err(|_| error("too many arguments", span, "the limit is 254"))?
        };
        let c = match results {
            None => 0,
            Some(r) => u8::try_from(r + 1)
                .map_err(|_| error("too many results requested", span, "the limit is 254"))?,
        };
        self.emit(Instr::abc(Op::Call, base, b, c), span);
        self.free_to(base as u32 + 1);
        Ok(())
    }

    /// Compiles a function body into a new prototype and emits `CLOSURE`.
    fn function(&mut self, func: &FunctionBody, target: u8) -> CResult {
        let mut child = FuncState::default();
        child.proto.name = func.name.clone();
        child.proto.span = func.span;
        child.proto.source = self.fs.proto.source.clone();
        child.proto.is_vararg = func.is_vararg;
        child.proto.num_params = u8::try_from(func.params.len())
            .map_err(|_| error("too many parameters", func.span, "the limit is 255"))?;
        let parent = std::mem::replace(&mut self.fs, child);
        self.parents.push(parent);
        let result = (|| -> CResult {
            for p in &func.params {
                let reg = self.alloc_reg(p.span)?;
                self.declare_local(p, reg);
            }
            self.fs.locals_top = self.fs.free_reg;
            self.block_body(&func.body)?;
            let end = Span::new(func.span.end, func.span.end);
            self.emit(Instr::abc(Op::Return, 0, 0, 0), end);
            Ok(())
        })();
        let parent = self.parents.pop().expect("pushed above");
        let mut child = std::mem::replace(&mut self.fs, parent);
        result?;
        child.finish_debug();
        let index = u16::try_from(self.fs.proto.protos.len()).map_err(|_| {
            error(
                "too many nested functions",
                func.span,
                "the limit is 65536 per function",
            )
        })?;
        self.fs.proto.protos.push(Rc::new(child.proto));
        self.emit(Instr::abx(Op::Closure, target, index), func.span);
        Ok(())
    }

    fn assign(&mut self, targets: &[Expr], values: &[Expr]) -> CResult {
        if let ([target], [value]) = (targets, values) {
            let value = self.root(value);
            if let ExprKind::Name(_) = target.kind {
                return self.assign_one(target, &value);
            }
            // `t.k = v`: object and key first, then the value
            let base = self.fs.free_reg;
            let place = self.place(target, false)?;
            let v = self.operand(&value, None)?;
            self.store_place(&place, v.reg, target.span)?;
            self.free_to(base);
            return Ok(());
        }
        // multiple assignment: evaluate every target's object and key, then
        // every value, and only then store. that way `a, b = b, a` swaps and
        // `i, t[i] = i + 1, 20` indexes with the *old* `i`
        let base = self.fs.free_reg;
        let mut places = Vec::with_capacity(targets.len());
        for target in targets {
            places.push(self.place(target, true)?);
        }
        let vbase =
            self.explist(values, Want::Exactly(targets.len() as u32), targets[0].span)? as u32;
        for (i, (place, target)) in places.iter().zip(targets).enumerate() {
            self.store_place(place, (vbase + i as u32) as u8, target.span)?;
        }
        self.free_to(base);
        Ok(())
    }

    /// Resolves an assignment target. For `t[k]` it evaluates `t` and `k` into
    /// registers that stay allocated until the caller frees them. With
    /// `copy_locals` even a local's register gets copied, so a later store to
    /// that local in the same statement can't change which slot gets written.
    fn place(&mut self, target: &Expr, copy_locals: bool) -> CResult<Place> {
        match &target.kind {
            ExprKind::Name(name) => Ok(match self.var(name)? {
                Var::Register(r) => Place::Register(r),
                Var::Upvalue(u) => Place::Upvalue(u),
                Var::Global => Place::Global(self.string_constant(&name.text, name.span)?),
            }),
            ExprKind::Index { object, key } => {
                let object = self.root(object);
                let key = self.root(key);
                let obj = self.place_operand(&object, copy_locals)?;
                let key = match self.field_constant(&key)? {
                    Some(k) => Key::Const(k),
                    None => Key::Register(self.place_operand(&key, copy_locals)?),
                };
                Ok(Place::Index { obj, key })
            }
            _ => unreachable!("parser only allows names and indexes as targets"),
        }
    }

    fn place_operand(&mut self, expr: &Expr, copy_locals: bool) -> CResult<u8> {
        if copy_locals {
            let reg = self.alloc_reg(expr.span)?;
            self.expr_into(expr, reg)?;
            Ok(reg)
        } else {
            Ok(self.operand(expr, None)?.reg)
        }
    }

    /// The constant index for `t.name` / `t["name"]`, if it fits in an 8-bit
    /// operand.
    fn field_constant(&mut self, key: &Expr) -> CResult<Option<u8>> {
        if let ExprKind::String(s) = &key.kind {
            let k = self.string_constant(s, key.span)?;
            return Ok(u8::try_from(k).ok());
        }
        Ok(None)
    }

    fn store_place(&mut self, place: &Place, src: u8, span: Span) -> CResult {
        match *place {
            Place::Register(reg) => {
                if reg != src {
                    self.emit(Instr::abc(Op::Move, reg, src, 0), span);
                }
            }
            Place::Upvalue(u) => {
                self.emit(Instr::abc(Op::SetUpval, src, u, 0), span);
            }
            Place::Global(k) => {
                self.emit(Instr::abx(Op::SetGlobal, src, k), span);
            }
            Place::Index {
                obj,
                key: Key::Const(k),
            } => {
                self.emit(Instr::abc(Op::SetField, obj, k, src), span);
            }
            Place::Index {
                obj,
                key: Key::Register(k),
            } => {
                self.emit(Instr::abc(Op::SetTable, obj, k, src), span);
            }
        }
        Ok(())
    }

    /// `t[k] op= v`. `t` and `k` only get evaluated once (Luau semantics), so
    /// `t[next_index()] += 1` calls `next_index` once.
    fn compound_index(&mut self, op: BinOp, target: &Expr, value: &Expr, span: Span) -> CResult {
        let base = self.fs.free_reg;
        let place = self.place(target, false)?;
        let Place::Index { obj, key } = place else {
            unreachable!("called for index targets")
        };
        let tmp = self.alloc_reg(span)?;
        match key {
            Key::Const(k) => self.emit(Instr::abc(Op::GetField, tmp, obj, k), span),
            Key::Register(k) => self.emit(Instr::abc(Op::GetTable, tmp, obj, k), span),
        };
        let value = self.root(value);
        match (op, fold::arith_op(op), &value.kind) {
            (BinOp::Concat, _, _) => {
                let v = self.operand(&value, None)?;
                self.emit(Instr::abc(Op::Concat, tmp, tmp, v.reg), span);
            }
            (_, Some(aop), ExprKind::Number(n)) if self.number_constant(*n, value.span)? <= 255 => {
                let k = self.number_constant(*n, value.span)? as u8;
                self.emit(Instr::abc(Op::arith_rk(aop), tmp, tmp, k), span);
            }
            (_, Some(aop), _) => {
                let v = self.operand(&value, None)?;
                self.emit(Instr::abc(Op::arith_rr(aop), tmp, tmp, v.reg), span);
            }
            _ => unreachable!("compound operators are arithmetic or `..`"),
        }
        self.store_place(&place, tmp, span)?;
        self.free_to(base);
        Ok(())
    }

    fn assign_one(&mut self, target: &Expr, value: &Expr) -> CResult {
        let ExprKind::Name(name) = &target.kind else {
            unreachable!("parser only allows names")
        };
        match self.var(name)? {
            // compile straight into the local when the value only gets written
            // after all its operands are read. otherwise (`x = a and x`) go
            // through a temporary so `x` isn't clobbered early
            Var::Register(reg) if writes_target_last(value) => self.expr_into(value, reg),
            _ => {
                let tmp = self.alloc_reg(value.span)?;
                self.expr_into(value, tmp)?;
                self.store(target, tmp)?;
                self.free_to(tmp as u32);
                Ok(())
            }
        }
    }

    fn store(&mut self, target: &Expr, src: u8) -> CResult {
        let base = self.fs.free_reg;
        let place = self.place(target, false)?;
        self.store_place(&place, src, target.span)?;
        self.free_to(base);
        Ok(())
    }

    fn return_values(&mut self, values: &[Expr], span: Span) -> CResult {
        // a single local gets returned straight from its register
        if let [value] = values
            && let ExprKind::Name(name) = &strip_groups(value).kind
            && let Var::Register(reg) = self.var(name)?
        {
            self.emit(Instr::abc(Op::Return, reg, 1, 0), span);
            return Ok(());
        }
        let multi_tail = values.last().is_some_and(|v| v.kind.is_multi());
        let base = self.fs.free_reg;
        self.explist(
            values,
            if multi_tail {
                Want::All
            } else {
                Want::Exactly(values.len() as u32)
            },
            span,
        )?;
        if multi_tail {
            // `return f(x)` (not parenthesised) is a tail call
            if let [value] = values
                && matches!(
                    value.kind,
                    ExprKind::Call { .. } | ExprKind::MethodCall { .. }
                )
                && let Some(last) = self.fs.proto.code.last_mut()
                && last.op() == Some(Op::Call)
                && last.a() as u32 == base
                && last.c() == 0
            {
                *last = Instr::abc(Op::TailCall, last.a(), last.b(), 0);
            }
            self.emit(Instr::abc(Op::Return, base as u8, 0, 1), span);
        } else {
            let count = u8::try_from(values.len())
                .map_err(|_| error("too many return values", span, "the limit is 255"))?;
            self.emit(Instr::abc(Op::Return, base as u8, count, 0), span);
        }
        self.free_to(base);
        Ok(())
    }

    fn if_stmt(&mut self, branches: &[(Expr, Block)], else_block: Option<&Block>) -> CResult {
        let mut to_end = Vec::new();
        for (i, (cond, body)) in branches.iter().enumerate() {
            let cond = self.root(cond);
            let skip = self.cond_jump(&cond, false)?;
            self.block(body)?;
            let is_last = i + 1 == branches.len() && else_block.is_none();
            if !is_last {
                to_end.push(self.emit_jump(body.span));
            }
            self.patch_here(skip)?;
        }
        if let Some(body) = else_block {
            self.block(body)?;
        }
        self.patch_here(to_end)
    }

    fn while_stmt(&mut self, cond: &Expr, body: &Block) -> CResult {
        let start = self.pc();
        let cond = self.root(cond);
        let exit = self.cond_jump(&cond, false)?;
        let base = self.fs.free_reg;
        let state = self.in_loop(base, base, |c| c.block(body))?;
        for j in state.continues {
            self.patch(j, start)?;
        }
        let back = self.emit_jump(body.span);
        self.patch(back, start)?;
        self.patch_here(exit)?;
        self.patch_here(state.breaks)
    }

    fn repeat_stmt(&mut self, body: &Block, cond: &Expr) -> CResult {
        let start = self.pc();
        let base = self.fs.free_reg;
        let active = self.fs.active.len();
        // the condition's inside the body's scope (it can read its locals)
        let mut breaks = Vec::new();
        self.scoped(|c| {
            let state = c.in_loop(base, base, |c| c.block_body(body))?;
            c.patch_here(state.continues)?;
            let cond = c.root(cond);
            match c.captured_from(active) {
                None => {
                    let again = c.cond_jump(&cond, false)?;
                    for j in again {
                        c.patch(j, start)?;
                    }
                }
                Some(reg) => {
                    // captured body locals have to be closed before each new
                    // iteration as well as on the way out (the scope's own
                    // CLOSE)
                    let exit = c.cond_jump(&cond, true)?;
                    c.emit(Instr::abc(Op::Close, reg, 0, 0), cond.span);
                    let back = c.emit_jump(cond.span);
                    c.patch(back, start)?;
                    c.patch_here(exit)?;
                }
            }
            breaks = state.breaks;
            Ok(())
        })?;
        self.patch_here(breaks)
    }

    fn numeric_for(
        &mut self,
        var: &Name,
        start: &Expr,
        limit: &Expr,
        step: Option<&Expr>,
        body: &Block,
        span: Span,
    ) -> CResult {
        self.scoped(|c| {
            // hidden locals: index, limit, step, then the visible variable
            let base = c.alloc_reg(span)?;
            let start = c.root(start);
            c.expr_into(&start, base)?;
            let limit_reg = c.alloc_reg(limit.span)?;
            let limit = c.root(limit);
            c.expr_into(&limit, limit_reg)?;
            let step_reg = c.alloc_reg(span)?;
            match step {
                Some(s) => {
                    let s = c.root(s);
                    c.expr_into(&s, step_reg)?;
                }
                None => {
                    c.emit(Instr::asbx(Op::LoadN, step_reg, 1), span);
                }
            }
            let var_reg = c.alloc_reg(var.span)?;
            c.fs.locals_top = c.fs.free_reg;
            c.declare_local(var, var_reg);

            let prep = c.emit(Instr::asbx(Op::ForPrep, base, 0), span);
            let body_start = c.pc();
            // `break` also closes the loop variable, `continue` only the body
            // (the variable gets closed just before FORLOOP, below)
            let state = c.in_loop(var_reg as u32, var_reg as u32 + 1, |c| c.block(body))?;
            c.patch_here(state.continues)?;
            // every iteration gets a fresh variable like in Luau, so close a
            // captured one before FORLOOP overwrites it
            let Binding::Local(id) = c.res.binding(var) else {
                unreachable!("loop variable is a local")
            };
            if c.res.is_captured(id) {
                c.emit(Instr::abc(Op::Close, var_reg, 0, 0), span);
            }
            let looped = c.emit(Instr::asbx(Op::ForLoop, base, 0), span);
            c.patch(looped, body_start)?;
            c.patch_here(vec![prep])?;
            c.patch_here(state.breaks)
        })
    }

    fn in_loop(
        &mut self,
        break_close: u32,
        continue_close: u32,
        f: impl FnOnce(&mut Self) -> CResult,
    ) -> CResult<LoopState> {
        self.fs.loops.push(LoopState {
            breaks: Vec::new(),
            continues: Vec::new(),
            break_close,
            continue_close,
        });
        let result = f(self);
        let state = self.fs.loops.pop().expect("pushed above");
        result.map(|()| state)
    }

    // ─ Expressions ───────────────────────────────────────────────────────────

    /// Compiles `expr`, leaving its value in register `target`.
    fn expr_into(&mut self, expr: &Expr, target: u8) -> CResult {
        stacker::maybe_grow(64 * 1024, 1024 * 1024, || {
            self.expr_into_inner(expr, target)
        })
    }

    fn expr_into_inner(&mut self, expr: &Expr, target: u8) -> CResult {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Nil => {
                self.emit(Instr::abc(Op::LoadNil, target, 0, 0), span);
            }
            ExprKind::Bool(b) => {
                self.emit(Instr::abc(Op::LoadB, target, *b as u8, 0), span);
            }
            ExprKind::Number(n) => self.load_number(*n, target, span)?,
            ExprKind::String(s) => {
                let k = self.string_constant(s, span)?;
                self.emit(Instr::abx(Op::LoadK, target, k), span);
            }
            ExprKind::Interp(parts) => {
                // every piece into consecutive registers, then one INTERP
                let base = self.fs.free_reg;
                for part in parts {
                    let reg = self.alloc_reg(span)?;
                    match part {
                        InterpPart::Text(t) => {
                            let k = self.string_constant(t, span)?;
                            self.emit(Instr::abx(Op::LoadK, reg, k), span);
                        }
                        InterpPart::Expr(e) => self.expr_into(e, reg)?,
                    }
                }
                let count = u8::try_from(parts.len()).map_err(|_| {
                    error(
                        "too many pieces in an interpolated string",
                        span,
                        "the limit is 255",
                    )
                })?;
                self.emit(Instr::abc(Op::Interp, target, base as u8, count), span);
                self.free_to(base);
            }
            ExprKind::Name(name) => match self.var(name)? {
                Var::Register(reg) => {
                    if reg != target {
                        self.emit(Instr::abc(Op::Move, target, reg, 0), span);
                    }
                }
                Var::Upvalue(i) => {
                    self.emit(Instr::abc(Op::GetUpval, target, i, 0), span);
                }
                Var::Global => {
                    let k = self.string_constant(&name.text, span)?;
                    self.emit(Instr::abx(Op::GetGlobal, target, k), span);
                }
            },
            ExprKind::Call { .. } | ExprKind::MethodCall { .. } => {
                // a call needs its own base at the top of the stack
                if self.is_top_temp(target) {
                    self.multi_into(expr, target, Some(1))?;
                } else {
                    let base = self.alloc_reg(span)?;
                    self.multi_into(expr, base, Some(1))?;
                    self.emit(Instr::abc(Op::Move, target, base, 0), span);
                    self.free_to(base as u32);
                }
            }
            ExprKind::Index { object, key } => {
                let obj = self.operand(object, Some(target))?;
                match self.field_constant(key)? {
                    Some(k) => {
                        self.emit(Instr::abc(Op::GetField, target, obj.reg, k), span);
                    }
                    None => {
                        let k = self.operand(key, None)?;
                        self.emit(Instr::abc(Op::GetTable, target, obj.reg, k.reg), span);
                    }
                }
                self.free_to(obj.restore);
            }
            ExprKind::Table(items) => {
                // positional values get gathered just above the table
                if self.is_top_temp(target) {
                    self.table_constructor(items, target, span)?;
                } else {
                    let base = self.alloc_reg(span)?;
                    self.table_constructor(items, base, span)?;
                    self.emit(Instr::abc(Op::Move, target, base, 0), span);
                    self.free_to(base as u32);
                }
            }
            ExprKind::Vararg => {
                self.emit(Instr::abc(Op::Vararg, target, 2, 0), span);
            }
            ExprKind::Function(func) => self.function(func, target)?,
            ExprKind::Group(inner) | ExprKind::Cast { expr: inner, .. } => {
                self.expr_into(inner, target)?
            }
            ExprKind::Unary { op, operand, .. } => {
                let opcode = match op {
                    UnOp::Neg => Op::Unm,
                    UnOp::Not => Op::Not,
                    UnOp::Len => Op::Len,
                };
                let src = self.operand(operand, Some(target))?;
                self.emit(Instr::abc(opcode, target, src.reg, 0), span);
                self.free_to(src.restore);
            }
            ExprKind::Binary { op, lhs, rhs, .. } => match op {
                BinOp::And | BinOp::Or => {
                    // `a and b`: target := a, if falsy we're done, else target
                    // := b
                    self.expr_into(lhs, target)?;
                    let jump = if *op == BinOp::And {
                        Op::JmpIfNot
                    } else {
                        Op::JmpIf
                    };
                    let skip = self.emit(Instr::asbx(jump, target, 0), span);
                    self.expr_into(rhs, target)?;
                    self.patch_here(vec![skip])?;
                }
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    self.bool_from_condition(expr, target)?;
                }
                BinOp::Concat => self.binary_rr(Op::Concat, lhs, rhs, target, span)?,
                _ => {
                    let aop = fold::arith_op(*op).expect("remaining operators are arithmetic");
                    self.arith(aop, lhs, rhs, target, span)?;
                }
            },
            ExprKind::IfElse {
                branches,
                else_expr,
            } => {
                let mut to_end = Vec::new();
                for (cond, value) in branches {
                    let skip = self.cond_jump(cond, false)?;
                    self.expr_into(value, target)?;
                    to_end.push(self.emit_jump(value.span));
                    self.patch_here(skip)?;
                }
                self.expr_into(else_expr, target)?;
                self.patch_here(to_end)?;
            }
        }
        Ok(())
    }

    /// `target := cond and true or false`, through a jump list and a pair of
    /// `LOADB`s (the first skips the second).
    fn bool_from_condition(&mut self, expr: &Expr, target: u8) -> CResult {
        let when_true = self.cond_jump(expr, true)?;
        self.emit(Instr::abc(Op::LoadB, target, 0, 1), expr.span);
        self.patch_here(when_true)?;
        self.emit(Instr::abc(Op::LoadB, target, 1, 0), expr.span);
        Ok(())
    }

    fn arith(&mut self, op: ArithOp, lhs: &Expr, rhs: &Expr, target: u8, span: Span) -> CResult {
        let left = self.operand(lhs, Some(target))?;
        // `x + 1` → ADDK when the right side is a constant that fits in C.
        // after folding every constant subexpression is already a literal
        let k = match rhs.kind {
            ExprKind::Number(n) => u8::try_from(self.number_constant(n, rhs.span)?).ok(),
            _ => None,
        };
        match k {
            Some(k) => {
                self.emit(Instr::abc(Op::arith_rk(op), target, left.reg, k), span);
            }
            None => {
                let right = self.operand(rhs, None)?;
                self.emit(
                    Instr::abc(Op::arith_rr(op), target, left.reg, right.reg),
                    span,
                );
            }
        }
        self.free_to(left.restore);
        Ok(())
    }

    fn binary_rr(&mut self, op: Op, lhs: &Expr, rhs: &Expr, target: u8, span: Span) -> CResult {
        let left = self.operand(lhs, Some(target))?;
        let right = self.operand(rhs, None)?;
        self.emit(Instr::abc(op, target, left.reg, right.reg), span);
        self.free_to(left.restore);
        Ok(())
    }

    /// Gets `expr` into some register the caller can read but not keep: a
    /// local's own register, `hint` if it's the top temporary, or a new
    /// temporary. Call `free_to(result.restore)` after.
    fn operand(&mut self, expr: &Expr, hint: Option<u8>) -> CResult<Operand> {
        let restore = self.fs.free_reg;
        let inner = strip_groups(expr);
        if let ExprKind::Name(name) = &inner.kind
            && let Var::Register(reg) = self.var(name)?
        {
            return Ok(Operand { reg, restore });
        }
        let reg = match hint {
            Some(t) if self.is_top_temp(t) => t,
            _ => self.alloc_reg(expr.span)?,
        };
        self.expr_into(expr, reg)?;
        Ok(Operand { reg, restore })
    }

    fn load_number(&mut self, n: f64, target: u8, span: Span) -> CResult {
        let small = n.fract() == 0.0
            && n >= i16::MIN as f64
            && n <= i16::MAX as f64
            && !(n == 0.0 && n.is_sign_negative());
        if small {
            self.emit(Instr::asbx(Op::LoadN, target, n as i16), span);
        } else {
            let k = self.number_constant(n, span)?;
            self.emit(Instr::abx(Op::LoadK, target, k), span);
        }
        Ok(())
    }

    // ─ Conditions ────────────────────────────────────────────────────────────

    /// Emits code that jumps when `expr`'s truthiness equals `when` and falls
    /// through otherwise. Returns the jumps to patch.
    fn cond_jump(&mut self, expr: &Expr, when: bool) -> CResult<Vec<usize>> {
        stacker::maybe_grow(64 * 1024, 1024 * 1024, || self.cond_jump_inner(expr, when))
    }

    fn cond_jump_inner(&mut self, expr: &Expr, when: bool) -> CResult<Vec<usize>> {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Group(inner) | ExprKind::Cast { expr: inner, .. } => {
                self.cond_jump(inner, when)
            }
            // constant conditions: an unconditional jump or nothing at all
            ExprKind::Nil | ExprKind::Bool(_) | ExprKind::Number(_) | ExprKind::String(_) => {
                let truthy = !matches!(expr.kind, ExprKind::Nil | ExprKind::Bool(false));
                Ok(if truthy == when {
                    vec![self.emit_jump(span)]
                } else {
                    vec![]
                })
            }
            ExprKind::Unary {
                op: UnOp::Not,
                operand,
                ..
            } => self.cond_jump(operand, !when),
            ExprKind::Binary {
                op: BinOp::And,
                lhs,
                rhs,
                ..
            } => {
                if when {
                    let lhs_false = self.cond_jump(lhs, false)?;
                    let jumps = self.cond_jump(rhs, true)?;
                    self.patch_here(lhs_false)?;
                    Ok(jumps)
                } else {
                    let mut jumps = self.cond_jump(lhs, false)?;
                    jumps.extend(self.cond_jump(rhs, false)?);
                    Ok(jumps)
                }
            }
            ExprKind::Binary {
                op: BinOp::Or,
                lhs,
                rhs,
                ..
            } => {
                if when {
                    let mut jumps = self.cond_jump(lhs, true)?;
                    jumps.extend(self.cond_jump(rhs, true)?);
                    Ok(jumps)
                } else {
                    let lhs_true = self.cond_jump(lhs, true)?;
                    let jumps = self.cond_jump(rhs, false)?;
                    self.patch_here(lhs_true)?;
                    Ok(jumps)
                }
            }
            ExprKind::Binary { op, lhs, rhs, .. }
                if matches!(
                    op,
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                ) =>
            {
                // a constant on either side, one instruction against K(C)
                if let Some((cmp, reg_side, k)) = self.compare_with_constant(*op, lhs, rhs)? {
                    let expect = if *op == BinOp::Ne { !when } else { when };
                    let value = self.operand(reg_side, None)?;
                    self.emit(Instr::abc(cmp, expect as u8, value.reg, k), span);
                    self.free_to(value.restore);
                    return Ok(vec![self.emit_jump(span)]);
                }
                // `a > b` is `b < a` and `a >= b` is `b <= a`, that's how Lua
                // defines them
                let (cmp, swap, expect) = match op {
                    BinOp::Eq => (Op::Eq, false, when),
                    BinOp::Ne => (Op::Eq, false, !when),
                    BinOp::Lt => (Op::Lt, false, when),
                    BinOp::Le => (Op::Le, false, when),
                    BinOp::Gt => (Op::Lt, true, when),
                    BinOp::Ge => (Op::Le, true, when),
                    _ => unreachable!(),
                };
                let left = self.operand(lhs, None)?;
                let right = self.operand(rhs, None)?;
                let (b, c) = if swap {
                    (right.reg, left.reg)
                } else {
                    (left.reg, right.reg)
                };
                self.emit(Instr::abc(cmp, expect as u8, b, c), span);
                self.free_to(left.restore);
                Ok(vec![self.emit_jump(span)])
            }
            _ => {
                let value = self.operand(expr, None)?;
                let op = if when { Op::JmpIf } else { Op::JmpIfNot };
                let j = self.emit(Instr::asbx(op, value.reg, 0), span);
                self.free_to(value.restore);
                Ok(vec![j])
            }
        }
    }
}

impl Compiler<'_> {
    /// For a comparison with a literal number or string on one side (and
    /// something else on the other), the `..K` opcode that compares the other
    /// side against it: `(op, other side, constant index)`. It's picked so
    /// `R(B) op K(C)` means the same as the source. `None` when neither side's
    /// a literal or the index won't fit in C.
    fn compare_with_constant<'e>(
        &mut self,
        op: BinOp,
        lhs: &'e Expr,
        rhs: &'e Expr,
    ) -> CResult<Option<(Op, &'e Expr, u8)>> {
        let literal = |c: &mut Self, e: &Expr| -> CResult<Option<u8>> {
            Ok(match &strip_groups(e).kind {
                ExprKind::Number(n) => u8::try_from(c.number_constant(*n, e.span)?).ok(),
                ExprKind::String(s) => u8::try_from(c.string_constant(s, e.span)?).ok(),
                _ => None,
            })
        };
        let is_literal = |e: &Expr| {
            matches!(
                strip_groups(e).kind,
                ExprKind::Number(_) | ExprKind::String(_)
            )
        };
        // `x op K`
        if is_literal(rhs) && !is_literal(lhs) {
            let Some(k) = literal(self, rhs)? else {
                return Ok(None);
            };
            let cmp = match op {
                BinOp::Eq | BinOp::Ne => Op::EqK,
                BinOp::Lt => Op::LtK,
                BinOp::Le => Op::LeK,
                BinOp::Gt => Op::GtK,
                BinOp::Ge => Op::GeK,
                _ => return Ok(None),
            };
            return Ok(Some((cmp, lhs, k)));
        }
        // `K op x` is `x op' K` with the comparison mirrored
        if is_literal(lhs) && !is_literal(rhs) {
            let Some(k) = literal(self, lhs)? else {
                return Ok(None);
            };
            let cmp = match op {
                BinOp::Eq | BinOp::Ne => Op::EqK,
                BinOp::Lt => Op::GtK,
                BinOp::Le => Op::GeK,
                BinOp::Gt => Op::LtK,
                BinOp::Ge => Op::LeK,
                _ => return Ok(None),
            };
            return Ok(Some((cmp, rhs, k)));
        }
        Ok(None)
    }
}

/// Strips parentheses and type assertions (`(e)`, `e :: T`), both just evaluate
/// `e` to one value.
fn strip_groups(mut expr: &Expr) -> &Expr {
    while let ExprKind::Group(inner) | ExprKind::Cast { expr: inner, .. } = &expr.kind {
        expr = inner;
    }
    expr
}

/// Whether compiling `expr` into a register writes it only after every
/// operand's been read. `and`/`or`/if-expressions write their first branch's
/// value early, so they can't target a live local directly.
fn writes_target_last(expr: &Expr) -> bool {
    !matches!(
        &strip_groups(expr).kind,
        ExprKind::Binary {
            op: BinOp::And | BinOp::Or,
            ..
        } | ExprKind::IfElse { .. }
            // `t = {t}` would make the new table before reading the old one
            | ExprKind::Table(_)
    )
}

trait BoxedDiagnosticExt {
    fn map_help(self, help: &str) -> Self;
}

impl BoxedDiagnosticExt for Box<Diagnostic> {
    fn map_help(mut self, help: &str) -> Self {
        self.help = Some(help.to_string());
        self
    }
}

#[cfg(test)]
mod tests;

/// Finds or makes the upvalue for local `id` in `fs`, making matching upvalues
/// in every enclosing function in between (Lua's `singlevaraux`). `None` if no
/// enclosing function declares `id`, `Some(Err(()))` past 255 upvalues.
fn upvalue_index(
    parents: &mut [FuncState],
    fs: &mut FuncState,
    id: LocalId,
    res: &Resolution,
) -> Option<Result<u8, ()>> {
    if let Some(i) = fs.upvalue_ids.iter().position(|&u| u == id) {
        return Some(Ok(i as u8));
    }
    let (parent, rest) = parents.split_last_mut()?;
    let (in_stack, index) = match parent.local_regs.get(&id) {
        Some(&reg) => (true, reg),
        None => match upvalue_index(rest, parent, id, res)? {
            Ok(i) => (false, i),
            Err(()) => return Some(Err(())),
        },
    };
    if fs.upvalue_ids.len() >= 255 {
        return Some(Err(()));
    }
    fs.upvalue_ids.push(id);
    fs.proto.upvalues.push(UpvalDesc {
        in_stack,
        index,
        name: res.locals[id as usize].name.clone(),
    });
    Some(Ok((fs.upvalue_ids.len() - 1) as u8))
}
