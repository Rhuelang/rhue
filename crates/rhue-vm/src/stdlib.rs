//! The standard library: base functions plus the `table`, `string` and `math`
//! libraries (`coroutine` is in `coroutine.rs`). Error messages follow Luau's
//! wording.
//!
//! The pattern functions (`find`, `match`, `gmatch`, `gsub`) are in
//! `strpat.rs`. Not here at all: `io` and the bits of `os` that touch the
//! system.

#![expect(
    clippy::cast_possible_truncation,
    reason = "script numbers become indices and counts all over here, each one range-checked or clamped first"
)]

use std::cell::Cell;
use std::rc::Rc;

use crate::table::InvalidKey;
use crate::{NativeCtx, NativeError, Rets, Special, Table, Value, Vm};
use smallvec::smallvec;

use crate::meta::Event;

type R = Result<Rets, NativeError>;

pub(crate) fn install(vm: &mut Vm) {
    // ─ Base ──────────────────────────────────────────────────────────────────
    vm.register_native("print", Rc::new(print));
    vm.register_native("type", Rc::new(type_));
    vm.register_native("typeof", Rc::new(typeof_));
    vm.register_native("tostring", Rc::new(tostring));
    vm.register_native("tonumber", Rc::new(tonumber));
    vm.register_native("error", Rc::new(error));
    vm.register_native("assert", Rc::new(assert));
    vm.register_native("select", Rc::new(select));
    vm.register_native("rawequal", Rc::new(rawequal));
    vm.register_native("rawget", Rc::new(rawget));
    vm.register_native("rawset", Rc::new(rawset));
    vm.register_native("rawlen", Rc::new(rawlen));
    vm.register_native("setmetatable", Rc::new(setmetatable));
    vm.register_native("getmetatable", Rc::new(getmetatable));
    vm.register_native("unpack", Rc::new(unpack));
    vm.register_native("collectgarbage", Rc::new(collectgarbage));
    vm.register_special("pcall", Special::Pcall);
    vm.register_native("xpcall", Rc::new(xpcall));

    let next_fn = vm.native("next", Rc::new(next));
    vm.keep_alive(next_fn);
    vm.set_global("next", next_fn);
    vm.register_native(
        "pairs",
        Rc::new(move |_ctx, args| {
            let t = args.first().copied().unwrap_or(Value::Nil);
            if !matches!(t, Value::Table(_)) {
                return Err(bad_arg(1, "pairs", "table", t));
            }
            Ok(smallvec![next_fn, t, Value::Nil])
        }),
    );
    let ipairs_iter = vm.native("ipairs_iter", Rc::new(ipairs_iter));
    vm.keep_alive(ipairs_iter);
    if let Value::Function(f) = ipairs_iter {
        vm.heap_mut().set_fast(f, crate::fastcall::ipairs_iter);
    }
    vm.register_native(
        "ipairs",
        Rc::new(move |_ctx, args| {
            let t = args.first().copied().unwrap_or(Value::Nil);
            if !matches!(t, Value::Table(_)) {
                return Err(bad_arg(1, "ipairs", "table", t));
            }
            Ok(smallvec![ipairs_iter, t, Value::Number(0.0)])
        }),
    );

    // ─ Libraries ─────────────────────────────────────────────────────────────
    let table = library(
        vm,
        &[
            ("insert", Rc::new(table_insert)),
            ("remove", Rc::new(table_remove)),
            ("concat", Rc::new(table_concat)),
            ("unpack", Rc::new(unpack)),
            ("pack", Rc::new(table_pack)),
            ("clear", Rc::new(table_clear)),
            ("find", Rc::new(table_find)),
            ("create", Rc::new(table_create)),
            ("move", Rc::new(table_move)),
            ("sort", Rc::new(table_sort)),
            ("freeze", Rc::new(table_freeze)),
            ("isfrozen", Rc::new(table_isfrozen)),
            ("clone", Rc::new(table_clone)),
        ],
    );
    vm.set_global("table", table);

    let string = library(
        vm,
        &[
            ("len", Rc::new(string_len)),
            ("sub", Rc::new(string_sub)),
            (
                "upper",
                Rc::new(|ctx, args| string_map(ctx, args, "upper", |s| s.to_uppercase())),
            ),
            (
                "lower",
                Rc::new(|ctx, args| string_map(ctx, args, "lower", |s| s.to_lowercase())),
            ),
            (
                "reverse",
                Rc::new(|ctx, args| {
                    string_map(ctx, args, "reverse", |s| s.chars().rev().collect())
                }),
            ),
            ("rep", Rc::new(string_rep)),
            ("byte", Rc::new(string_byte)),
            ("char", Rc::new(string_char)),
            ("find", Rc::new(crate::strpat::string_find)),
            ("match", Rc::new(crate::strpat::string_match)),
            ("gmatch", Rc::new(crate::strpat::string_gmatch)),
            ("gsub", Rc::new(crate::strpat::string_gsub)),
            ("split", Rc::new(string_split)),
            ("format", Rc::new(string_format)),
        ],
    );
    let Value::Table(string_ref) = string else {
        unreachable!()
    };
    vm.set_string_methods(string_ref);
    vm.set_global("string", string);

    let seed = Rc::new(Cell::new(0x2545_F491_4F6C_DD1Du64));
    let (s1, s2) = (seed.clone(), seed);
    let math = library(
        vm,
        &[
            ("abs", Rc::new(|_, a| math1(a, "abs", f64::abs))),
            ("floor", Rc::new(|_, a| math1(a, "floor", f64::floor))),
            ("ceil", Rc::new(|_, a| math1(a, "ceil", f64::ceil))),
            ("sqrt", Rc::new(|_, a| math1(a, "sqrt", f64::sqrt))),
            ("exp", Rc::new(|_, a| math1(a, "exp", f64::exp))),
            ("sin", Rc::new(|_, a| math1(a, "sin", f64::sin))),
            ("cos", Rc::new(|_, a| math1(a, "cos", f64::cos))),
            ("tan", Rc::new(|_, a| math1(a, "tan", f64::tan))),
            ("round", Rc::new(|_, a| math1(a, "round", f64::round))),
            (
                "sign",
                Rc::new(|_, a| {
                    math1(a, "sign", |x| {
                        if x > 0.0 {
                            1.0
                        } else if x < 0.0 {
                            -1.0
                        } else {
                            0.0
                        }
                    })
                }),
            ),
            ("log", Rc::new(math_log)),
            ("fmod", Rc::new(math_fmod)),
            ("min", Rc::new(|_, a| math_fold(a, "min", f64::min))),
            ("max", Rc::new(|_, a| math_fold(a, "max", f64::max))),
            ("clamp", Rc::new(math_clamp)),
            ("random", Rc::new(move |_, a| math_random(&s1, a))),
            (
                "randomseed",
                Rc::new(move |_, a| {
                    let x = num(a, 0, "randomseed")?;
                    s2.set((x.to_bits() ^ 0x9E37_79B9_7F4A_7C15) | 1);
                    Ok(smallvec![])
                }),
            ),
        ],
    );
    if let Value::Table(m) = math {
        let pi = vm.string("pi");
        let huge = vm.string("huge");
        vm.heap_mut().with_table(m, |t| {
            let _ = t.set(pi, Value::Number(std::f64::consts::PI));
            let _ = t.set(huge, Value::Number(f64::INFINITY));
        });
    }
    vm.set_global("math", math);
    crate::coroutine::install(vm);
    crate::oslib::install(vm);
    crate::bitlib::install(vm);
    crate::buffer::install(vm);
    crate::debuglib::install(vm);
    crate::vectorlib::install(vm);
}

