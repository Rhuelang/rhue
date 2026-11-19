//! `bit32`, same as Luau: bitwise ops on numbers treated as unsigned 32-bit
//! integers (truncated toward zero, then wrapped mod 2^32).

use std::rc::Rc;

use smallvec::smallvec;

use crate::stdlib::num;
use crate::{NativeError, Rets, Value, Vm};

type R = Result<Rets, NativeError>;

/// A number as `bit32` sees it.
pub(crate) fn to_u32(n: f64) -> u32 {
    // rust's float-to-int casts saturate (NaN is 0), wrapping through i64
    // matches Luau for everything an integer can hold
    (n.trunc() as i64) as u32
}

fn u(args: &[Value], i: usize, f: &str) -> Result<u32, NativeError> {
    num(args, i, f).map(to_u32)
}

fn ret(x: u32) -> R {
    Ok(smallvec![Value::Number(f64::from(x))])
}

fn fold(args: &[Value], f: &str, init: u32, op: fn(u32, u32) -> u32) -> Result<u32, NativeError> {
    let mut acc = init;
    for i in 0..args.len() {
        acc = op(acc, u(args, i, f)?);
    }
    Ok(acc)
}

/// Shifts left by `d` (right when it's negative), 0 past 31 bits.
fn shift(x: u32, d: i64) -> u32 {
    match d {
        0 => x,
        d if d <= -32 || d >= 32 => 0,
        d if d > 0 => x << d,
        d => x >> -d,
    }
}

/// `field` and `width` checked the way Luau does.
fn field(args: &[Value], fi: usize, f: &str) -> Result<(u32, u32), NativeError> {
    let fld = num(args, fi, f)?.trunc() as i64;
    let width = match args.get(fi + 1) {
        None | Some(Value::Nil) => 1,
        Some(_) => num(args, fi + 1, f)?.trunc() as i64,
    };
    if fld < 0 {
        return Err(NativeError::Message(format!(
            "invalid argument #{} to '{f}' (field cannot be negative)",
            fi + 1
        )));
    }
    if width <= 0 {
        return Err(NativeError::Message(format!(
            "invalid argument #{} to '{f}' (width must be positive)",
            fi + 2
        )));
    }
    if fld + width > 32 {
        return Err(NativeError::Message(
            "trying to access non-existent bits".into(),
        ));
    }
    Ok((fld as u32, width as u32))
}

fn mask(width: u32) -> u32 {
    if width >= 32 {
        u32::MAX
    } else {
        (1 << width) - 1
    }
}

pub(crate) fn install(vm: &mut Vm) {
    let entries: &[(&str, crate::NativeFn)] = &[
        (
            "band",
            Rc::new(|_, a| ret(fold(a, "band", u32::MAX, |x, y| x & y)?)),
        ),
        ("bor", Rc::new(|_, a| ret(fold(a, "bor", 0, |x, y| x | y)?))),
        (
            "bxor",
            Rc::new(|_, a| ret(fold(a, "bxor", 0, |x, y| x ^ y)?)),
        ),
        (
            "btest",
            Rc::new(|_, a| {
                let x = fold(a, "btest", u32::MAX, |x, y| x & y)?;
                Ok(smallvec![Value::Bool(x != 0)])
            }),
        ),
        ("bnot", Rc::new(|_, a| ret(!u(a, 0, "bnot")?))),
        (
            "lshift",
            Rc::new(|_, a| ret(shift(u(a, 0, "lshift")?, num(a, 1, "lshift")? as i64))),
        ),
        (
            "rshift",
            Rc::new(|_, a| ret(shift(u(a, 0, "rshift")?, -(num(a, 1, "rshift")? as i64)))),
        ),
        (
            "arshift",
            Rc::new(|_, a| {
                let x = u(a, 0, "arshift")?;
                let d = num(a, 1, "arshift")? as i64;
                if d < 0 || x & 0x8000_0000 == 0 {
                    return ret(shift(x, -d));
                }
                // negative, shift in ones
                ret(if d >= 32 {
                    u32::MAX
                } else {
                    ((x as i32) >> d) as u32
                })
            }),
        ),
        (
            "lrotate",
            Rc::new(|_, a| {
                let d = (num(a, 1, "lrotate")? as i64).rem_euclid(32) as u32;
                ret(u(a, 0, "lrotate")?.rotate_left(d))
            }),
        ),
        (
            "rrotate",
            Rc::new(|_, a| {
                let d = (num(a, 1, "rrotate")? as i64).rem_euclid(32) as u32;
                ret(u(a, 0, "rrotate")?.rotate_right(d))
            }),
        ),
        (
            "extract",
            Rc::new(|_, a| {
                let x = u(a, 0, "extract")?;
                let (f, w) = field(a, 1, "extract")?;
                ret((x >> f) & mask(w))
            }),
        ),
        (
            "replace",
            Rc::new(|_, a| {
                let x = u(a, 0, "replace")?;
                let v = u(a, 1, "replace")?;
                let (f, w) = field(a, 2, "replace")?;
                let m = mask(w);
                ret((x & !(m << f)) | ((v & m) << f))
            }),
        ),
        (
            "countlz",
            Rc::new(|_, a| ret(u(a, 0, "countlz")?.leading_zeros())),
        ),
        (
            "countrz",
            Rc::new(|_, a| ret(u(a, 0, "countrz")?.trailing_zeros())),
        ),
        (
            "byteswap",
            Rc::new(|_, a| ret(u(a, 0, "byteswap")?.swap_bytes())),
        ),
    ];
    let lib = crate::stdlib::library(vm, entries);
    vm.set_global("bit32", lib);
}
