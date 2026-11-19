//! The bytecode verifier.
//!
//! [`verify`] checks that a prototype (and every prototype nested in it) is
//! well formed before the VM runs it:
//!
//! - every opcode is valid, and the code can't run off its end
//! - every register an instruction reads or writes is below `max_stack`
//!   (including the hidden ones `FORLOOP`, `TFORCALL` and `SELF` use)
//! - constant, upvalue and prototype indices exist, and the constants are
//!   the type the instruction needs (global and field names are strings)
//! - every jump lands on an instruction (never on a `DATA` word)
//! - a comparison is followed by its `JMP`, and `SETLIST` by its `DATA`
//! - an instruction that eats "values up to `top`" comes right after one
//!   that makes them, and isn't a jump target (so `top` is always the one
//!   that was just set)
//! - closures capture registers and upvalues that exist
//! - debug info is in range: spans are inside the source text, and each
//!   local's register and pc range exist
//!
//! The VM is memory-safe no matter what it runs (no `unsafe`, every access
//! is checked), but bad bytecode would make it panic instead of raising a
//! script error. The compiler's output always verifies, the engine checks
//! it after every compile, and the test suites check every prototype they
//! compile.

use std::fmt;

use crate::{Constant, Instr, Op, Proto};

/// Why a prototype was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyError {
    /// The function's name in tracebacks (`<main>` for a chunk).
    pub function: String,
    /// The offending instruction, when there is one.
    pub pc: Option<usize>,
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pc {
            Some(pc) => write!(
                f,
                "invalid bytecode in {} at {pc}: {}",
                self.function, self.message
            ),
            None => write!(f, "invalid bytecode in {}: {}", self.function, self.message),
        }
    }
}

impl std::error::Error for VerifyError {}

/// The most registers a function can use. The VM keeps a window of a bit more
/// than 256 slots above every frame, so this leaves room for the registers
/// above `R(A)` that some instructions touch.
pub const MAX_STACK: usize = 250;

/// Checks `proto` (a main chunk) and everything nested in it.
pub fn verify(proto: &Proto) -> Result<(), VerifyError> {
    verify_one(proto, true)
}

/// [`verify`], skipped when this prototype already passed (it stays marked for
/// as long as it lives, the code can't change inside an `Rc`).
pub fn verify_once(proto: &Proto) -> Result<(), VerifyError> {
    if proto.verified.get() {
        return Ok(());
    }
    verify(proto)?;
    proto.verified.set();
    Ok(())
}