pub(crate) fn library(vm: &mut Vm, entries: &[(&str, crate::NativeFn)]) -> Value {
    let mut table = Table::default();
    for (name, func) in entries {
        let key = vm.string(name);
        let f = vm.native(name, func.clone());
        let _ = table.set(key, f);
    }
    let t = vm.heap_mut().alloc_table(table);
    vm.add_library(t);
    Value::Table(t)
}

// ─ Argument Helpers ──────────────────────────────────────────────────────────

pub(crate) fn bad_arg(n: usize, fname: &str, expected: &str, got: Value) -> NativeError {
    NativeError::Message(format!(
        "invalid argument #{n} to '{fname}' ({expected} expected, got {})",
        got.type_name()
    ))
}

fn arg(args: &[Value], i: usize, fname: &str) -> Result<Value, NativeError> {
    args.get(i)
        .copied()
        .ok_or_else(|| NativeError::Message(format!("missing argument #{} to '{fname}'", i + 1)))
}

pub(crate) fn num(args: &[Value], i: usize, fname: &str) -> Result<f64, NativeError> {
    match args.get(i).copied().unwrap_or(Value::Nil) {
        Value::Number(n) => Ok(n),
        other => Err(bad_arg(i + 1, fname, "number", other)),
    }
}

pub(crate) fn int(args: &[Value], i: usize, fname: &str) -> Result<i64, NativeError> {
    let n = num(args, i, fname)?;
    if n.fract() != 0.0 {
        return Err(NativeError::Message(format!(
            "invalid argument #{} to '{fname}' (number has no integer representation)",
            i + 1
        )));
    }
    Ok(n as i64)
}

pub(crate) fn opt_int(
    args: &[Value],
    i: usize,
    fname: &str,
    default: i64,
) -> Result<i64, NativeError> {
    match args.get(i) {
        None | Some(Value::Nil) => Ok(default),
        _ => int(args, i, fname),
    }
}

/// Strings, plus numbers converted to strings (Lua coerces them for the string
/// library).
pub(crate) fn text_arg(
    ctx: &NativeCtx<'_>,
    args: &[Value],
    i: usize,
    fname: &str,
) -> Result<String, NativeError> {
    match args.get(i).copied().unwrap_or(Value::Nil) {
        Value::String(s) => Ok(ctx.heap().get(s).to_string()),
        Value::Number(n) => Ok(rhue_bytecode::format_number(n)),
        other => Err(bad_arg(i + 1, fname, "string", other)),
    }
}

/// A table argument the function's going to change, so not a frozen one.
fn writable_arg(
    ctx: &NativeCtx<'_>,
    args: &[Value],
    i: usize,
    fname: &str,
) -> Result<crate::TableRef, NativeError> {
    let t = table_arg(args, i, fname)?;
    if ctx.heap().table(t).is_frozen() {
        return Err("attempt to modify a readonly table".into());
    }
    Ok(t)
}

fn table_arg(args: &[Value], i: usize, fname: &str) -> Result<crate::TableRef, NativeError> {
    match args.get(i).copied().unwrap_or(Value::Nil) {
        Value::Table(t) => Ok(t),
        other => Err(bad_arg(i + 1, fname, "table", other)),
    }
}

// ─ Base ──────────────────────────────────────────────────────────────────────

fn print(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let mut line = Vec::with_capacity(args.len());
    for &v in args {
        line.push(match ctx.tostring_meta(v)? {
            Some(Value::String(s)) => ctx.heap().get(s).to_string(),
            _ => ctx.display(v),
        });
    }
    ctx.print(&line.join("\t"));
    Ok(smallvec![])
}

