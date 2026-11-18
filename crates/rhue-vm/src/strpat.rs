//! The pattern-matching half of the `string` library: `find`, `match`,
//! `gmatch` and `gsub` (see [`crate::pattern`] for the patterns).
//!
//! Positions are byte offsets, as in Lua. Captured text that cuts through
//! a multi-byte character comes out with U+FFFD in its place, like
//! `string.sub`.

use std::cell::Cell;
use std::rc::Rc;

use smallvec::smallvec;

use crate::pattern::{self, Capture, Matcher};
use crate::{NativeCtx, NativeError, Rets, Value};

use crate::stdlib::{bad_arg, opt_int, text_arg};

type R = Result<Rets, NativeError>;

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn capture_value(ctx: &mut NativeCtx<'_>, src: &[u8], c: Capture) -> Value {
    match c {
        Capture::Str(a, b) => ctx.string(&text(&src[a..b])),
        Capture::Pos(p) => Value::Number((p + 1) as f64),
    }
}

/// Lua 5.4's `init` for `find`/`match`/`gmatch`: 1-based, negative from
/// the end. `None` when it starts past the end (no match possible).
fn start_index(len: usize, init: i64) -> Option<usize> {
    let len = len as i64;
    let i = if init > 0 {
        init
    } else if init == 0 || -init > len {
        1
    } else {
        len + init + 1
    };
    (i <= len + 1).then_some((i - 1) as usize)
}

fn find_aux(ctx: &mut NativeCtx<'_>, args: &[Value], find: bool) -> R {
    let fname = if find { "find" } else { "match" };
    let s = text_arg(ctx, args, 0, fname)?;
    let pat = text_arg(ctx, args, 1, fname)?;
    let init = opt_int(args, 2, fname, 1)?;
    let Some(start) = start_index(s.len(), init) else {
        return Ok(smallvec![Value::Nil]);
    };
    let plain = find && args.get(3).is_some_and(|v| v.is_truthy());
    if find && (plain || !pattern::has_specials(pat.as_bytes())) {
        return Ok(
            match s.as_bytes()[start..]
                .windows(pat.len().max(1))
                .position(|w| w == pat.as_bytes())
                .or_else(|| pat.is_empty().then_some(0))
            {
                Some(i) => smallvec![
                    Value::Number((start + i + 1) as f64),
                    Value::Number((start + i + pat.len()) as f64),
                ],
                None => smallvec![Value::Nil],
            },
        );
    }
    let src = s.as_bytes();
    match pattern::find(src, pat.as_bytes(), start)? {
        Some((a, b, mut caps)) => {
            let mut out = Rets::new();
            if find {
                out.push(Value::Number((a + 1) as f64));
                out.push(Value::Number(b as f64));
            } else if caps.is_empty() {
                caps.push(Capture::Str(a, b));
            }
            for c in caps {
                out.push(capture_value(ctx, src, c));
            }
            Ok(out)
        }
        None => Ok(smallvec![Value::Nil]),
    }
}

pub(crate) fn string_find(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    find_aux(ctx, args, true)
}

pub(crate) fn string_match(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    find_aux(ctx, args, false)
}

/// `string.gmatch(s, pattern [, init])`: an iterator over the matches.
pub(crate) fn string_gmatch(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let s = text_arg(ctx, args, 0, "gmatch")?;
    let pat = text_arg(ctx, args, 1, "gmatch")?;
    let init = opt_int(args, 2, "gmatch", 1)?;
    let start = start_index(s.len(), init).unwrap_or(s.len() + 1);
    let src: Rc<[u8]> = Rc::from(s.into_bytes());
    let pat: Rc<[u8]> = Rc::from(pat.into_bytes());
    let pos = Cell::new(start);
    let last: Cell<Option<usize>> = Cell::new(None);
    let iter = move |ctx: &mut NativeCtx<'_>, _: &[Value]| -> R {
        let mut m = Matcher::new(&src, &pat);
        let mut s = pos.get();
        while s <= src.len() {
            if let Some(e) = m.try_at(s, 0)?
                && Some(e) != last.get()
            {
                pos.set(e);
                last.set(Some(e));
                let caps = m.captures(s, e)?;
                return Ok(caps
                    .into_iter()
                    .map(|c| capture_value(ctx, &src, c))
                    .collect());
            }
            s += 1;
        }
        pos.set(src.len() + 1);
        Ok(smallvec![Value::Nil])
    };
    Ok(smallvec![ctx.vm().native("gmatch_iter", Rc::new(iter))])
}

