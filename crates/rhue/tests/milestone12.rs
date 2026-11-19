//! Milestone 12: debug hooks (line, call and return events), looking at the
//! stack, locals and upvalues from a hook, and the language server's rename and
//! signature help (in `rhue-lsp`'s tests). The debugger itself (`rhue-dap`) has
//! its own tests.

use std::cell::RefCell;
use std::rc::Rc;

use rhue::{Context, DebugEvent, Engine, Function, HookMask, ScriptError};

const LINES: HookMask = HookMask::new().line(true);

fn all() -> HookMask {
    HookMask::new().line(true).call(true).ret(true)
}

/// Runs `src` with a hook that records `describe(dbg, event)` for each event.
fn trace(
    e: &mut Engine,
    mask: HookMask,
    src: &str,
    describe: impl Fn(&mut rhue::Debugger<'_, '_>, DebugEvent) -> Option<String> + 'static,
) -> Vec<String> {
    let log = Rc::new(RefCell::new(Vec::new()));
    let l = log.clone();
    e.set_debug_hook(mask, move |dbg, ev| {
        if let Some(s) = describe(dbg, ev) {
            l.borrow_mut().push(s);
        }
        Ok(())
    });
    e.eval_named("t.rhue", src)
        .unwrap_or_else(|err| panic!("{}", err.render(false)));
    e.clear_debug_hook();
    log.borrow().clone()
}

#[test]
fn line_events_follow_execution() {
    let mut e = Engine::new();
    let src =
        "local s = 0\nfor i = 1, 3 do\n  s += i\nend\nif s > 5 then\n  s = 0\nelse\n  s = 1\nend";
    let lines = trace(&mut e, LINES, src, |_, ev| match ev {
        DebugEvent::Line(l) => Some(l.to_string()),
        _ => None,
    });
    // the loop body runs three times, the `else` branch never does
    assert_eq!(lines.join(" "), "1 2 3 2 3 2 3 2 5 6 9");
}

#[test]
fn call_and_return_events_name_their_functions() {
    let mut e = Engine::new();
    let src = "local function inner() return 1 end\nlocal function outer()\n  return inner() + 1\nend\nouter()";
    let events = trace(
        &mut e,
        all().line(false),
        src,
        |dbg, ev| {
            let f = dbg.frame(0)?;
            Some(format!("{ev:?} {} depth {}", f.function, dbg.depth()))
        },
    );
    assert_eq!(
        events,
        [
            "Call <main> depth 1",
            "Call outer depth 2",
            "Call inner depth 3",
            "Return inner depth 3",
            "Return outer depth 2",
            "Return <main> depth 1",
        ]
    );
}

#[test]
fn the_stack_from_inside_a_hook() {
    let mut e = Engine::new();
    let src = "local function c()\n  local x = 1\nend\nlocal function b() c() end\nlocal function a()\n  b()\nend\na()";
    let stacks = trace(&mut e, LINES, src, |dbg, ev| {
        (ev == DebugEvent::Line(2)).then(|| {
            dbg.stack()
                .iter()
                .map(|f| format!("{}@{}:{}", f.function, f.source, f.line))
                .collect::<Vec<_>>()
                .join(" < ")
        })
    });
    assert_eq!(
        stacks,
        ["c@t.rhue:2 < b@t.rhue:4 < a@t.rhue:6 < <main>@t.rhue:8"]
    );
}