fn type_(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let v = arg(args, 0, "type")?;
    Ok(smallvec![ctx.string(v.type_name())])
}

/// Luau's `typeof`: like `type`, but userdata report their own type name.
fn typeof_(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let v = arg(args, 0, "typeof")?;
    let name = match v {
        Value::UserData(u) => ctx.heap().userdata_type_name(u).to_string(),
        other => other.type_name().to_string(),
    };
    Ok(smallvec![ctx.string(&name)])
}

fn tostring(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let v = arg(args, 0, "tostring")?;
    if let Some(s) = ctx.tostring_meta(v)? {
        return Ok(smallvec![s]);
    }
    let s = ctx.display(v);
    Ok(smallvec![ctx.string(&s)])
}

/// `setmetatable(t, mt)` sets (or with nil, removes) `t`'s metatable and
/// returns `t`. A metatable with a `__metatable` field is protected.
fn setmetatable(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = writable_arg(ctx, args, 0, "setmetatable")?;
    let meta = match args.get(1).copied().unwrap_or(Value::Nil) {
        Value::Nil => None,
        Value::Table(m) => Some(m),
        other => return Err(bad_arg(2, "setmetatable", "nil or table", other)),
    };
    if ctx.vm.metamethod(Value::Table(t), Event::Metatable) != Value::Nil {
        return Err("cannot change a protected metatable".into());
    }
    ctx.heap_mut().table_mut(t).set_meta(meta);
    Ok(smallvec![Value::Table(t)])
}

/// `getmetatable(v)`, `v`'s metatable (strings share one), or its `__metatable`
/// field when it has one.
fn getmetatable(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let v = arg(args, 0, "getmetatable")?;
    let Some(meta) = ctx.vm.metatable_of(v) else {
        return Ok(smallvec![Value::Nil]);
    };
    match ctx.vm.metamethod(v, Event::Metatable) {
        Value::Nil => Ok(smallvec![Value::Table(meta)]),
        protected => Ok(smallvec![protected]),
    }
}

fn tonumber(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let v = arg(args, 0, "tonumber")?;
    let base = match args.get(1) {
        None | Some(Value::Nil) => None,
        Some(Value::Number(b)) if b.fract() == 0.0 && (2.0..=36.0).contains(b) => Some(*b as u32),
        Some(_) => return Err("invalid argument #2 to 'tonumber' (base out of range)".into()),
    };
    let result = match (v, base) {
        (Value::Number(n), None) => Some(n),
        (Value::String(_), None) => parse_number(ctx.str(v).expect("is a string")),
        (Value::String(_), Some(b)) => {
            let s = ctx.str(v).expect("is a string").trim().to_ascii_lowercase();
            let (neg, digits) = match s.strip_prefix('-') {
                Some(rest) => (true, rest.to_string()),
                None => (false, s),
            };
            u64::from_str_radix(&digits, b)
                .ok()
                .map(|n| if neg { -(n as f64) } else { n as f64 })
        }
        (_, Some(_)) => return Err("invalid argument #1 to 'tonumber' (string expected)".into()),
        _ => None,
    };
    Ok(smallvec![result.map_or(Value::Nil, Value::Number)])
}

/// Luau's number syntax, decimal (with an optional fraction/exponent) or hex,
/// whitespace around it allowed. Unlike Rust's `parse` it rejects `inf`, `nan`
/// and `infinity`.
fn parse_number(s: &str) -> Option<f64> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let value = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()? as f64
    } else {
        let ok = !body.is_empty()
            && body
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
            && body
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() || c == '.');
        if !ok {
            return None;
        }
        body.parse::<f64>().ok()?
    };
    Some(if neg { -value } else { value })
}

fn error(_ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let value = args.first().copied().unwrap_or(Value::Nil);
    let level = match args.get(1) {
        None | Some(Value::Nil) => 1,
        Some(Value::Number(n)) if *n >= 0.0 => *n as u32,
        Some(other) => return Err(bad_arg(2, "error", "number", *other)),
    };
    Err(NativeError::Raise { value, level })
}

/// `xpcall(f, handler, ...)`, like `pcall` but the error value goes through
/// `handler` first. Unlike Lua, the handler runs after the stack's unwound, so
/// it can't look at the failing frames.
fn xpcall(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let f = arg(args, 0, "xpcall")?;
    let handler = match args.get(1) {
        Some(&h @ Value::Function(_)) => h,
        Some(&other) => return Err(bad_arg(2, "xpcall", "function", other)),
        None => return Err("missing argument #2 to 'xpcall'".into()),
    };
    match ctx.call(f, &args[2..]) {
        Ok(mut values) => {
            values.insert(0, Value::Bool(true));
            Ok(values)
        }
        Err(NativeError::Rethrow(r)) if r.catchable() => {
            let mut out = smallvec![Value::Bool(false)];
            match ctx.call(handler, &[r.value()]) {
                Ok(values) => out.extend(values),
                Err(NativeError::Rethrow(h)) if h.catchable() => out.push(h.value()),
                Err(e) => return Err(e),
            }
            Ok(out)
        }
        Err(e) => Err(e),
    }
}

fn assert(_ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let v = arg(args, 0, "assert")?;
    if v.is_truthy() {
        Ok(Rets::from_slice(args))
    } else {
        match args.get(1) {
            Some(&m) => Err(NativeError::Raise { value: m, level: 1 }),
            None => Err("assertion failed!".into()),
        }
    }
}