fn verify_one(proto: &Proto, is_main: bool) -> Result<(), VerifyError> {
    let function = if is_main {
        "<main>".to_string()
    } else {
        format!("`{}`", proto.display_name())
    };
    let fail = |pc: Option<usize>, message: String| VerifyError {
        function: function.clone(),
        pc,
        message,
    };

    let code = &proto.code;
    let max = proto.max_stack as usize;
    if max > MAX_STACK {
        return Err(fail(
            None,
            format!("uses {max} registers (the limit is {MAX_STACK})"),
        ));
    }
    if proto.num_params as usize > max {
        return Err(fail(None, "has more parameters than registers".into()));
    }
    if is_main && !proto.upvalues.is_empty() {
        // a chunk runs as a closure with no upvalues
        return Err(fail(None, "a main chunk can't have upvalues".into()));
    }
    if proto.spans.len() != code.len() {
        return Err(fail(None, "needs one source span per instruction".into()));
    }
    let text_len = proto.source.text.len() as u32;
    if let Some(pc) =
        (proto.spans.iter().chain([&proto.span])).position(|s| s.start > s.end || s.end > text_len)
    {
        return Err(fail(
            (pc < code.len()).then_some(pc),
            "has a source span outside its source text".into(),
        ));
    }
    for (i, t) in proto.templates.iter().enumerate() {
        let bad = |why: &str| Err(fail(None, format!("has a bad table template T{i}: {why}")));
        if t.array > MAX_TEMPLATE_SIZE || t.hash > MAX_TEMPLATE_SIZE {
            return bad("too large");
        }
        if (t.hash as usize) < t.keys.len() {
            return bad("fewer hash slots than keys");
        }
        let mut seen = std::collections::HashSet::with_capacity(t.keys.len());
        for &k in &t.keys {
            let Some(Constant::String(s)) = proto.constants.get(k as usize) else {
                return bad("a key isn't a string constant");
            };
            if !seen.insert(&**s) {
                return bad("a key appears twice");
            }
        }
    }
    for local in &proto.locals {
        if local.reg as usize >= max
            || local.start_pc > local.end_pc
            || local.end_pc as usize > code.len()
        {
            return Err(fail(
                None,
                format!("has bad debug info for local `{}`", local.name),
            ));
        }
    }
    match code.last().and_then(|i| i.op()) {
        Some(Op::Return | Op::Jmp) => {}
        _ => return Err(fail(None, "doesn't end with RETURN or JMP".into())),
    }

    // instructions that consume `top` have to come right after one that sets
    // it, so neither can be a jump target
    let mut targets = vec![false; code.len()];

    for (pc, &ins) in code.iter().enumerate() {
        let err = |message: String| fail(Some(pc), message);
        let Some(op) = ins.op() else {
            return Err(err(format!("invalid opcode {:#04x}", ins.0 as u8)));
        };
        let (a, b, c) = (ins.a() as usize, ins.b() as usize, ins.c() as usize);
        let reg = |r: usize, what: &str| {
            if r < max {
                Ok(())
            } else {
                Err(err(format!(
                    "{what} register R{r} is beyond max_stack {max}"
                )))
            }
        };
        let regs = |from: usize, count: usize| {
            if count == 0 || from + count <= max {
                Ok(())
            } else {
                Err(err(format!(
                    "registers R{from}..R{} are beyond max_stack {max}",
                    from + count - 1
                )))
            }
        };
        let constant = |k: usize| {
            proto
                .constants
                .get(k)
                .ok_or_else(|| err(format!("no constant K{k}")))
        };
        let string_constant = |k: usize| match constant(k)? {
            Constant::String(_) => Ok(()),
            Constant::Number(_) => Err(err(format!("K{k} must be a string"))),
        };
        let number_constant = |k: usize| match constant(k)? {
            Constant::Number(_) => Ok(()),
            Constant::String(_) => Err(err(format!("K{k} must be a number"))),
        };
        let mut jump = |by: i32| {
            let target = pc as i64 + 1 + by as i64;
            if target < 0 || target >= code.len() as i64 {
                return Err(err(format!("jump to {target} is outside the code")));
            }
            let target = target as usize;
            if code[target].op() == Some(Op::Data) {
                return Err(err(format!("jump to {target} lands on a DATA word")));
            }
            targets[target] = true;
            Ok(())
        };
        let next = code.get(pc + 1).and_then(|i| i.op());

        match op {
            Op::LoadNil | Op::LoadN | Op::NewTable => reg(a, "target")?,
            Op::DupTable => {
                reg(a, "target")?;
                if ins.bx() as usize >= proto.templates.len() {
                    return Err(err(format!("no table template T{}", ins.bx())));
                }
            }
            Op::LoadB => {
                reg(a, "target")?;
                if c != 0 {
                    // skips the next instruction
                    jump(1)?;
                }
            }
            Op::LoadK => {
                reg(a, "target")?;
                constant(ins.bx() as usize)?;
            }
            Op::GetGlobal | Op::SetGlobal => {
                reg(a, "value")?;
                string_constant(ins.bx() as usize)?;
            }
            Op::Move | Op::Unm | Op::Not | Op::Len => {
                reg(a, "target")?;
                reg(b, "source")?;
            }
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::IDiv | Op::Mod | Op::Pow | Op::Concat => {
                reg(a, "target")?;
                reg(b, "left")?;
                reg(c, "right")?;
            }
            Op::AddK | Op::SubK | Op::MulK | Op::DivK | Op::IDivK | Op::ModK | Op::PowK => {
                reg(a, "target")?;
                reg(b, "left")?;
                number_constant(c)?;
            }
            Op::Eq | Op::Lt | Op::Le | Op::EqK | Op::LtK | Op::LeK | Op::GtK | Op::GeK => {
                reg(b, "left")?;
                if matches!(op, Op::Eq | Op::Lt | Op::Le) {
                    reg(c, "right")?;
                } else {
                    constant(c)?;
                }
                if next != Some(Op::Jmp) {
                    return Err(err("a comparison must be followed by a JMP".into()));
                }
                // skips the jump
                jump(1)?;
            }
            Op::Jmp => jump(ins.sax())?,
            Op::JmpIf | Op::JmpIfNot => {
                reg(a, "tested")?;
                jump(ins.sbx() as i32)?;
            }
            Op::ForPrep | Op::ForLoop => {
                regs(a, 4)?;
                jump(ins.sbx() as i32)?;
            }
            Op::GetUpval | Op::SetUpval => {
                reg(a, "value")?;
                if b >= proto.upvalues.len() {
                    return Err(err(format!("no upvalue U{b}")));
                }
            }
            Op::Close => {
                if a > max {
                    return Err(err(format!("closes from R{a}, beyond max_stack {max}")));
                }
            }
            Op::Closure => {
                reg(a, "target")?;
                let Some(child) = proto.protos.get(ins.bx() as usize) else {
                    return Err(err(format!("no prototype P{}", ins.bx())));
                };
                for (i, up) in child.upvalues.iter().enumerate() {
                    let ok = if up.in_stack {
                        (up.index as usize) < max
                    } else {
                        (up.index as usize) < proto.upvalues.len()
                    };
                    if !ok {
                        return Err(err(format!(
                            "the closure's upvalue {i} captures something that doesn't exist"
                        )));
                    }
                }
            }
            Op::Call => {
                reg(a, "function")?;
                if b > 0 {
                    regs(a + 1, b - 1)?;
                } else {
                    consumes_top(code, pc, a + 1).map_err(err)?;
                }
                if c > 0 {
                    regs(a, c - 1)?;
                }
            }
            Op::Interp => {
                reg(a, "target")?;
                if c == 0 {
                    return Err(err("INTERP needs at least one piece".into()));
                }
                regs(b, c)?;
            }
            Op::TailCall => {
                reg(a, "function")?;
                if b > 0 {
                    regs(a + 1, b - 1)?;
                } else {
                    consumes_top(code, pc, a + 1).map_err(err)?;
                }
                let returns = code.get(pc + 1).is_some_and(|r| {
                    r.op() == Some(Op::Return) && r.a() as usize == a && r.c() != 0
                });
                if c != 0 || !returns {
                    return Err(err(
                        "TAILCALL must keep all results and be followed by RETURN of them".into(),
                    ));
                }
            }
            Op::Vararg => {
                if b > 0 {
                    regs(a, b - 1)?;
                } else if a > max {
                    return Err(err(format!("R{a} is beyond max_stack {max}")));
                }
            }
            Op::GetTable | Op::SetTable => {
                reg(a, "table/target")?;
                reg(b, "table/key")?;
                reg(c, "key/value")?;
            }
            Op::GetField => {
                reg(a, "target")?;
                reg(b, "table")?;
                string_constant(c)?;
            }
            Op::SetField => {
                reg(a, "table")?;
                string_constant(b)?;
                reg(c, "value")?;
            }
            Op::SelfOp => {
                regs(a, 2)?;
                reg(b, "object")?;
                string_constant(c)?;
            }
            Op::SetList => {
                reg(a, "table")?;
                if b > 0 {
                    regs(a + 1, b)?;
                } else {
                    consumes_top(code, pc, a + 1).map_err(err)?;
                }
                if next != Some(Op::Data) {
                    return Err(err("SETLIST must be followed by its DATA word".into()));
                }
                if code[pc + 1].sax() < 0 {
                    return Err(err("SETLIST start index is negative".into()));
                }
            }
            Op::Data => {
                if pc == 0 || code[pc - 1].op() != Some(Op::SetList) {
                    return Err(err("a DATA word must follow SETLIST".into()));
                }
            }
            Op::TForCall => {
                // the iterator, state and control, then the call's copies of
                // them, which double as the variables (at least three)
                if c == 0 {
                    return Err(err("a generic for needs at least one variable".into()));
                }
                regs(a, 3 + c.max(3))?;
            }
            Op::TForLoop => {
                regs(a, 4)?;
                jump(ins.sbx() as i32)?;
            }
            Op::Return => {
                if c != 0 {
                    consumes_top(code, pc, a).map_err(err)?;
                } else if b > 0 {
                    regs(a, b)?;
                } else if a > max {
                    return Err(err(format!("R{a} is beyond max_stack {max}")));
                }
            }
        }
    }

    // `top` only carries over from the instruction right before
    for (pc, &ins) in code.iter().enumerate() {
        let uses_top = match ins.op() {
            Some(Op::Call | Op::TailCall) => ins.b() == 0,
            Some(Op::SetList) => ins.b() == 0,
            Some(Op::Return) => ins.c() != 0,
            _ => false,
        };
        if uses_top && targets[pc] {
            return Err(fail(
                Some(pc),
                "a jump lands on an instruction that uses the values left by the one before it"
                    .into(),
            ));
        }
    }

    for child in &proto.protos {
        verify_one(child, false)?;
    }
    Ok(())
}

