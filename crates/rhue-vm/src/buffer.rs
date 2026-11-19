//! `buffer`, same as Luau: fixed-size mutable byte arrays. Offsets count from
//! 0, numbers are little-endian, and every access is bounds-checked. Integers
//! get wrapped to their width when written (`writeu8(b, 0, 256)` stores 0).
//! Rhue strings are UTF-8, so `tostring` and `readstring` refuse bytes that
//! aren't.

use std::rc::Rc;

use smallvec::smallvec;

use crate::heap::BufRef;
use crate::stdlib::{bad_arg, num, text_arg};
use crate::{NativeCtx, NativeError, Rets, Value, Vm};

type R = Result<Rets, NativeError>;

/// The biggest buffer `create` will make, same as Luau.
pub const MAX_BUFFER: usize = 1 << 30;

fn buf(args: &[Value], i: usize, f: &str) -> Result<BufRef, NativeError> {
    match args.get(i).copied().unwrap_or(Value::Nil) {
        Value::Buffer(b) => Ok(b),
        other => Err(bad_arg(i + 1, f, "buffer", other)),
    }
}

/// An integer argument (truncated toward zero, like Luau).
fn int(args: &[Value], i: usize, f: &str) -> Result<i64, NativeError> {
    Ok(num(args, i, f)?.trunc() as i64)
}

fn opt_int(args: &[Value], i: usize, f: &str) -> Result<Option<i64>, NativeError> {
    match args.get(i) {
        None | Some(Value::Nil) => Ok(None),
        _ => int(args, i, f).map(Some),
    }
}

fn out_of_bounds() -> NativeError {
    NativeError::Message("buffer access out of bounds".into())
}

/// `[offset, offset + count)` inside a buffer of `len` bytes.
fn range(len: usize, offset: i64, count: i64) -> Result<std::ops::Range<usize>, NativeError> {
    if offset < 0 || count < 0 {
        return Err(out_of_bounds());
    }
    let (o, c) = (offset as usize, count as usize);
    match o.checked_add(c) {
        Some(end) if end <= len => Ok(o..end),
        _ => Err(out_of_bounds()),
    }
}

fn number(n: f64) -> R {
    Ok(smallvec![Value::Number(n)])
}

/// `buffer.readT` / `buffer.writeT` for one numeric type.
macro_rules! numeric {
    ($entries:ident, $read:literal, $write:literal, $t:ty, int) => {
        $entries.push((
            $read,
            Rc::new(|ctx: &mut NativeCtx<'_>, a: &[Value]| {
                let b = buf(a, 0, $read)?;
                let bytes = ctx.heap().buffer(b);
                let r = range(bytes.len(), int(a, 1, $read)?, size_of::<$t>() as i64)?;
                let v = <$t>::from_le_bytes(bytes[r].try_into().expect("sized"));
                number(v as f64)
            }),
        ));
        $entries.push((
            $write,
            Rc::new(|ctx: &mut NativeCtx<'_>, a: &[Value]| {
                let b = buf(a, 0, $write)?;
                let offset = int(a, 1, $write)?;
                // wrapped through a 64-bit integer, that's how Luau converts
                let v = num(a, 2, $write)?.trunc() as i64 as $t;
                let bytes = ctx.heap_mut().buffer_mut(b);
                let r = range(bytes.len(), offset, size_of::<$t>() as i64)?;
                bytes[r].copy_from_slice(&v.to_le_bytes());
                Ok(smallvec![])
            }),
        ));
    };
    ($entries:ident, $read:literal, $write:literal, $t:ty, float) => {
        $entries.push((
            $read,
            Rc::new(|ctx: &mut NativeCtx<'_>, a: &[Value]| {
                let b = buf(a, 0, $read)?;
                let bytes = ctx.heap().buffer(b);
                let r = range(bytes.len(), int(a, 1, $read)?, size_of::<$t>() as i64)?;
                number(<$t>::from_le_bytes(bytes[r].try_into().expect("sized")) as f64)
            }),
        ));
        $entries.push((
            $write,
            Rc::new(|ctx: &mut NativeCtx<'_>, a: &[Value]| {
                let b = buf(a, 0, $write)?;
                let offset = int(a, 1, $write)?;
                let v = num(a, 2, $write)? as $t;
                let bytes = ctx.heap_mut().buffer_mut(b);
                let r = range(bytes.len(), offset, size_of::<$t>() as i64)?;
                bytes[r].copy_from_slice(&v.to_le_bytes());
                Ok(smallvec![])
            }),
        ));
    };
}