fn select(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let n = arg(args, 0, "select")?;
    let rest = &args[1..];
    if ctx.str(n) == Some("#") {
        return Ok(smallvec![Value::Number(rest.len() as f64)]);
    }
    let Value::Number(i) = n else {
        return Err(bad_arg(1, "select", "number", n));
    };
    let i = i as i64;
    let len = rest.len() as i64;
    let start = if i < 0 { len + i } else { i - 1 };
    if i == 0 || start < 0 {
        return Err("invalid argument #1 to 'select' (index out of range)".into());
    }
    Ok(rest
        .get(start as usize..)
        .map(Rets::from_slice)
        .unwrap_or_default())
}

fn rawequal(_ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let (a, b) = (arg(args, 0, "rawequal")?, arg(args, 1, "rawequal")?);
    Ok(smallvec![Value::Bool(a.raw_eq(b))])
}

fn rawget(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = table_arg(args, 0, "rawget")?;
    let k = arg(args, 1, "rawget")?;
    Ok(smallvec![ctx.heap().table(t).get(k)])
}

fn rawset(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = table_arg(args, 0, "rawset")?;
    let (k, v) = (arg(args, 1, "rawset")?, arg(args, 2, "rawset")?);
    ctx.heap_mut()
        .with_table(t, |tbl| tbl.set(k, v))
        .map_err(|e| NativeError::Message(e.message().into()))?;
    Ok(smallvec![args[0]])
}

fn rawlen(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    match arg(args, 0, "rawlen")? {
        Value::Table(t) => Ok(smallvec![Value::Number(ctx.heap().table(t).len() as f64)]),
        Value::String(s) => Ok(smallvec![Value::Number(ctx.heap().get(s).len() as f64)]),
        other => Err(bad_arg(1, "rawlen", "table or string", other)),
    }
}

fn next(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = table_arg(args, 0, "next")?;
    let key = args.get(1).copied().unwrap_or(Value::Nil);
    match ctx.heap().table(t).next(key) {
        Ok(Some((k, v))) => Ok(smallvec![k, v]),
        Ok(None) => Ok(smallvec![Value::Nil]),
        Err(InvalidKey) => Err("invalid key to 'next'".into()),
    }
}

fn ipairs_iter(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = table_arg(args, 0, "ipairs")?;
    let i = num(args, 1, "ipairs")? as usize + 1;
    match ctx.heap().table(t).get_int(i) {
        Value::Nil => Ok(smallvec![Value::Nil]),
        v => Ok(smallvec![Value::Number(i as f64), v]),
    }
}

fn unpack(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = table_arg(args, 0, "unpack")?;
    let tbl = ctx.heap().table(t);
    let i = opt_int(args, 1, "unpack", 1)?;
    let j = opt_int(args, 2, "unpack", tbl.len() as i64)?;
    if j.saturating_sub(i) >= 8000 {
        return Err("too many results to unpack".into());
    }
    Ok((i..=j)
        .map(|k| {
            if k >= 1 {
                tbl.get_int(k as usize)
            } else {
                tbl.get(Value::Number(k as f64))
            }
        })
        .collect())
}

fn collectgarbage(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let opt = match args.first() {
        None | Some(Value::Nil) => "collect".to_string(),
        Some(&v) => ctx
            .str(v)
            .ok_or_else(|| bad_arg(1, "collectgarbage", "string", v))?
            .to_string(),
    };
    match opt.as_str() {
        "count" => Ok(smallvec![Value::Number(
            ctx.heap().stats().bytes as f64 / 1024.0,
        )]),
        // runs at the VM's next safe point (right after this call returns)
        "collect" => {
            ctx.heap_mut().request_collection();
            Ok(smallvec![Value::Number(0.0)])
        }
        other => Err(
            format!("invalid argument #1 to 'collectgarbage' (invalid option '{other}')").into(),
        ),
    }
}

// ─ table ─────────────────────────────────────────────────────────────────────

fn table_insert(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = writable_arg(ctx, args, 0, "insert")?;
    match args.len() {
        2 => {
            ctx.heap_mut()
                .with_table(t, |tbl| tbl.push(args[1]))
                .map_err(|e| NativeError::Message(e.message().into()))?;
        }
        3 => {
            let n = ctx.heap().table(t).len() as i64;
            let pos = int(args, 1, "insert")?;
            if pos < 1 || pos > n + 1 {
                return Err("invalid argument #2 to 'insert' (position out of bounds)".into());
            }
            let v = args[2];
            ctx.heap_mut().with_table(t, |tbl| {
                let mut i = n;
                while i >= pos {
                    let moved = tbl.get_int(i as usize);
                    let _ = tbl.set(Value::Number((i + 1) as f64), moved);
                    i -= 1;
                }
                let _ = tbl.set(Value::Number(pos as f64), v);
            });
        }
        _ => return Err("wrong number of arguments to 'insert'".into()),
    }
    Ok(smallvec![])
}

fn table_remove(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = writable_arg(ctx, args, 0, "remove")?;
    let n = ctx.heap().table(t).len() as i64;
    let pos = opt_int(args, 1, "remove", n)?;
    if n == 0 && args.len() < 2 {
        return Ok(smallvec![Value::Nil]);
    }
    if n + 1 == pos {
        let v = ctx.heap().table(t).get_int(pos as usize);
        ctx.heap_mut()
            .with_table(t, |tbl| tbl.set(Value::Number(pos as f64), Value::Nil))
            .ok();
        return Ok(smallvec![v]);
    }
    if pos < 1 || pos > n + 1 {
        return Err("invalid argument #2 to 'remove' (position out of bounds)".into());
    }
    let removed = ctx.heap().table(t).get_int(pos as usize);
    ctx.heap_mut().with_table(t, |tbl| {
        for i in pos..n {
            let next = tbl.get_int((i + 1) as usize);
            let _ = tbl.set(Value::Number(i as f64), next);
        }
        let _ = tbl.set(Value::Number(n as f64), Value::Nil);
    });
    Ok(smallvec![removed])
}