/// Checks that the instruction before `pc` leaves its values (`top`) at or
/// above register `from`: a `CALL` keeping all its results, a `TAILCALL` (when
/// the callee's a native), or a `VARARG` copying everything.
fn consumes_top(code: &[Instr], pc: usize, from: usize) -> Result<(), String> {
    let prev = pc.checked_sub(1).map(|p| code[p]);
    match prev.and_then(|i| i.op().map(|op| (op, i))) {
        Some((Op::Call, i)) if i.c() == 0 && i.a() as usize >= from => Ok(()),
        Some((Op::TailCall, i)) if i.a() as usize >= from => Ok(()),
        Some((Op::Vararg, i)) if i.b() == 0 && i.a() as usize >= from => Ok(()),
        _ => Err(
            "uses the values up to `top`, but the instruction before doesn't leave any \
                  there"
                .into(),
        ),
    }
}

/// The most array or hash slots a table template can reserve.
pub const MAX_TEMPLATE_SIZE: u32 = 1 << 16;

#[cfg(test)]
mod tests {
    use super::*;
    use rhue_common::Span;

    fn proto(code: Vec<Instr>, constants: Vec<Constant>, max_stack: u8) -> Proto {
        Proto {
            spans: vec![Span::default(); code.len()],
            code,
            constants,
            max_stack,
            ..Proto::default()
        }
    }