/// `string.gsub(s, pattern, repl [, n])`.
pub(crate) fn string_gsub(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let s = text_arg(ctx, args, 0, "gsub")?;
    let pat = text_arg(ctx, args, 1, "gsub")?;
    let repl = args.get(2).copied().unwrap_or(Value::Nil);
    let repl_text = match repl {
        Value::String(_) | Value::Number(_) => Some(text_arg(ctx, args, 2, "gsub")?),
        Value::Table(_) | Value::Function(_) => None,
        other => return Err(bad_arg(3, "gsub", "string/function/table", other)),
    };
    let src = s.as_bytes();
    let max = match args.get(3) {
        None | Some(Value::Nil) => usize::MAX,
        Some(_) => opt_int(args, 3, "gsub", 0)?.max(0) as usize,
    };
    let (anchor, p) = if pat.as_bytes().first() == Some(&b'^') {
        (true, 1)
    } else {
        (false, 0)
    };
    let mut m = Matcher::new(src, pat.as_bytes());
    let mut out: Vec<u8> = Vec::with_capacity(src.len());
    let (mut i, mut n) = (0usize, 0usize);
    let mut last: Option<usize> = None;
    while n < max {
        let found = m.try_at(i, p)?;
        match found {
            Some(e) if Some(e) != last => {
                n += 1;
                add_value(ctx, &m, src, i, e, repl, repl_text.as_deref(), &mut out)?;
                i = e;
                last = Some(e);
            }
            _ if i < src.len() => {
                out.push(src[i]);
                i += 1;
            }
            _ => break,
        }
        if anchor {
            break;
        }
    }
    out.extend_from_slice(&src[i.min(src.len())..]);
    ctx.reserve(out.len())?;
    let result = text(&out);
    Ok(smallvec![ctx.string(&result), Value::Number(n as f64)])
}

#[allow(clippy::too_many_arguments)]
fn add_value(
    ctx: &mut NativeCtx<'_>,
    m: &Matcher<'_>,
    src: &[u8],
    s: usize,
    e: usize,
    repl: Value,
    repl_text: Option<&str>,
    out: &mut Vec<u8>,
) -> Result<(), NativeError> {
    if let Some(r) = repl_text {
        let r = r.as_bytes();
        let mut k = 0;
        while k < r.len() {
            let c = r[k];
            k += 1;
            if c != b'%' {
                out.push(c);
                continue;
            }
            let d = r.get(k).copied().unwrap_or(0);
            k += 1;
            if d == b'%' {
                out.push(b'%');
            } else if d.is_ascii_digit() {
                let cap = if d == b'0' {
                    Capture::Str(s, e)
                } else {
                    m.capture_or_whole((d - b'1') as usize, s, e)?
                };
                match cap {
                    Capture::Str(a, b) => out.extend_from_slice(&src[a..b]),
                    Capture::Pos(p) => out.extend_from_slice((p + 1).to_string().as_bytes()),
                }
            } else {
                return Err("invalid use of '%' in replacement string".into());
            }
        }
        return Ok(());
    }
    let value = match repl {
        Value::Table(t) => {
            let key = capture_value(ctx, src, m.first_capture(s, e)?);
            ctx.heap().table(t).get(key)
        }
        _ => {
            let caps: Vec<Value> = m
                .captures(s, e)?
                .into_iter()
                .map(|c| capture_value(ctx, src, c))
                .collect();
            ctx.call(repl, &caps)?
                .first()
                .copied()
                .unwrap_or(Value::Nil)
        }
    };
    match value {
        Value::Nil | Value::Bool(false) => out.extend_from_slice(&src[s..e]),
        Value::String(r) => out.extend_from_slice(ctx.heap().get(r).as_bytes()),
        Value::Number(x) => out.extend_from_slice(rhue_bytecode::format_number(x).as_bytes()),
        other => {
            return Err(NativeError::Message(format!(
                "invalid replacement value (a {})",
                other.type_name()
            )));
        }
    }
    Ok(())
}