fn table_concat(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = table_arg(args, 0, "concat")?;
    let sep = match args.get(1) {
        None | Some(Value::Nil) => String::new(),
        Some(_) => text_arg(ctx, args, 1, "concat")?,
    };
    let n = ctx.heap().table(t).len() as i64;
    let i = opt_int(args, 2, "concat", 1)?;
    let j = opt_int(args, 3, "concat", n)?;
    let mut out = String::new();
    for k in i..=j {
        // charged as it goes, a huge j only costs what's actually there before
        // the loop hits a hole and errors
        ctx.charge(1)?;
        // checked as it grows (the final size isn't known up front)
        ctx.reserve(out.len())?;
        match ctx.heap().table(t).get(Value::Number(k as f64)) {
            Value::String(s) => out.push_str(ctx.heap().get(s)),
            Value::Number(x) => out.push_str(&rhue_bytecode::format_number(x)),
            other => {
                return Err(format!(
                    "invalid value (at index {k}) in table for 'concat' (string expected, got {})",
                    other.type_name()
                )
                .into());
            }
        }
        if k < j {
            out.push_str(&sep);
        }
    }
    Ok(smallvec![ctx.string(&out)])
}

fn table_pack(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let mut t = Table::with_capacity(args.len(), 1);
    for (i, v) in args.iter().enumerate() {
        let _ = t.set(Value::Number((i + 1) as f64), *v);
    }
    let n = ctx.string("n");
    let _ = t.set(n, Value::Number(args.len() as f64));
    Ok(smallvec![ctx.new_table(t)])
}

fn table_freeze(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = table_arg(args, 0, "freeze")?;
    if ctx.heap().table(t).is_frozen() {
        return Err("invalid argument #1 to 'freeze' (table is already frozen)".into());
    }
    if ctx.vm.metamethod(Value::Table(t), Event::Metatable) != Value::Nil {
        return Err("invalid argument #1 to 'freeze' (table has a protected metatable)".into());
    }
    ctx.heap_mut().table_mut(t).freeze();
    Ok(smallvec![Value::Table(t)])
}

fn table_isfrozen(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = table_arg(args, 0, "isfrozen")?;
    Ok(smallvec![Value::Bool(ctx.heap().table(t).is_frozen())])
}

/// A shallow copy with the same metatable, not frozen.
fn table_clone(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = table_arg(args, 0, "clone")?;
    if ctx.vm.metamethod(Value::Table(t), Event::Metatable) != Value::Nil {
        return Err("invalid argument #1 to 'clone' (table has a protected metatable)".into());
    }
    ctx.reserve(ctx.heap().table(t).bytes())?;
    let mut copy = ctx.heap().table(t).clone();
    copy.unfreeze_copy();
    Ok(smallvec![ctx.new_table(copy)])
}

fn table_clear(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = writable_arg(ctx, args, 0, "clear")?;
    ctx.heap_mut().with_table(t, Table::clear);
    Ok(smallvec![])
}

fn table_find(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = table_arg(args, 0, "find")?;
    let needle = arg(args, 1, "find")?;
    let start = opt_int(args, 2, "find", 1)?.max(1) as usize;
    let tbl = ctx.heap().table(t);
    let mut i = start;
    loop {
        let v = tbl.get_int(i);
        if v == Value::Nil {
            return Ok(smallvec![Value::Nil]);
        }
        if v.raw_eq(needle) {
            return Ok(smallvec![Value::Number(i as f64)]);
        }
        i += 1;
    }
}

fn table_create(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let n = int(args, 0, "create")?;
    if !(0..=1 << 26).contains(&n) {
        return Err("invalid argument #1 to 'create' (size out of range)".into());
    }
    let v = args.get(1).copied().unwrap_or(Value::Nil);
    ctx.reserve(n as usize * 16)?;
    Ok(smallvec![ctx.new_table(Table::filled(n as usize, v))])
}

/// `table.move(a1, f, e, t, a2?)`: `a2[t..] = a1[f..=e]`, raw, as if it went
/// through a temporary (so overlapping moves in one table work). Returns `a2`.
fn table_move(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    const MAX_MOVE: i64 = 1 << 26;
    let a1 = table_arg(args, 0, "move")?;
    let f = int(args, 1, "move")?;
    let e = int(args, 2, "move")?;
    let t = int(args, 3, "move")?;
    let a2 = match args.get(4) {
        None | Some(Value::Nil) => a1,
        Some(_) => writable_arg(ctx, args, 4, "move")?,
    };
    if ctx.heap().table(a2).is_frozen() {
        return Err("attempt to modify a readonly table".into());
    }
    if e >= f {
        if e - f >= MAX_MOVE {
            return Err("invalid argument #3 to 'move' (too many elements to move)".into());
        }
        let count = (e - f + 1) as usize;
        if t > (1i64 << 53) - count as i64 {
            return Err("invalid argument #4 to 'move' (destination wrap around)".into());
        }
        ctx.reserve(count * 16)?;
        // copying is cheap, about 8 elements for one instruction's worth
        ctx.charge(count as u64 / 8)?;
        let values = ctx.heap().table(a1).get_run(f, count);
        ctx.heap_mut()
            .with_table(a2, |tbl| tbl.set_run(t, &values))
            .map_err(|e| NativeError::Message(e.message().into()))?;
    }
    Ok(smallvec![Value::Table(a2)])
}

