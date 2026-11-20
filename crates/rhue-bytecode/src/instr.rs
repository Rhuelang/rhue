use std::fmt;

use crate::Op;

/// A 32-bit instruction.
///
/// ```text
///  31      24 23      16 15       8 7        0
/// +----------+----------+----------+----------+
/// |    C     |    B     |    A     |    op    |   ABC
/// +----------+----------+----------+----------+
/// |        Bx / sBx     |    A     |    op    |   ABx / AsBx
/// +---------------------+----------+----------+
/// |              sAx               |    op    |   sAx (JMP)
/// +--------------------------------+----------+
/// ```
///
/// The opcode's in the low byte (like Luau), so decoding it is one `as u8`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Instr(pub u32);

impl Instr {
    pub const fn abc(op: Op, a: u8, b: u8, c: u8) -> Self {
        Instr(op as u32 | (a as u32) << 8 | (b as u32) << 16 | (c as u32) << 24)
    }

    pub const fn abx(op: Op, a: u8, bx: u16) -> Self {
        Instr(op as u32 | (a as u32) << 8 | (bx as u32) << 16)
    }

    pub const fn asbx(op: Op, a: u8, sbx: i16) -> Self {
        Self::abx(op, a, sbx as u16)
    }

    /// Largest |offset| a `JMP` can encode (24-bit signed).
    pub const MAX_SAX: i32 = (1 << 23) - 1;

    /// A 24-bit signed operand in the A, B and C fields (used by `JMP`).
    pub const fn asax(op: Op, sax: i32) -> Self {
        Instr(op as u32 | ((sax as u32) << 8))
    }

    /// The opcode, or `None` if the instruction's corrupt.
    #[inline]
    pub fn op(self) -> Option<Op> {
        Op::from_u8(self.0.to_le_bytes()[0])
    }

    #[inline]
    pub const fn a(self) -> u8 {
        self.0.to_le_bytes()[1]
    }

    #[inline]
    pub const fn b(self) -> u8 {
        self.0.to_le_bytes()[2]
    }

    #[inline]
    pub const fn c(self) -> u8 {
        (self.0 >> 24) as u8
    }

    #[inline]
    pub const fn bx(self) -> u16 {
        (self.0 >> 16) as u16
    }

    #[inline]
    pub const fn sbx(self) -> i16 {
        self.bx() as i16
    }

    /// arithmetic shift sign-extends the top 24 bits
    #[inline]
    pub const fn sax(self) -> i32 {
        (self.0 as i32) >> 8
    }
}

impl fmt::Debug for Instr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.op() {
            Some(op) => write!(
                f,
                "{} {} {} {}",
                op.mnemonic(),
                self.a(),
                self.b(),
                self.c()
            ),
            None => write!(f, "<bad {:#010x}>", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abc_roundtrip() {
        let i = Instr::abc(Op::AddK, 1, 200, 255);
        assert_eq!(i.op(), Some(Op::AddK));
        assert_eq!((i.a(), i.b(), i.c()), (1, 200, 255));
    }

    #[test]
    fn bx_roundtrip() {
        let i = Instr::abx(Op::LoadK, 7, 65535);
        assert_eq!((i.op(), i.a(), i.bx()), (Some(Op::LoadK), 7, 65535));
    }

    #[test]
    fn sbx_roundtrip() {
        for v in [i16::MIN, -1, 0, 1, i16::MAX] {
            let i = Instr::asbx(Op::LoadN, 3, v);
            assert_eq!((i.a(), i.sbx()), (3, v));
        }
    }

    #[test]
    fn sax_roundtrip() {
        for v in [-Instr::MAX_SAX, -1, 0, 1, 1000, Instr::MAX_SAX] {
            let i = Instr::asax(Op::Jmp, v);
            assert_eq!((i.op(), i.sax()), (Some(Op::Jmp), v));
        }
    }

    #[test]
    fn bad_opcode() {
        assert_eq!(Instr(0xff).op(), None);
    }

    #[test]
    fn every_opcode_roundtrips_through_u8() {
        for (i, op) in Op::ALL.iter().enumerate() {
            assert_eq!(*op as usize, i);
            assert_eq!(Op::from_u8(u8::try_from(i).unwrap()), Some(*op));
        }
    }
}
