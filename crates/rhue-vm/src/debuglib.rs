//! `debug.traceback` and `debug.info`, same as Luau: the call stack as text,
//! and facts about a stack level or a function. Level 1 is whatever called
//! `debug.info`/`debug.traceback`, 2 is its caller and so on. Level 0 is the
//! native itself.

#![expect(
    clippy::cast_possible_truncation,
    reason = "stack levels are script numbers, checked against the stack first"
)]

use std::rc::Rc;

use smallvec::{SmallVec, smallvec};

use crate::heap::{Function, ThreadRef};
use crate::stdlib::{bad_arg, num, text_arg};
use crate::{Frame, NativeCtx, NativeError, Rets, RuntimeError, TraceEntry, Value, Vm};

type R = Result<Rets, NativeError>;

/// The Lua frames of `thread` (the running one when `None`), innermost first.
fn frames(vm: &Vm, thread: Option<ThreadRef>) -> Vec<&Frame> {
    let list: &[Frame] = match thread {
        Some(t) if t != vm.current => &vm.heap.thread(t).state.frames,
        _ => &vm.frames,
    };
    list.iter()
        .rev()
        .filter(|f| !Rc::ptr_eq(&f.cl, &vm.placeholder))
        .collect()
}

/// A leading thread argument, and the index of the next one.
fn thread_arg(args: &[Value]) -> (Option<ThreadRef>, usize) {
    match args.first() {
        Some(Value::Thread(t)) => (Some(*t), 1),
        _ => (None, 0),
    }
}

fn traceback(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let (thread, i) = thread_arg(args);
    let message = args.get(i).copied().unwrap_or(Value::Nil);
    let message = match message {
        Value::Nil => None,
        Value::String(_) | Value::Number(_) => Some(text_arg(ctx, args, i, "traceback")?),
        // Lua hands anything else back untouched
        other => return Ok(smallvec![other]),
    };
    // another thread's frames start at its own top (level 0)
    let default = if thread.is_some_and(|t| t != ctx.vm.current) {
        0
    } else {
        1
    };
    let level = match args.get(i + 1) {
        None | Some(Value::Nil) => default,
        Some(_) => num(args, i + 1, "traceback")?.trunc() as i64,
    };
    if level < 0 {
        return Err("invalid argument #2 to 'traceback' (level can't be negative)".into());
    }
    let vm = ctx.vm();
    let mut entries: Vec<TraceEntry> = Vec::new();
    for f in frames(vm, thread) {
        entries.push(TraceEntry {
            function: Vm::name_of(f),
            span: Vm::span_of(f, f.pc.saturating_sub(1)),
            source: f.proto().source.clone(),
        });
        entries.extend(vm.trail_trace(f));
    }
    // level 1 is the innermost Lua frame (0, this native, isn't one)
    let skip = (level.max(1) - 1) as usize;
    let entries: Vec<TraceEntry> = entries.into_iter().skip(skip).collect();
    let text = if entries.is_empty() {
        "stack traceback:".to_string()
    } else {
        RuntimeError {
            message: String::new(),
            span: entries[0].span,
            source: entries[0].source.clone(),
            traceback: entries,
        }
        .traceback_text()
    };
    let out = match message {
        Some(m) => format!("{m}\n{text}"),
        None => text,
    };
    Ok(smallvec![ctx.string(&out)])
}

/// What `debug.info` reports about one function or frame.
struct Facts {
    source: String,
    line: f64,
    name: Option<String>,
    params: u8,
    vararg: bool,
    func: Value,
}

fn facts_of_function(vm: &Vm, func: Value) -> Facts {
    let Value::Function(r) = func else {
        unreachable!("checked by the caller")
    };
    match vm.heap.function(r) {
        Function::Lua(c) => Facts {
            source: c.proto.source.name.to_string(),
            line: f64::from(c.proto.source.line(c.proto.span.start)),
            name: c.proto.name.as_ref().map(|n| n.to_string()),
            params: c.proto.num_params,
            vararg: c.proto.is_vararg,
            func,
        },
        Function::Native(n) => Facts {
            source: "[C]".into(),
            line: -1.0,
            name: Some(n.name.to_string()),
            params: 0,
            vararg: true,
            func,
        },
    }
}

fn info(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let (thread, i) = thread_arg(args);
    let target = args.get(i).copied().unwrap_or(Value::Nil);
    let options = match args.get(i + 1) {
        Some(Value::String(_)) => text_arg(ctx, args, i + 1, "info")?,
        other => {
            return Err(bad_arg(
                i + 2,
                "info",
                "string",
                other.copied().unwrap_or(Value::Nil),
            ));
        }
    };
    if let Some(bad) = options.chars().find(|c| !"slnaf".contains(*c)) {
        return Err(format!(
            "invalid argument #{} to 'info' (invalid option '{bad}')",
            i + 2
        )
        .into());
    }
    if options.chars().any(|c| options.matches(c).count() > 1) {
        return Err(format!("invalid argument #{} to 'info' (duplicate option)", i + 2).into());
    }
    let facts = match target {
        Value::Function(_) => facts_of_function(ctx.vm(), target),
        Value::Number(n) => {
            let level = n.trunc() as i64;
            if level < 0 {
                return Err("invalid argument #1 to 'info' (level can't be negative)".into());
            }
            let vm = ctx.vm();
            if level == 0 && thread.is_none_or(|t| t == vm.current) {
                // this native
                Facts {
                    source: "[C]".into(),
                    line: -1.0,
                    name: Some("info".into()),
                    params: 0,
                    vararg: true,
                    func: Value::Nil,
                }
            } else {
                let own = thread.is_none_or(|t| t == vm.current);
                let index = if own { level - 1 } else { level } as usize;
                let Some(f) = frames(vm, thread).get(index).copied() else {
                    // past the end of the stack, nothing
                    return Ok(SmallVec::new());
                };
                let span = Vm::span_of(f, f.pc.saturating_sub(1));
                let proto = f.proto();
                Facts {
                    source: proto.source.name.to_string(),
                    line: f64::from(proto.source.line(span.start)),
                    name: (!f.is_main)
                        .then(|| proto.name.as_ref().map(|n| n.to_string()))
                        .flatten(),
                    params: proto.num_params,
                    vararg: proto.is_vararg,
                    func: Value::Function(f.closure),
                }
            }
        }
        other => return Err(bad_arg(i + 1, "info", "function or level", other)),
    };
    let mut out: Rets = SmallVec::new();
    for c in options.chars() {
        match c {
            's' => out.push(ctx.string(&facts.source)),
            'l' => out.push(Value::Number(facts.line)),
            'n' => out.push(match &facts.name {
                Some(n) => ctx.string(n),
                None => Value::Nil,
            }),
            'a' => {
                out.push(Value::Number(f64::from(facts.params)));
                out.push(Value::Bool(facts.vararg));
            }
            'f' => out.push(facts.func),
            _ => unreachable!("options checked above"),
        }
    }
    Ok(out)
}

pub(crate) fn install(vm: &mut Vm) {
    let entries: &[(&str, crate::NativeFn)] =
        &[("traceback", Rc::new(traceback)), ("info", Rc::new(info))];
    let lib = crate::stdlib::library(vm, entries);
    vm.set_global("debug", lib);
}
