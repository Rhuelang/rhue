use std::cell::{Cell, OnceCell};
use std::fmt::{self, Write};
use std::rc::Rc;

use rhue_common::{SourceFile, Span};

use crate::{Instr, Op, format_number};

/// A compile-time constant.
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Number(f64),
    String(Rc<str>),
}

/// A local variable, for debuggers: which register holds it, and when.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalVar {
    pub name: Rc<str>,
    pub reg: u8,
    /// The instructions it's in scope for, `start_pc..end_pc`.
    pub start_pc: u32,
    pub end_pc: u32,
}

/// How a closure finds one of its upvalues when it gets created.
#[derive(Clone, Debug, PartialEq)]
pub struct UpvalDesc {
    /// `true`: capture the enclosing function's register `index`. `false`:
    /// share the enclosing function's own upvalue `index`.
    pub in_stack: bool,
    pub index: u8,
    /// The variable's name, for disassembly and debugging.
    pub name: Rc<str>,
}

/// Per-instruction slots the VM uses as *inline caches*. For a field or global
/// access it remembers where in the table's hash part the key was last time, so
/// the next lookup can check that one entry instead of hashing (the key still
/// gets compared, so a stale hint just costs a normal lookup).
///
/// Made on first use, one slot per instruction. It's runtime state, not part of
/// the program, so clones start empty and it never affects equality.
#[derive(Default)]
pub struct InlineCache(OnceCell<Box<[Cell<u32>]>>);

impl InlineCache {
    /// The slot for instruction `pc` of a prototype with `len` instructions.
    #[inline]
    pub fn slot(&self, pc: usize, len: usize) -> &Cell<u32> {
        &self
            .0
            .get_or_init(|| (0..len).map(|_| Cell::new(0)).collect())[pc]
    }
}

impl Clone for InlineCache {
    fn clone(&self) -> Self {
        InlineCache::default()
    }
}

impl PartialEq for InlineCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl fmt::Debug for InlineCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InlineCache")
    }
}

/// Remembers that a prototype passed [`verify`](crate::verify), so a chunk that
/// runs a lot only gets checked once. Same as [`InlineCache`], clones start
/// unmarked and it never affects equality.
#[derive(Default)]
pub struct Verified(Cell<bool>);

impl Verified {
    pub fn get(&self) -> bool {
        self.0.get()
    }

    pub fn set(&self) {
        self.0.set(true);
    }
}

impl Clone for Verified {
    fn clone(&self) -> Self {
        Verified::default()
    }
}

impl PartialEq for Verified {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl fmt::Debug for Verified {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Verified({})", self.0.get())
    }
}

/// The shape of the tables a constructor makes (`DUPTABLE`): sizes to reserve
/// and the constant string keys it stores first, in order. Keys start out nil,
/// which tables treat as tombstones, so the program can't tell a template's
/// there except the constructor doesn't have to grow the table and search for
/// each key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableTemplate {
    /// Array slots to reserve (the constructor's positional values).
    pub array: u32,
    /// Hash slots to reserve (all its keyed fields).
    pub hash: u32,
    /// Constant indices of the keys, each a different string.
    pub keys: Vec<u32>,
}

/// A compiled function prototype, what the VM actually runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Proto {
    /// `None` for the main chunk, otherwise the declared name if there is one.
    pub name: Option<Rc<str>>,
    pub code: Vec<Instr>,
    pub constants: Vec<Constant>,
    /// One source span per instruction, for runtime errors.
    pub spans: Vec<Span>,
    /// How many registers the VM needs to reserve for one call.
    pub max_stack: u8,
    pub num_params: u8,
    pub is_vararg: bool,
    /// Prototypes of the functions defined inside this one (`CLOSURE`
    /// operands).
    pub protos: Vec<Rc<Proto>>,
    pub upvalues: Vec<UpvalDesc>,
    /// Debug info, each local's register and live range.
    pub locals: Vec<LocalVar>,
    /// Where the function is defined in the source.
    pub span: Span,
    /// The source every span in here points into.
    pub source: Rc<SourceFile>,
    /// Shapes for `DUPTABLE`, one per table constructor with constant keys.
    pub templates: Vec<TableTemplate>,
    /// The VM's lookup hints (see [`InlineCache`]).
    pub cache: InlineCache,
    /// Set once [`verify_once`](crate::verify_once) has checked this tree.
    pub verified: Verified,
}