    fn rejects(p: Proto, needle: &str) {
        let e = verify(&p).expect_err("should be rejected");
        assert!(e.to_string().contains(needle), "{e}");
    }

    #[test]
    fn accepts_a_minimal_function() {
        verify(&proto(vec![Instr::abc(Op::Return, 0, 0, 0)], vec![], 0)).unwrap();
    }

    #[test]
    fn rejects_malformed_code() {
        rejects(proto(vec![], vec![], 0), "doesn't end");
        rejects(
            proto(vec![Instr::asbx(Op::LoadN, 0, 1)], vec![], 1),
            "doesn't end",
        );
        rejects(
            proto(
                vec![Instr(0xff), Instr::abc(Op::Return, 0, 0, 0)],
                vec![],
                1,
            ),
            "invalid opcode",
        );
        rejects(
            proto(
                vec![
                    Instr::abc(Op::Move, 3, 0, 0),
                    Instr::abc(Op::Return, 0, 0, 0),
                ],
                vec![],
                2,
            ),
            "R3",
        );
        rejects(
            proto(
                vec![Instr::abx(Op::LoadK, 0, 5), Instr::abc(Op::Return, 0, 0, 0)],
                vec![],
                1,
            ),
            "no constant K5",
        );
        rejects(
            proto(
                vec![
                    Instr::abx(Op::GetGlobal, 0, 0),
                    Instr::abc(Op::Return, 0, 0, 0),
                ],
                vec![Constant::Number(1.0)],
                1,
            ),
            "must be a string",
        );
        rejects(
            proto(
                vec![Instr::asax(Op::Jmp, 5), Instr::abc(Op::Return, 0, 0, 0)],
                vec![],
                0,
            ),
            "outside the code",
        );
        rejects(
            proto(
                vec![Instr::abc(Op::Lt, 0, 0, 0), Instr::abc(Op::Return, 0, 0, 0)],
                vec![],
                1,
            ),
            "followed by a JMP",
        );
        rejects(
            proto(
                vec![
                    Instr::abc(Op::Call, 0, 0, 1),
                    Instr::abc(Op::Return, 0, 0, 0),
                ],
                vec![],
                1,
            ),
            "top",
        );
        rejects(
            proto(
                vec![
                    Instr::abc(Op::GetUpval, 0, 0, 0),
                    Instr::abc(Op::Return, 0, 0, 0),
                ],
                vec![],
                1,
            ),
            "no upvalue",
        );
        rejects(
            proto(
                vec![
                    Instr::abc(Op::SetList, 0, 1, 0),
                    Instr::abc(Op::Return, 0, 0, 0),
                ],
                vec![],
                2,
            ),
            "DATA",
        );
    }

    #[test]
    fn rejects_jumps_onto_data_and_into_top_consumers() {
        rejects(
            proto(
                vec![
                    Instr::asax(Op::Jmp, 1),
                    Instr::abc(Op::SetList, 0, 1, 0),
                    Instr::asax(Op::Data, 0),
                    Instr::abc(Op::Return, 0, 0, 0),
                ],
                vec![],
                2,
            ),
            "DATA word",
        );
        rejects(
            proto(
                vec![
                    Instr::abc(Op::Call, 0, 1, 0),
                    Instr::asax(Op::Jmp, 0),
                    Instr::abc(Op::Return, 0, 0, 1),
                ],
                vec![],
                1,
            ),
            "before",
        );
    }
}