fn table_sort(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = writable_arg(ctx, args, 0, "sort")?;
    let comparator = match args.get(1) {
        None | Some(Value::Nil) => None,
        Some(&f @ Value::Function(_)) => Some(f),
        Some(&other) => return Err(bad_arg(2, "sort", "function", other)),
    };
    let n = ctx.heap().table(t).len();
    let mut items: Vec<Value> = (1..=n).map(|i| ctx.heap().table(t).get_int(i)).collect();
    match comparator {
        Some(f) => {
            // the comparator might change the table, so the items get rooted
            // separately from it
            for &v in &items {
                ctx.root(v);
            }
            merge_sort(&mut items, &mut |a, b| {
                Ok(ctx.call(f, &[a, b])?.first().is_some_and(|v| v.is_truthy()))
            })?;
        }
        None => {
            let all_numbers = items.iter().all(|v| matches!(v, Value::Number(_)));
            let all_strings = items.iter().all(|v| matches!(v, Value::String(_)));
            if !all_numbers && !all_strings {
                return Err(
                    "attempt to compare mixed types in 'sort' (all numbers or all strings expected)"
                        .into(),
                );
            }
            let heap = ctx.heap();
            merge_sort(&mut items, &mut |a, b| {
                Ok(match (a, b) {
                    (Value::Number(x), Value::Number(y)) => x < y,
                    (Value::String(x), Value::String(y)) => heap.get(x) < heap.get(y),
                    _ => unreachable!("checked above"),
                })
            })?;
        }
    }
    ctx.heap_mut().with_table(t, |tbl| {
        for (i, v) in items.into_iter().enumerate() {
            let _ = tbl.set(Value::Number((i + 1) as f64), v);
        }
    });
    Ok(smallvec![])
}

/// A stable merge sort over a fallible "less than". Unlike `slice::sort_by` it
/// never panics when a script's comparator is inconsistent (`a < b` and `b < a`
/// both true). The order's unspecified then, but every item stays.
fn merge_sort(
    items: &mut [Value],
    lt: &mut dyn FnMut(Value, Value) -> Result<bool, NativeError>,
) -> Result<(), NativeError> {
    if items.len() <= 1 {
        return Ok(());
    }
    // insertion sort for short runs
    if items.len() <= 12 {
        for i in 1..items.len() {
            let mut j = i;
            while j > 0 && lt(items[j], items[j - 1])? {
                items.swap(j, j - 1);
                j -= 1;
            }
        }
        return Ok(());
    }
    let mid = items.len() / 2;
    merge_sort(&mut items[..mid], lt)?;
    merge_sort(&mut items[mid..], lt)?;
    let (left, right) = (items[..mid].to_vec(), items[mid..].to_vec());
    let (mut i, mut j, mut k) = (0, 0, 0);
    while i < left.len() && j < right.len() {
        // only take from the right when it's strictly less, keeps it stable
        if lt(right[j], left[i])? {
            items[k] = right[j];
            j += 1;
        } else {
            items[k] = left[i];
            i += 1;
        }
        k += 1;
    }
    for &v in left[i..].iter().chain(&right[j..]) {
        items[k] = v;
        k += 1;
    }
    Ok(())
}

// ─ string ────────────────────────────────────────────────────────────────────

fn string_len(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let s = text_arg(ctx, args, 0, "len")?;
    Ok(smallvec![Value::Number(s.len() as f64)])
}

fn string_map(
    ctx: &mut NativeCtx<'_>,
    args: &[Value],
    fname: &str,
    f: impl Fn(&str) -> String,
) -> R {
    let s = text_arg(ctx, args, 0, fname)?;
    let out = f(&s);
    Ok(smallvec![ctx.string(&out)])
}

/// Lua's index rules: negative counts from the end, then clamp to the string.
/// Returns a 0-based half-open byte range.
pub(crate) fn byte_range(len: usize, i: i64, j: i64) -> (usize, usize) {
    let len = len as i64;
    let start = if i < 0 {
        (len + i + 1).max(1)
    } else {
        i.max(1)
    };
    let end = if j < 0 { len + j + 1 } else { j.min(len) };
    if start > end {
        (0, 0)
    } else {
        ((start - 1) as usize, end as usize)
    }
}

fn string_sub(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let s = text_arg(ctx, args, 0, "sub")?;
    let i = opt_int(args, 1, "sub", 1)?;
    let j = opt_int(args, 2, "sub", -1)?;
    let (a, b) = byte_range(s.len(), i, j);
    // Rhue strings are UTF-8, a cut through a character becomes U+FFFD
    let out = String::from_utf8_lossy(&s.as_bytes()[a..b]).into_owned();
    Ok(smallvec![ctx.string(&out)])
}

fn string_rep(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let s = text_arg(ctx, args, 0, "rep")?;
    let n = int(args, 1, "rep")?.max(0) as usize;
    let sep = match args.get(2) {
        None | Some(Value::Nil) => String::new(),
        Some(_) => text_arg(ctx, args, 2, "rep")?,
    };
    let size = (s.len() + sep.len()).saturating_mul(n);
    if size > 1 << 28 {
        return Err("resulting string too large".into());
    }
    // ("", 1e12) is empty, and building it piece by piece would take forever
    if size == 0 {
        return Ok(smallvec![ctx.string("")]);
    }
    ctx.reserve(size)?;
    ctx.charge(n as u64 + size as u64 / 64)?;
    let mut out = String::with_capacity(size);
    for i in 0..n {
        if i > 0 {
            out.push_str(&sep);
        }
        out.push_str(&s);
    }
    Ok(smallvec![ctx.string(&out)])
}