#[test]
fn locals_and_upvalues() {
    let mut e = Engine::new();
    let src = "local count = 10
local function make(step)
  local total = 0
  return function(n)
    local x = 1
    do
      local x = 2
      total += n * step + count + x
    end
    return total
  end
end
local f = make(3)
f(1)";
    let seen = trace(&mut e, LINES, src, |dbg, ev| {
        (ev == DebugEvent::Line(8)).then(|| {
            let fmt = |v: Vec<(String, rhue::Value)>| {
                v.iter()
                    .map(|(n, v)| match v {
                        rhue::Value::Function(_) => format!("{n}=<function>"),
                        v => format!("{n}={v}"),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            };
            format!(
                "locals {} | upvalues {} | caller {}",
                fmt(dbg.locals(0)),
                fmt(dbg.upvalues(0)),
                fmt(dbg.locals(1))
            )
        })
    });
    // the inner `x` shadows the outer one, the caller is the main chunk
    assert_eq!(
        seen,
        [
            "locals n=1,x=2 | upvalues total=0,step=3,count=10 | caller count=10,make=<function>,f=<function>"
        ]
    );
}

#[test]
fn tables_in_locals_can_be_read() {
    let mut e = Engine::new();
    e.set_gc_stress(true);
    let src = "local t = { name = 'Aalto', n = 3 }\nlocal pad = {}\nprint(t.name)";
    let seen = trace(&mut e, LINES, src, |dbg, ev| {
        (ev == DebugEvent::Line(3)).then(|| {
            let locals = dbg.locals(0);
            let rhue::Value::Table(t) = &locals[0].1 else {
                return "not a table".into();
            };
            let mut cx = dbg.context();
            let name: String = cx.get(t, "name").unwrap();
            let n: f64 = cx.get(t, "n").unwrap();
            format!("{name} {n}")
        })
    });
    assert_eq!(seen, ["Aalto 3"]);
}

#[test]
fn hooks_see_coroutines_and_metamethods() {
    let mut e = Engine::new();
    let src = "local mt = { __add = function(a, b)\n  return 42\nend }\nlocal t = setmetatable({}, mt)\nlocal co = coroutine.wrap(function()\n  local v = t + 1\n  coroutine.yield(v)\nend)\nco()";
    let seen = trace(&mut e, LINES, src, |dbg, ev| match ev {
        DebugEvent::Line(2) | DebugEvent::Line(7) => Some(
            dbg.stack()
                .iter()
                .map(|f| f.function.clone())
                .collect::<Vec<_>>()
                .join("<"),
        ),
        _ => None,
    });
    // a coroutine's stack is its own, the metamethod runs as a frame on it
    assert_eq!(seen, ["<anonymous><<anonymous>", "<anonymous>"]);
}

#[test]
fn a_hook_can_stop_the_script() {
    let mut e = Engine::new();
    e.set_debug_hook(LINES, |dbg, ev| {
        if ev == DebugEvent::Line(3) && dbg.depth() > 0 {
            return Err("stopped by the debugger".into());
        }
        Ok(())
    });
    // `pcall` can't catch it
    let err = e
        .eval("local ok = pcall(function()\n  local x = 1\n  return x\nend)\nreturn ok")
        .unwrap_err();
    assert_eq!(err.to_string(), "stopped by the debugger");
    e.clear_debug_hook();
    assert_eq!(
        e.eval_as::<f64>("local x = 1\nlocal y = 2\nreturn x + y")
            .unwrap(),
        3.0
    );
}

#[test]
fn scripts_run_from_a_hook_dont_re_enter_it() {
    let mut e = Engine::new();
    e.eval("function helper(x)\n  return x * 2\nend").unwrap();
    let helper: Function = e.get("helper").unwrap();
    let calls = Rc::new(RefCell::new(0));
    let (c, h) = (calls.clone(), helper.clone());
    e.set_debug_hook(LINES, move |dbg, _| {
        *c.borrow_mut() += 1;
        let mut cx: Context<'_> = dbg.context();
        let v: f64 = cx.call(&h, 21).map_err(|e: ScriptError| e.to_string())?;
        assert_eq!(v, 42.0);
        Ok(())
    });
    e.eval("local a = 1\nlocal b = 2").unwrap();
    assert_eq!(*calls.borrow(), 2);
}

#[test]
fn natives_calling_back_and_breakpoint_style_use() {
    let mut e = Engine::new();
    // a "breakpoint" on line 3 of a comparator called by `table.sort`
    let src =
        "local t = {3, 1, 2}\ntable.sort(t, function(a, b)\n  return a < b\nend)\nreturn t[1]";
    let hits = trace(&mut e, LINES, src, |dbg, ev| {
        (ev == DebugEvent::Line(3)).then(|| {
            let l = dbg.locals(0);
            format!("{}<{} depth {}", l[0].1, l[1].1, dbg.depth())
        })
    });
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h.ends_with("depth 2")), "{hits:?}");
}