impl Proto {
    /// The name shown in listings and tracebacks.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or("<anonymous>")
    }

    /// A readable listing, used by `rhue --dump` and the snapshot tests. Nested
    /// functions come after their parent.
    pub fn disassemble(&self) -> String {
        let mut out = String::new();
        self.disassemble_into(&mut out, true);
        out
    }

    fn disassemble_into(&self, out: &mut String, is_main: bool) {
        let name = if is_main {
            "<main>"
        } else {
            self.display_name()
        };
        let _ = write!(
            out,
            "function {name} ({} instructions, {} constants, {} registers",
            self.code.len(),
            self.constants.len(),
            self.max_stack
        );
        if !is_main {
            let _ = write!(
                out,
                ", {} params{}",
                self.num_params,
                if self.is_vararg { " + ..." } else { "" }
            );
        }
        if !self.upvalues.is_empty() {
            let names: Vec<&str> = self.upvalues.iter().map(|u| &*u.name).collect();
            let _ = write!(out, ", upvalues: {}", names.join(", "));
        }
        let _ = writeln!(out, ")");
        for (pc, ins) in self.code.iter().enumerate() {
            let Some(op) = ins.op() else {
                let _ = writeln!(out, "  {pc:>4}  <bad opcode {:#04x}>", ins.0 & 0xff);
                continue;
            };
            let (a, b, c) = (ins.a(), ins.b(), ins.c());
            let target = |offset: i32| pc as i64 + 1 + offset as i64;
            let operands = match op {
                Op::LoadNil | Op::Close => format!("R{a}"),
                Op::LoadB if c != 0 => format!("R{a} {} skip", b != 0),
                Op::LoadB => format!("R{a} {}", b != 0),
                Op::LoadN => format!("R{a} {}", ins.sbx()),
                Op::LoadK | Op::GetGlobal | Op::SetGlobal => format!("R{a} K{}", ins.bx()),
                Op::Move | Op::Unm | Op::Not | Op::Len => format!("R{a} R{b}"),
                Op::Add
                | Op::Sub
                | Op::Mul
                | Op::Div
                | Op::IDiv
                | Op::Mod
                | Op::Pow
                | Op::Concat => {
                    format!("R{a} R{b} R{c}")
                }
                Op::AddK | Op::SubK | Op::MulK | Op::DivK | Op::IDivK | Op::ModK | Op::PowK => {
                    format!("R{a} R{b} K{c}")
                }
                Op::Eq | Op::Lt | Op::Le => format!("{} R{b} R{c}", a != 0),
                Op::EqK | Op::LtK | Op::LeK | Op::GtK | Op::GeK => {
                    format!("{} R{b} K{c}", a != 0)
                }
                Op::Jmp => format!("-> {}", target(ins.sax())),
                Op::JmpIf | Op::JmpIfNot | Op::ForPrep | Op::ForLoop => {
                    format!("R{a} -> {}", target(ins.sbx() as i32))
                }
                Op::GetUpval | Op::SetUpval => format!("R{a} U{b}"),
                Op::Closure => format!("R{a} P{}", ins.bx()),
                Op::Interp => format!("R{a} R{b} {c}"),
                Op::TailCall => {
                    let args = if b == 0 {
                        "...".to_string()
                    } else {
                        (b - 1).to_string()
                    };
                    format!("R{a} {args}")
                }
                Op::Call => {
                    let args = if b == 0 {
                        "...".to_string()
                    } else {
                        (b - 1).to_string()
                    };
                    let results = if c == 0 {
                        "...".to_string()
                    } else {
                        (c - 1).to_string()
                    };
                    format!("R{a} {args} -> {results}")
                }
                Op::Vararg => {
                    if b == 0 {
                        format!("R{a} ...")
                    } else {
                        format!("R{a} {}", b - 1)
                    }
                }
                Op::NewTable => format!("R{a} {b} {c}"),
                Op::DupTable => format!("R{a} T{}", ins.bx()),
                Op::GetTable | Op::SetTable => format!("R{a} R{b} R{c}"),
                Op::GetField | Op::SelfOp => format!("R{a} R{b} K{c}"),
                Op::SetField => format!("R{a} K{b} R{c}"),
                Op::SetList if b == 0 => format!("R{a} ..."),
                Op::SetList => format!("R{a} {b}"),
                Op::Data => format!("{}", ins.sax()),
                Op::TForCall => format!("R{a} {c}"),
                Op::TForLoop => format!("R{a} -> {}", target(ins.sbx() as i32)),
                Op::Return if c != 0 => format!("R{a} ..."),
                Op::Return => format!("R{a} {b}"),
            };
            let comment = match op {
                Op::LoadK | Op::GetGlobal | Op::SetGlobal => {
                    self.constant_comment(ins.bx() as usize)
                }
                Op::AddK
                | Op::SubK
                | Op::MulK
                | Op::DivK
                | Op::IDivK
                | Op::ModK
                | Op::PowK
                | Op::EqK
                | Op::LtK
                | Op::LeK
                | Op::GtK
                | Op::GeK => self.constant_comment(c as usize),
                Op::GetField | Op::SelfOp => self.constant_comment(c as usize),
                Op::SetField => self.constant_comment(b as usize),
                Op::GetUpval | Op::SetUpval => self
                    .upvalues
                    .get(b as usize)
                    .map_or_else(String::new, |u| u.name.to_string()),
                Op::DupTable => self.template_comment(ins.bx() as usize),
                Op::Closure => self
                    .protos
                    .get(ins.bx() as usize)
                    .map_or_else(String::new, |p| p.display_name().to_string()),
                _ => String::new(),
            };
            let line = format!("  {pc:>4}  {:<10}{operands}", op.mnemonic());
            if comment.is_empty() {
                let _ = writeln!(out, "{line}");
            } else {
                let _ = writeln!(out, "{line:<34}; {comment}");
            }
        }
        for child in &self.protos {
            let _ = writeln!(out);
            child.disassemble_into(out, false);
        }
    }

    /// `{x, y}`, the template's keys
    fn template_comment(&self, t: usize) -> String {
        let Some(t) = self.templates.get(t) else {
            return "<missing template>".into();
        };
        let keys: Vec<String> = t
            .keys
            .iter()
            .map(|&k| match self.constants.get(k as usize) {
                Some(Constant::String(s)) => s.to_string(),
                _ => "?".into(),
            })
            .collect();
        format!("{{{}}}", keys.join(", "))
    }

    fn constant_comment(&self, k: usize) -> String {
        match self.constants.get(k) {
            Some(Constant::Number(n)) => format_number(*n),
            Some(Constant::String(s)) => format!("{s:?}"),
            None => "<missing constant>".into(),
        }
    }
}