fn string_byte(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let s = text_arg(ctx, args, 0, "byte")?;
    let i = opt_int(args, 1, "byte", 1)?;
    let j = opt_int(args, 2, "byte", i)?;
    let (a, b) = byte_range(s.len(), i, j);
    Ok(s.as_bytes()[a..b]
        .iter()
        .map(|&c| Value::Number(c as f64))
        .collect())
}

fn string_char(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let mut out = String::new();
    for i in 0..args.len() {
        let c = int(args, i, "char")?;
        // Rhue strings are UTF-8, so values are code points (0..=255 match Lua
        // for ASCII, 128..=255 become the Latin-1 characters)
        let ch = u32::try_from(c)
            .ok()
            .and_then(char::from_u32)
            .filter(|_| c <= 255)
            .ok_or_else(|| {
                NativeError::Message(format!(
                    "invalid argument #{} to 'char' (value out of range)",
                    i + 1
                ))
            })?;
        out.push(ch);
    }
    Ok(smallvec![ctx.string(&out)])
}

fn string_split(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let s = text_arg(ctx, args, 0, "split")?;
    let sep = match args.get(1) {
        None | Some(Value::Nil) => ",".to_string(),
        Some(_) => text_arg(ctx, args, 1, "split")?,
    };
    let parts: Vec<String> = if sep.is_empty() {
        s.chars().map(String::from).collect()
    } else {
        s.split(sep.as_str()).map(String::from).collect()
    };
    let mut t = Table::with_capacity(parts.len(), 0);
    for (i, p) in parts.iter().enumerate() {
        let v = ctx.string(p);
        let _ = t.set(Value::Number((i + 1) as f64), v);
    }
    Ok(smallvec![ctx.new_table(t)])
}

/// `string.format` with `%d %i %u %c %x %X %o %e %E %f %g %G %q %s %%`,
/// flags `-+ #0`, width and precision.
fn string_format(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let fmt = text_arg(ctx, args, 0, "format")?;
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    let mut argn = 1;
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut flags = String::new();
        while let Some(&f) = chars.peek() {
            if "-+ #0".contains(f) {
                flags.push(f);
                chars.next();
            } else {
                break;
            }
        }
        let mut width = String::new();
        while let Some(&d) = chars.peek().filter(|c| c.is_ascii_digit()) {
            width.push(d);
            chars.next();
        }
        let mut precision: Option<usize> = None;
        if chars.peek() == Some(&'.') {
            chars.next();
            let mut p = String::new();
            while let Some(&d) = chars.peek().filter(|c| c.is_ascii_digit()) {
                p.push(d);
                chars.next();
            }
            precision = Some(p.parse().unwrap_or(0));
        }
        let Some(conv) = chars.next() else {
            return Err("invalid conversion '%' to 'format'".into());
        };
        if conv == '%' {
            out.push('%');
            continue;
        }
        let i = argn;
        argn += 1;
        let body = match conv {
            'd' | 'i' | 'u' => {
                let n = num(args, i, "format")?;
                if n.fract() != 0.0 {
                    return Err(format!(
                        "invalid argument #{} to 'format' (number has no integer representation)",
                        i + 1
                    )
                    .into());
                }
                let mut s = (n as i64).abs().to_string();
                if let Some(p) = precision {
                    while s.len() < p {
                        s.insert(0, '0');
                    }
                }
                sign(n < 0.0, &flags) + &s
            }
            'c' => {
                let n = int(args, i, "format")?;
                char::from_u32(n as u32)
                    .map(String::from)
                    .unwrap_or_default()
            }
            'x' | 'X' | 'o' => {
                let n = int(args, i, "format")?;
                let s = match conv {
                    'x' => format!("{:x}", n),
                    'X' => format!("{:X}", n),
                    _ => format!("{:o}", n),
                };
                if flags.contains('#') && n != 0 {
                    (match conv {
                        'x' => "0x",
                        'X' => "0X",
                        _ => "0",
                    })
                    .to_string()
                        + &s
                } else {
                    s
                }
            }
            'f' | 'F' => {
                let n = num(args, i, "format")?;
                sign(n < 0.0, &flags) + &format!("{:.*}", precision.unwrap_or(6), n.abs())
            }
            'e' | 'E' => {
                let n = num(args, i, "format")?;
                let s = c_exponent(n.abs(), precision.unwrap_or(6));
                let s = if conv == 'E' { s.to_uppercase() } else { s };
                sign(n < 0.0, &flags) + &s
            }
            'g' | 'G' => {
                let n = num(args, i, "format")?;
                let s = c_general(n.abs(), precision.unwrap_or(6), flags.contains('#'));
                let s = if conv == 'G' { s.to_uppercase() } else { s };
                sign(n < 0.0, &flags) + &s
            }
            's' => {
                let v = arg(args, i, "format")?;
                let s = ctx.display(v);
                match precision {
                    Some(p) => s.chars().take(p).collect(),
                    None => s,
                }
            }
            'q' => {
                let v = arg(args, i, "format")?;
                match v {
                    Value::String(sr) => quote(ctx.heap().get(sr)),
                    other => ctx.display(other),
                }
            }
            other => return Err(format!("invalid conversion '%{other}' to 'format'").into()),
        };
        let width: usize = width.parse().unwrap_or(0);
        let pad = width.saturating_sub(body.chars().count());
        if flags.contains('-') {
            out.push_str(&body);
            out.extend(std::iter::repeat_n(' ', pad));
        } else if flags.contains('0') && "dioxXuefgEG".contains(conv) {
            let (sign_part, digits) = match body.chars().next() {
                Some(c @ ('-' | '+' | ' ')) => (c.to_string(), body[1..].to_string()),
                _ => (String::new(), body.clone()),
            };
            out.push_str(&sign_part);
            out.extend(std::iter::repeat_n('0', pad));
            out.push_str(&digits);
        } else {
            out.extend(std::iter::repeat_n(' ', pad));
            out.push_str(&body);
        }
    }
    Ok(smallvec![ctx.string(&out)])
}