fn utf8(bytes: &[u8], f: &str) -> Result<String, NativeError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        NativeError::Message(format!(
            "'{f}' needs the bytes to be valid UTF-8 (Rhue strings are)"
        ))
    })
}

fn create(ctx: &mut NativeCtx<'_>, a: &[Value]) -> R {
    let size = int(a, 0, "create")?;
    if size < 0 || size as usize > MAX_BUFFER {
        return Err(format!(
            "invalid argument #1 to 'create' (size must be between 0 and {MAX_BUFFER})"
        )
        .into());
    }
    ctx.reserve(size as usize)?;
    let b = ctx
        .heap_mut()
        .alloc_buffer(vec![0u8; size as usize].into_boxed_slice());
    Ok(smallvec![Value::Buffer(b)])
}

fn fromstring(ctx: &mut NativeCtx<'_>, a: &[Value]) -> R {
    let s = text_arg(ctx, a, 0, "fromstring")?;
    ctx.reserve(s.len())?;
    let b = ctx
        .heap_mut()
        .alloc_buffer(s.into_bytes().into_boxed_slice());
    Ok(smallvec![Value::Buffer(b)])
}

fn tostring(ctx: &mut NativeCtx<'_>, a: &[Value]) -> R {
    let b = buf(a, 0, "tostring")?;
    let s = utf8(ctx.heap().buffer(b), "tostring")?;
    Ok(smallvec![ctx.string(&s)])
}

fn len(ctx: &mut NativeCtx<'_>, a: &[Value]) -> R {
    let b = buf(a, 0, "len")?;
    number(ctx.heap().buffer(b).len() as f64)
}

fn readstring(ctx: &mut NativeCtx<'_>, a: &[Value]) -> R {
    let b = buf(a, 0, "readstring")?;
    let (offset, count) = (int(a, 1, "readstring")?, int(a, 2, "readstring")?);
    let bytes = ctx.heap().buffer(b);
    let r = range(bytes.len(), offset, count)?;
    let s = utf8(&bytes[r], "readstring")?;
    Ok(smallvec![ctx.string(&s)])
}

fn writestring(ctx: &mut NativeCtx<'_>, a: &[Value]) -> R {
    let b = buf(a, 0, "writestring")?;
    let offset = int(a, 1, "writestring")?;
    let s = text_arg(ctx, a, 2, "writestring")?;
    let count = opt_int(a, 3, "writestring")?.unwrap_or(s.len() as i64);
    if count < 0 || count as usize > s.len() {
        return Err("invalid argument #4 to 'writestring' (string length overflow)".into());
    }
    let bytes = ctx.heap_mut().buffer_mut(b);
    let r = range(bytes.len(), offset, count)?;
    bytes[r].copy_from_slice(&s.as_bytes()[..count as usize]);
    Ok(smallvec![])
}

fn copy(ctx: &mut NativeCtx<'_>, a: &[Value]) -> R {
    let target = buf(a, 0, "copy")?;
    let t_off = int(a, 1, "copy")?;
    let source = buf(a, 2, "copy")?;
    let s_off = opt_int(a, 3, "copy")?.unwrap_or(0);
    let s_len = ctx.heap().buffer(source).len();
    let count = opt_int(a, 4, "copy")?.unwrap_or(s_len as i64 - s_off);
    let from = range(s_len, s_off, count)?;
    let to = range(ctx.heap().buffer(target).len(), t_off, count)?;
    if target == source {
        ctx.heap_mut()
            .buffer_mut(target)
            .copy_within(from, to.start);
    } else {
        let bytes = ctx.heap().buffer(source)[from].to_vec();
        ctx.heap_mut().buffer_mut(target)[to].copy_from_slice(&bytes);
    }
    Ok(smallvec![])
}

