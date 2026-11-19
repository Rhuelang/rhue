//! Fast paths for common builtins, Luau's `FASTCALL` done at run time.
//!
//! `CALL` on a native that has a [`FastFn`] and three arguments or fewer tries
//! the fast path first. The arguments get copied to a small array and the
//! single result goes straight into the caller's registers, skipping the native
//! context, argument buffer and result vector. Luau's compiler picks fast calls
//! by name and has to check at run time that `math` (say) wasn't replaced. Here
//! the check is free since the fast path belongs to the function value being
//! called, so a script that shadows `math.floor` just calls its own function,
//! which doesn't have a fast path.
//!
//! Each fast path only handles the plain case and returns `None` for anything
//! else (wrong types, extra arguments it doesn't model, any error, zero
//! results), and then the full native runs instead. So behaviour, every error
//! message included, comes from one place, the full native in `stdlib`.

use crate::heap::{Fast, FastFn, FnRef};
use crate::meta::Event;
use crate::{Value, Vm};

/// Attaches the fast paths to the standard library's functions. Run after
/// `stdlib::install`, while the globals still hold the originals.
pub(crate) fn install(vm: &mut Vm) {
    let globals: &[(&str, FastFn)] = &[
        ("type", type_),
        ("rawequal", rawequal),
        ("rawget", rawget),
        ("rawlen", rawlen),
        ("setmetatable", setmetatable),
        ("getmetatable", getmetatable),
        ("next", next),
    ];
    for &(name, fast) in globals {
        if let Some(f) = function(vm.global(name)) {
            vm.heap.set_fast(f, fast);
        }
    }
    let fields: &[(&str, &str, FastFn)] = &[
        ("math", "abs", |_, a| math1(a, f64::abs)),
        ("math", "floor", |_, a| math1(a, f64::floor)),
        ("math", "ceil", |_, a| math1(a, f64::ceil)),
        ("math", "sqrt", |_, a| math1(a, f64::sqrt)),
        ("math", "round", |_, a| math1(a, f64::round)),
        ("math", "sign", |_, a| {
            math1(a, |x| {
                if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            })
        }),
        ("math", "min", |_, a| fold(a, f64::min)),
        ("math", "max", |_, a| fold(a, f64::max)),
        ("math", "clamp", clamp),
        ("string", "len", string_len),
        ("string", "byte", string_byte),
        ("string", "sub", string_sub),
    ];
    for &(lib, name, fast) in fields {
        let Value::Table(t) = vm.global(lib) else {
            continue;
        };
        let key = vm.heap.intern(name);
        if let Some(f) = function(vm.heap.table(t).get_str(key)) {
            vm.heap.set_fast(f, fast);
        }
    }
}

fn function(v: Value) -> Option<FnRef> {
    match v {
        Value::Function(f) => Some(f),
        _ => None,
    }
}

/// `n` as an integer, if it is one (what `int` in `stdlib` accepts).
fn integer(v: Value) -> Option<i64> {
    match v {
        Value::Number(n) if n.fract() == 0.0 => Some(n as i64),
        _ => None,
    }
}

fn math1(args: &[Value], f: fn(f64) -> f64) -> Option<Fast> {
    match args {
        [Value::Number(x), ..] => Some(Fast::One(Value::Number(f(*x)))),
        _ => None,
    }
}

fn fold(args: &[Value], f: fn(f64, f64) -> f64) -> Option<Fast> {
    let (Value::Number(first), rest) = args.split_first()? else {
        return None;
    };
    let mut acc = *first;
    for v in rest {
        let Value::Number(n) = v else { return None };
        acc = f(acc, *n);
    }
    Some(Fast::One(Value::Number(acc)))
}

fn clamp(_: &mut Vm, args: &[Value]) -> Option<Fast> {
    match *args {
        [Value::Number(x), Value::Number(lo), Value::Number(hi)] if lo <= hi => {
            Some(Fast::One(Value::Number(x.max(lo).min(hi))))
        }
        _ => None,
    }
}

fn type_(vm: &mut Vm, args: &[Value]) -> Option<Fast> {
    let v = *args.first()?;
    Some(Fast::One(Value::String(vm.heap.intern(v.type_name()))))
}

fn rawequal(_: &mut Vm, args: &[Value]) -> Option<Fast> {
    match *args {
        [a, b, ..] => Some(Fast::One(Value::Bool(a.raw_eq(b)))),
        _ => None,
    }
}

fn rawget(vm: &mut Vm, args: &[Value]) -> Option<Fast> {
    match *args {
        [Value::Table(t), k, ..] => Some(Fast::One(vm.heap.table(t).get(k))),
        _ => None,
    }
}