fn sign(negative: bool, flags: &str) -> String {
    if negative {
        "-".into()
    } else if flags.contains('+') {
        "+".into()
    } else if flags.contains(' ') {
        " ".into()
    } else {
        String::new()
    }
}

/// C's `%e`: `1.500000e+02`.
fn c_exponent(n: f64, precision: usize) -> String {
    if !n.is_finite() {
        return if n.is_nan() {
            "nan".into()
        } else {
            "inf".into()
        };
    }
    let s = format!("{:.*e}", precision, n);
    let (mantissa, exp) = s.split_once('e').expect("has an exponent");
    let exp: i32 = exp.parse().expect("integer exponent");
    format!(
        "{mantissa}e{}{:02}",
        if exp < 0 { '-' } else { '+' },
        exp.abs()
    )
}

/// C's `%g`: the shorter of `%e` and `%f` at `precision` significant digits,
/// trailing zeros removed (unless `#`).
fn c_general(n: f64, precision: usize, keep_zeros: bool) -> String {
    if !n.is_finite() {
        return if n.is_nan() {
            "nan".into()
        } else {
            "inf".into()
        };
    }
    let p = precision.max(1);
    let exp = if n == 0.0 {
        0
    } else {
        n.log10().floor() as i32
    };
    let s = if exp < -4 || exp >= p as i32 {
        c_exponent(n, p - 1)
    } else {
        format!("{:.*}", (p as i32 - 1 - exp).max(0) as usize, n)
    };
    if keep_zeros || !s.contains('.') {
        return s;
    }
    match s.split_once('e') {
        Some((m, e)) => format!("{}e{e}", m.trim_end_matches('0').trim_end_matches('.')),
        None => s.trim_end_matches('0').trim_end_matches('.').to_string(),
    }
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ─ Math ──────────────────────────────────────────────────────────────────────

fn math1(args: &[Value], fname: &str, f: impl Fn(f64) -> f64) -> R {
    Ok(smallvec![Value::Number(f(num(args, 0, fname)?))])
}

fn math_fold(args: &[Value], fname: &str, f: impl Fn(f64, f64) -> f64) -> R {
    let mut acc = num(args, 0, fname)?;
    for i in 1..args.len() {
        acc = f(acc, num(args, i, fname)?);
    }
    Ok(smallvec![Value::Number(acc)])
}

fn math_log(_: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let x = num(args, 0, "log")?;
    Ok(smallvec![Value::Number(match args.get(1) {
        None | Some(Value::Nil) => x.ln(),
        _ => {
            let b = num(args, 1, "log")?;
            if b == 2.0 {
                x.log2()
            } else if b == 10.0 {
                x.log10()
            } else {
                x.ln() / b.ln()
            }
        }
    })])
}

fn math_fmod(_: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let (a, b) = (num(args, 0, "fmod")?, num(args, 1, "fmod")?);
    Ok(smallvec![Value::Number(a % b)])
}

fn math_clamp(_: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let (x, lo, hi) = (
        num(args, 0, "clamp")?,
        num(args, 1, "clamp")?,
        num(args, 2, "clamp")?,
    );
    if lo > hi {
        return Err(
            "invalid argument #3 to 'clamp' (max must be greater than or equal to min)".into(),
        );
    }
    Ok(smallvec![Value::Number(x.max(lo).min(hi))])
}

/// xorshift64*. Small, fast, deterministic per seed. Not cryptographic.
fn math_random(state: &Cell<u64>, args: &[Value]) -> R {
    let mut x = state.get();
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    state.set(x);
    let r = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64;
    match args.len() {
        0 => Ok(smallvec![Value::Number(r)]),
        _ => {
            let (lo, hi) = if args.len() == 1 {
                (1.0, num(args, 0, "random")?)
            } else {
                (num(args, 0, "random")?, num(args, 1, "random")?)
            };
            let (lo, hi) = (lo.floor(), hi.floor());
            if lo > hi {
                return Err("invalid argument #2 to 'random' (interval is empty)".into());
            }
            Ok(smallvec![Value::Number(lo + (r * (hi - lo + 1.0)).floor())])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_parsing() {
        assert_eq!(parse_number(" 42 "), Some(42.0));
        assert_eq!(parse_number("-3.5e2"), Some(-350.0));
        assert_eq!(parse_number(".5"), Some(0.5));
        assert_eq!(parse_number("0x1F"), Some(31.0));
        assert_eq!(parse_number("inf"), None);
        assert_eq!(parse_number("nan"), None);
        assert_eq!(parse_number("12abc"), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn byte_ranges_follow_lua() {
        assert_eq!(byte_range(5, 2, 4), (1, 4));
        assert_eq!(byte_range(5, -3, -1), (2, 5));
        assert_eq!(byte_range(5, 0, 100), (0, 5));
        assert_eq!(byte_range(5, 4, 2), (0, 0));
    }

    #[test]
    fn c_style_number_formats() {
        assert_eq!(c_exponent(150.0, 6), "1.500000e+02");
        assert_eq!(c_general(0.0001, 6, false), "0.0001");
        assert_eq!(c_general(1e20, 6, false), "1e+20");
        assert_eq!(c_general(1.23456, 3, false), "1.23");
        assert_eq!(c_general(100.0, 6, false), "100");
    }
}