fn fill(ctx: &mut NativeCtx<'_>, a: &[Value]) -> R {
    let b = buf(a, 0, "fill")?;
    let offset = int(a, 1, "fill")?;
    let value = num(a, 2, "fill")?.trunc() as i64 as u8;
    let len = ctx.heap().buffer(b).len();
    let count = opt_int(a, 3, "fill")?.unwrap_or(len as i64 - offset);
    let r = range(len, offset, count)?;
    ctx.heap_mut().buffer_mut(b)[r].fill(value);
    Ok(smallvec![])
}

/// The bit range `[offset, offset + count)` (count 32 at most), the bytes it
/// covers.
fn bits(
    len: usize,
    a: &[Value],
    f: &str,
) -> Result<(usize, u32, std::ops::Range<usize>), NativeError> {
    let offset = int(a, 1, f)?;
    let count = int(a, 2, f)?;
    if !(0..=32).contains(&count) {
        return Err(
            format!("invalid argument #3 to '{f}' (bit count must be in [0, 32] range)").into(),
        );
    }
    if offset < 0 || (offset as u64 + count as u64) > len as u64 * 8 {
        return Err(out_of_bounds());
    }
    let first = offset as usize / 8;
    let last = (offset as usize + count as usize).div_ceil(8);
    Ok((offset as usize % 8, count as u32, first..last.max(first)))
}

fn readbits(ctx: &mut NativeCtx<'_>, a: &[Value]) -> R {
    let b = buf(a, 0, "readbits")?;
    let bytes = ctx.heap().buffer(b);
    let (shift, count, r) = bits(bytes.len(), a, "readbits")?;
    let mut word = 0u64;
    for (i, &byte) in bytes[r].iter().enumerate() {
        word |= u64::from(byte) << (8 * i);
    }
    let mask = if count == 32 {
        u64::from(u32::MAX)
    } else {
        (1u64 << count) - 1
    };
    number(((word >> shift) & mask) as f64)
}

fn writebits(ctx: &mut NativeCtx<'_>, a: &[Value]) -> R {
    let b = buf(a, 0, "writebits")?;
    let value = crate::bitlib::to_u32(num(a, 3, "writebits")?);
    let len = ctx.heap().buffer(b).len();
    let (shift, count, r) = bits(len, a, "writebits")?;
    let bytes = &mut ctx.heap_mut().buffer_mut(b)[r];
    let mut word = 0u64;
    for (i, &byte) in bytes.iter().enumerate() {
        word |= u64::from(byte) << (8 * i);
    }
    let mask = if count == 32 {
        u64::from(u32::MAX)
    } else {
        (1u64 << count) - 1
    };
    word = (word & !(mask << shift)) | ((u64::from(value) & mask) << shift);
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = (word >> (8 * i)) as u8;
    }
    Ok(smallvec![])
}

pub(crate) fn install(vm: &mut Vm) {
    let mut entries: Vec<(&str, crate::NativeFn)> = vec![
        ("create", Rc::new(create)),
        ("fromstring", Rc::new(fromstring)),
        ("tostring", Rc::new(tostring)),
        ("len", Rc::new(len)),
        ("readstring", Rc::new(readstring)),
        ("writestring", Rc::new(writestring)),
        ("copy", Rc::new(copy)),
        ("fill", Rc::new(fill)),
        ("readbits", Rc::new(readbits)),
        ("writebits", Rc::new(writebits)),
    ];
    numeric!(entries, "readi8", "writei8", i8, int);
    numeric!(entries, "readu8", "writeu8", u8, int);
    numeric!(entries, "readi16", "writei16", i16, int);
    numeric!(entries, "readu16", "writeu16", u16, int);
    numeric!(entries, "readi32", "writei32", i32, int);
    numeric!(entries, "readu32", "writeu32", u32, int);
    numeric!(entries, "readf32", "writef32", f32, float);
    numeric!(entries, "readf64", "writef64", f64, float);
    let lib = crate::stdlib::library(vm, &entries);
    vm.set_global("buffer", lib);
}