fn rawlen(vm: &mut Vm, args: &[Value]) -> Option<Fast> {
    match args.first()? {
        Value::Table(t) => Some(Fast::One(Value::Number(vm.heap.table(*t).len() as f64))),
        Value::String(s) => Some(Fast::One(Value::Number(vm.heap.get(*s).len() as f64))),
        _ => None,
    }
}

fn setmetatable(vm: &mut Vm, args: &[Value]) -> Option<Fast> {
    let (Value::Table(t), meta) = (*args.first()?, args.get(1).copied().unwrap_or(Value::Nil))
    else {
        return None;
    };
    let meta = match meta {
        Value::Nil => None,
        Value::Table(m) => Some(m),
        _ => return None,
    };
    if vm.heap.table(t).is_frozen()
        || vm.metamethod(Value::Table(t), Event::Metatable) != Value::Nil
    {
        return None;
    }
    vm.heap.table_mut(t).set_meta(meta);
    Some(Fast::One(Value::Table(t)))
}

fn getmetatable(vm: &mut Vm, args: &[Value]) -> Option<Fast> {
    let v = *args.first()?;
    let Some(meta) = vm.metatable_of(v) else {
        return Some(Fast::One(Value::Nil));
    };
    Some(Fast::One(match vm.metamethod(v, Event::Metatable) {
        Value::Nil => Value::Table(meta),
        protected => protected,
    }))
}

fn string_len(vm: &mut Vm, args: &[Value]) -> Option<Fast> {
    match args.first()? {
        Value::String(s) => Some(Fast::One(Value::Number(vm.heap.get(*s).len() as f64))),
        _ => None,
    }
}

/// `s:byte()` and `s:byte(i)` when there's a byte there (no byte means no
/// results, the full native handles that).
fn string_byte(vm: &mut Vm, args: &[Value]) -> Option<Fast> {
    let (s, i) = match *args {
        [Value::String(s)] | [Value::String(s), Value::Nil] => (s, 1),
        [Value::String(s), i] => (s, integer(i)?),
        _ => return None,
    };
    let bytes = vm.heap.get(s).as_bytes();
    let (a, b) = crate::stdlib::byte_range(bytes.len(), i, i);
    (b == a + 1).then(|| Fast::One(Value::Number(bytes[a] as f64)))
}

fn string_sub(vm: &mut Vm, args: &[Value]) -> Option<Fast> {
    let int_or = |v: Option<&Value>, default: i64| match v {
        None | Some(Value::Nil) => Some(default),
        Some(&v) => integer(v),
    };
    let Value::String(s) = *args.first()? else {
        return None;
    };
    let (i, j) = (int_or(args.get(1), 1)?, int_or(args.get(2), -1)?);
    let text = vm.heap.get(s);
    let (a, b) = crate::stdlib::byte_range(text.len(), i, j);
    if a == 0 && b == text.len() {
        // the whole string, it's already interned
        return Some(Fast::One(Value::String(s)));
    }
    // a cut through a character becomes U+FFFD, same as the full native
    let out = String::from_utf8_lossy(&text.as_bytes()[a..b]).into_owned();
    Some(Fast::One(Value::String(vm.heap.intern(&out))))
}

/// `next(t, k)`, the step of a `pairs` loop.
fn next(vm: &mut Vm, args: &[Value]) -> Option<Fast> {
    let (Value::Table(t), key) = (*args.first()?, args.get(1).copied().unwrap_or(Value::Nil))
    else {
        return None;
    };
    match vm.heap.table(t).next(key).ok()? {
        Some((k, v)) => Some(Fast::Two(k, v)),
        None => Some(Fast::One(Value::Nil)),
    }
}

/// The step of an `ipairs` loop (the iterator `ipairs` returns).
pub(crate) fn ipairs_iter(vm: &mut Vm, args: &[Value]) -> Option<Fast> {
    let [Value::Table(t), Value::Number(i), ..] = *args else {
        return None;
    };
    // same conversion as the full native's
    let i = i as usize + 1;
    match vm.heap.table(t).get_int(i) {
        Value::Nil => Some(Fast::One(Value::Nil)),
        v => Some(Fast::Two(Value::Number(i as f64), v)),
    }
}

/// Whether `f` has a fast path (for tests).
#[cfg(test)]
pub(crate) fn has_fast_path(vm: &Vm, f: Value) -> bool {
    matches!(f, Value::Function(r) if matches!(vm.heap.function(r), crate::heap::Function::Native(n) if n.fast.is_some()))
}
