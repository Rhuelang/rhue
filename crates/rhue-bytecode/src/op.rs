use crate::ArithOp;

macro_rules! opcodes {
    ($( $(#[$doc:meta])* $name:ident = $mnemonic:literal ),* $(,)?) => {
        /// Every opcode. The comment on each one gives its operands, `R(x)` is
        /// register x and `K(x)` is constant x.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Op { $( $(#[$doc])* $name ),* }

        impl Op {
            pub const ALL: &'static [Op] = &[ $( Op::$name ),* ];

            /// Decodes an opcode byte. It's a match on each opcode's own value
            /// so the optimiser sees the identity mapping (a lookup table would
            /// cost a load per instruction in the VM).
            #[inline]
            pub fn from_u8(byte: u8) -> Option<Op> {
                #[allow(non_upper_case_globals)]
                mod byte_of {
                    use super::Op;
                    $( pub const $name: u8 = Op::$name as u8; )*
                }
                match byte {
                    $( byte_of::$name => Some(Op::$name), )*
                    _ => None,
                }
            }

            pub fn mnemonic(self) -> &'static str {
                match self { $( Op::$name => $mnemonic ),* }
            }
        }
    };
}

opcodes! {
    /// `R(A) := nil`
    LoadNil = "LOADNIL",
    /// `R(A) := (B != 0); if C != 0 then pc++`
    LoadB = "LOADB",
    /// `R(A) := sBx` (small integral numbers)
    LoadN = "LOADN",
    /// `R(A) := K(Bx)`
    LoadK = "LOADK",
    /// `R(A) := R(B)`
    Move = "MOVE",
    /// `R(A) := globals[K(Bx)]`
    GetGlobal = "GETGLOBAL",
    /// `globals[K(Bx)] := R(A)`
    SetGlobal = "SETGLOBAL",

    /// `R(A) := R(B) + R(C)`
    Add = "ADD",
    Sub = "SUB",
    Mul = "MUL",
    Div = "DIV",
    IDiv = "IDIV",
    Mod = "MOD",
    Pow = "POW",

    /// `R(A) := R(B) + K(C)`
    AddK = "ADDK",
    SubK = "SUBK",
    MulK = "MULK",
    DivK = "DIVK",
    IDivK = "IDIVK",
    ModK = "MODK",
    PowK = "POWK",

    /// `R(A) := -R(B)`
    Unm = "UNM",
    /// `R(A) := not R(B)`
    Not = "NOT",
    /// `R(A) := #R(B)`
    Len = "LEN",
    /// `R(A) := R(B) .. R(C)`
    Concat = "CONCAT",

    /// `if (R(B) == R(C)) != (A != 0) then pc++` (the next instruction is a `JMP`)
    Eq = "EQ",
    /// `if (R(B) < R(C)) != (A != 0) then pc++`
    Lt = "LT",
    /// `if (R(B) <= R(C)) != (A != 0) then pc++`
    Le = "LE",
    /// `if (R(B) == K(C)) != (A != 0) then pc++` (same for the ones below,
    /// constant on the right. `a > b` means `b < a` for errors)
    EqK = "EQK",
    LtK = "LTK",
    LeK = "LEK",
    GtK = "GTK",
    GeK = "GEK",

    /// `pc += sAx` (24-bit signed offset, relative to the next instruction)
    Jmp = "JMP",
    /// `if R(A) is truthy then pc += sBx`
    JmpIf = "JMPIF",
    /// `if R(A) is falsy then pc += sBx`
    JmpIfNot = "JMPIFNOT",

    /// Numeric `for` setup. `R(A)` index, `R(A+1)` limit, `R(A+2)` step (all
    /// checked to be numbers), `R(A+3)` the visible loop variable. Runs zero
    /// times: `pc += sBx`. Otherwise `R(A+3) := R(A)`.
    ForPrep = "FORPREP",
    /// `R(A) += R(A+2)`, and if it's still in range, `R(A+3) := R(A)` and `pc
    /// += sBx`.
    ForLoop = "FORLOOP",

    /// `R(A) := upvalues[B]`
    GetUpval = "GETUPVAL",
    /// `upvalues[B] := R(A)`
    SetUpval = "SETUPVAL",
    /// `R(A) := closure(protos[Bx])`, capturing upvalues per the child's
    /// `upvalues` descriptors.
    Closure = "CLOSURE",
    /// Closes every open upvalue pointing at `R(A)` or above.
    Close = "CLOSE",
    /// `R(A), ..., R(A+C-2) := R(A)(R(A+1), ..., R(A+B-1))`. `B = 0`: arguments
    /// go up to `top`. `C = 0`: keep every result and set `top` past the last
    /// one.
    Call = "CALL",
    /// `R(A) := tostring(R(B)) .. ... .. tostring(R(B+C-1))`, for interpolated
    /// strings (`__tostring` applies).
    Interp = "INTERP",
    /// `return R(A)(R(A+1), ..., R(A+B-1))`. A Lua callee replaces the current
    /// frame (so tail recursion doesn't use stack), anything else gets called
    /// like `CALL A B 0` and the `RETURN A 0 1` that always follows returns its
    /// results.
    TailCall = "TAILCALL",
    /// `R(A), ..., R(A+B-2) := ...`. `B = 0`: copy all the varargs and set
    /// `top`.
    Vararg = "VARARG",

    /// `R(A) := {}` with room for `B` array and `C` hash entries (just hints).
    NewTable = "NEWTABLE",
    /// `R(A) := R(B)[R(C)]`
    GetTable = "GETTABLE",
    /// `R(A) := R(B)[K(C)]` (constant string key)
    GetField = "GETFIELD",
    /// `R(A)[R(B)] := R(C)`
    SetTable = "SETTABLE",
    /// `R(A)[K(B)] := R(C)` (constant string key)
    SetField = "SETFIELD",
    /// `R(A+1) := R(B); R(A) := R(B)[K(C)]` (method lookup for `obj:m()`)
    SelfOp = "SELF",
    /// `R(A)[n + i] := R(A + i)` for `i` in `1..=B` (`B = 0`: up to `top`),
    /// where `n` is the start offset in the `DATA` word after it.
    SetList = "SETLIST",
    /// Never executed, it's an operand word for the instruction before it.
    Data = "DATA",
    /// Generic-for step: `R(A+3), ..., R(A+2+C) := R(A)(R(A+1), R(A+2))`. If
    /// `R(A)` is a table it gets iterated directly (Luau's generalized
    /// iteration, like `next`).
    TForCall = "TFORCALL",
    /// `if R(A+3) ~= nil then R(A+2) := R(A+3); pc += sBx`
    TForLoop = "TFORLOOP",

    /// `return R(A), ..., R(A+B-1)`, or if `C != 0` return `R(A)` up to `top`.
    Return = "RETURN",
    /// `R(A) := {}` shaped by `templates[Bx]`. Room for its array and hash
    /// sizes with its keys already in there (as nil), so a constructor's
    /// `SETFIELD`s fill existing slots through their inline caches.
    DupTable = "DUPTABLE",
}

impl Op {
    /// The register-register opcode for an arithmetic operator.
    pub fn arith_rr(op: ArithOp) -> Op {
        match op {
            ArithOp::Add => Op::Add,
            ArithOp::Sub => Op::Sub,
            ArithOp::Mul => Op::Mul,
            ArithOp::Div => Op::Div,
            ArithOp::IDiv => Op::IDiv,
            ArithOp::Mod => Op::Mod,
            ArithOp::Pow => Op::Pow,
        }
    }

    /// The register-constant opcode for an arithmetic operator.
    pub fn arith_rk(op: ArithOp) -> Op {
        match op {
            ArithOp::Add => Op::AddK,
            ArithOp::Sub => Op::SubK,
            ArithOp::Mul => Op::MulK,
            ArithOp::Div => Op::DivK,
            ArithOp::IDiv => Op::IDivK,
            ArithOp::Mod => Op::ModK,
            ArithOp::Pow => Op::PowK,
        }
    }

    /// Inverse of [`Op::arith_rr`] / [`Op::arith_rk`].
    pub fn as_arith(self) -> Option<ArithOp> {
        Some(match self {
            Op::Add | Op::AddK => ArithOp::Add,
            Op::Sub | Op::SubK => ArithOp::Sub,
            Op::Mul | Op::MulK => ArithOp::Mul,
            Op::Div | Op::DivK => ArithOp::Div,
            Op::IDiv | Op::IDivK => ArithOp::IDiv,
            Op::Mod | Op::ModK => ArithOp::Mod,
            Op::Pow | Op::PowK => ArithOp::Pow,
            _ => return None,
        })
    }
}
