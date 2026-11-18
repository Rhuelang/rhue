//! Milestone 7: performance work that must not change behaviour. Inline
//! caches meeting tables of different shapes, the `..K` comparisons and
//! fused jumps, the instruction budget's new accounting, the register
//! window and spare stack slots under the collector, natives that never
//! touch the VM, and the verifier's place in the pipeline.

use rhue::{Context, Engine, ErrorKind, Function, ScriptError, Value};

fn engines() -> Vec<Engine> {
    let plain = Engine::new();
    let mut stressed = Engine::new();
    stressed.set_gc_stress(true);
    vec![plain, stressed]
}

fn each(test: impl Fn(&mut Engine)) {
    for mut e in engines() {
        test(&mut e);
    }
}

fn eval_str(e: &mut Engine, src: &str) -> String {
    e.eval(src)
        .unwrap_or_else(|err| panic!("{}", err.render(false)))
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

// --- inline caches -------------------------------------------------------------

#[test]
fn field_caches_survive_tables_of_different_shapes() {
    each(|e| {
        // One GETFIELD site sees `x` at a different position in each table,
        // and in tables that lack it entirely.
        let src = "
            local shapes = {
                {x = 1},
                {a = 0, x = 2},
                {a = 0, b = 0, c = 0, x = 3},
                {a = 0},
                {},
                {a = 0, b = 0, c = 0, d = 0, e = 0, f = 0, g = 0, h = 0, i = 0, x = 4},
            }
            local out = {}
            for round = 1, 3 do
                for i, t in ipairs(shapes) do
                    out[#out + 1] = t.x or 'nil'
                end
            end
            return table.concat(out, ',')";
        assert_eq!(
            eval_str(e, src),
            "1,2,3,nil,nil,4,1,2,3,nil,nil,4,1,2,3,nil,nil,4"
        );
    });
}

#[test]
fn field_caches_follow_deletes_and_reinserts() {
    each(|e| {
        let src = "
            local t = {a = 1, b = 2, c = 3}
            local seen = {}
            for i = 1, 4 do
                seen[#seen + 1] = tostring(t.c)
                if i == 1 then t.a = nil end      -- tombstone before c
                if i == 2 then t.c = nil end      -- c itself gone
                if i == 3 then t.c = 30 end       -- back, maybe elsewhere
            end
            -- Enough churn to compact the tombstones and move entries.
            for i = 1, 40 do t['k' .. i] = i end
            for i = 1, 40 do t['k' .. i] = nil end
            for i = 41, 60 do t['k' .. i] = i end
            seen[#seen + 1] = tostring(t.c)
            seen[#seen + 1] = tostring(t.b)
            return table.concat(seen, ',')";
        assert_eq!(eval_str(e, src), "3,3,nil,30,30,2");
    });
}

#[test]
fn set_field_caches_only_overwrite_existing_keys() {
    each(|e| {
        let src = "
            local function mark(t) t.seen = true t.count = (t.count or 0) + 1 return t end
            local a = mark(mark({}))
            local b = mark({count = 10, other = 1})
            local c = mark({other = 1, seen = false})
            return a.count, b.count, c.count, a.seen, c.seen, b.other";
        assert_eq!(eval_str(e, src), "2 11 1 true true 1");
    });
}

#[test]
fn global_caches_follow_removal_and_redefinition() {
    each(|e| {
        let src = "
            g1, g2, g3 = 1, 2, 3
            local function read() return tostring(g1) .. tostring(g2) .. tostring(g3) end
            local out = {read()}
            g1 = nil             -- removal moves another global into its slot
            out[#out + 1] = read()
            g1 = 10
            out[#out + 1] = read()
            return table.concat(out, ' ')";
        assert_eq!(eval_str(e, src), "123 nil23 1023");
    });
}

#[test]
fn small_tables_grow_into_hashed_ones_without_losing_order() {
    each(|e| {
        let src = "
            local t = {}
            for i = 1, 20 do t['k' .. i] = i end
            local order = {}
            for k, v in pairs(t) do order[#order + 1] = v end
            local s = 0
            for i = 1, 20 do s += t['k' .. i] end
            return table.concat(order, ','), s";
        assert_eq!(
            eval_str(e, src),
            "1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20 210"
        );
    });
}

// --- comparisons against constants ---------------------------------------------

#[test]
fn constant_comparisons_match_register_comparisons() {
    let mut e = Engine::new();
    // Each comparison written with the constant on the right, on the left,
    // and with both sides in registers must agree, NaN and strings included.
    let values = ["-1", "0", "1", "2.5", "0/0", "math.huge", "-math.huge"];
    let constants = ["0", "1", "2.5", "-1"];
    let ops = ["==", "~=", "<", "<=", ">", ">="];
    for v in values {
        for k in constants {
            for op in ops {
                let src = format!(
                    "local v = {v} local k = {k}
                     return (v {op} {k}), ({k} {op} v), (v {op} k), (k {op} v)"
                );
                let got = eval_str(&mut e, &src);
                let parts: Vec<&str> = got.split(' ').collect();
                assert_eq!(parts[0], parts[2], "{v} {op} {k}: {got}");
                assert_eq!(parts[1], parts[3], "{k} {op} {v}: {got}");
            }
        }
    }
    let src = "local s = 'b'
               return s < 'c', 'a' < s, s >= 'b', 'b' == s, s ~= 'b', s > 'a'";
    assert_eq!(eval_str(&mut e, src), "true true true true false true");
}

#[test]
fn constant_comparison_errors_read_like_lua() {
    let mut e = Engine::new();
    let cases = [
        (
            "local x = 's' return x < 1",
            "attempt to compare string < number",
        ),
        (
            "local x = 's' return 1 < x",
            "attempt to compare number < string",
        ),
        (
            "local x = 's' return x > 1",
            "attempt to compare number < string",
        ),
        (
            "local x = {} return x >= 1",
            "attempt to compare number <= table",
        ),
        (
            "local x = {} return 1 >= x",
            "attempt to compare table <= number",
        ),
        (
            "local x = nil return x <= 'a'",
            "attempt to compare nil <= string",
        ),
    ];
    for (src, want) in cases {
        let err = e.eval(src).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Runtime);
        assert_eq!(err.to_string(), want, "{src}");
    }
    // Equality never errors.
    assert_eq!(
        eval_str(&mut e, "local t = {} return t == 1, 'a' ~= t"),
        "false true"
    );
}

#[test]
fn comparisons_drive_loops_and_branches() {
    let mut e = Engine::new();
    let src = "
        local n, steps = 27, 0
        while n ~= 1 do
            if n % 2 == 0 then n = n // 2 else n = 3 * n + 1 end
            steps += 1
        end
        local hits = 0
        for i = 1, 100 do
            if i > 10 and i <= 20 or i == 50 then hits += 1 end
        end
        repeat steps -= 100 until steps < 0
        return steps, hits";
    assert_eq!(eval_str(&mut e, src), "-89 11");
}

// --- the instruction budget ----------------------------------------------------

#[test]
fn every_kind_of_loop_is_stopped_by_the_instruction_limit() {
    let loops = [
        "while true do end",
        "repeat local x = 1 until false",
        "for i = 1, math.huge do end",
        "local t = {} for i = 1, 10 do t[i] = i end while true do for _ in t do end end",
        "local t = setmetatable and {} or {} while true do for _, v in ipairs({1, 2, 3}) do end end",
        "local function f() return f() end f()",
        "local function f(n) if n > 0 then return f(n - 1) end return f(100) end f(1)",
        "local x = 0 while x >= 0 do x = x + 1 if x > 5 then x = 0 end end",
        "local t = {} while true do table.sort({3, 2, 1}, function(a, b) return a < b end) end",
    ];
    for src in loops {
        let mut e = Engine::new();
        e.set_instruction_limit(Some(50_000));
        let err = e.eval(src).expect_err(src);
        assert!(
            err.to_string().contains("instruction limit")
                || err.to_string().contains("stack overflow"),
            "{src}: {err}"
        );
    }
}

#[test]
fn the_instruction_limit_is_roughly_an_instruction_count() {
    // A loop body of a few instructions, run 1000 times, fits in a budget
    // a little above its size and not in one well below it.
    let src = "local s = 0 for i = 1, 1000 do s += i * 2 end return s";
    let mut e = Engine::new();
    e.set_instruction_limit(Some(10_000));
    assert_eq!(eval_str(&mut e, src), "1001000");
    e.set_instruction_limit(Some(1_000));
    assert!(e.eval(src).is_err());
}

// --- the stack ------------------------------------------------------------------

#[test]
fn natives_calling_scripts_in_a_loop_reuse_the_stack() {
    each(|e| {
        e.register(
            "sum_with",
            |cx: &mut Context, f: Function, n: i64| -> Result<f64, ScriptError> {
                let mut total = 0.0;
                for i in 1..=n {
                    total += cx.call::<f64>(&f, (i,))?;
                }
                Ok(total)
            },
        );
        let src = "
            local junk = {}
            local total = sum_with(function(i)
                junk[i % 7] = {i, tostring(i)}   -- garbage between calls
                return i * 2
            end, 2000)
            return total, #junk";
        assert_eq!(eval_str(e, src), "4002000 6");
    });
}

#[test]
fn deep_and_wide_frames_keep_their_values_under_collection() {
    each(|e| {
        // Frames at many depths, each with many registers live, while
        // natives and the collector run in between.
        let mut src = String::from("local function f(n)\n");
        for i in 0..60 {
            src.push_str(&format!("local v{i} = {{n = n + {i}}}\n"));
        }
        src.push_str("if n > 0 then f(n - 1) end\nlocal s = 0\n");
        for i in 0..60 {
            src.push_str(&format!("s += v{i}.n\n"));
        }
        src.push_str("tostring(s) return s\nend\nreturn f(20)");
        assert_eq!(eval_str(e, &src), "2970");
    });
}

#[test]
fn many_results_and_arguments_cross_the_register_window() {
    each(|e| {
        let src = "
            local function many(n) local t = {} for i = 1, n do t[i] = i end return table.unpack(t) end
            local function count(...) return select('#', ...) end
            local function sum(...) local s = 0 for _, v in {...} do s += v end return s end
            return count(many(600)), sum(many(600)), #{many(300), many(300)}";
        assert_eq!(eval_str(e, src), "600 180300 301");
    });
}

// --- natives --------------------------------------------------------------------

#[test]
fn typed_natives_convert_arguments_lazily_and_report_the_right_index() {
    let mut e = Engine::new();
    e.register("pair", |a: f64, b: String| format!("{a}:{b}"));
    e.register("count", |n: i64| n + 1);
    e.register("tail", |first: f64, rest: rhue::Variadic<f64>| {
        first + rest.iter().sum::<f64>()
    });
    assert_eq!(
        eval_str(&mut e, "return pair(1, 'x'), count(41), tail(1, 2, 3)"),
        "1:x 42 6"
    );
    let err = e.eval("return pair(1, 2)").unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument #2 to 'pair' (string expected, got number)"
    );
    let err = e.eval("return tail(1, 2, 'x')").unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument #3 to 'tail' (number expected, got string)"
    );
    let err = e.eval("return count()").unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid argument #1 to 'count' (number expected, got nil)"
    );
    // Results of several kinds come back intact.
    e.register("three", || (1, "two", Value::Bool(true)));
    assert_eq!(eval_str(&mut e, "return three()"), "1 two true");
}

// --- the verifier -----------------------------------------------------------------

#[test]
fn compiled_chunks_verify() {
    let mut e = Engine::new();
    let p = e
        .compile(
            "v",
            "local t = {...} local function f(a, ...) return a, ... end
             for k, v in pairs(t) do print(k, v) end
             return f(table.unpack(t))",
        )
        .unwrap();
    rhue_bytecode::verify(&p).unwrap();
}

#[test]
fn hand_built_backward_branches_are_charged_too() {
    use rhue_bytecode::{Instr, Op, Proto};
    use std::rc::Rc;
    // `JMPIF` jumping back to itself: the compiler never emits a backward
    // conditional jump, but the budget must still hold for any bytecode
    // that verifies.
    let code = vec![
        Instr::abc(Op::LoadB, 0, 1, 0),
        Instr::asbx(Op::JmpIf, 0, -1),
        Instr::abc(Op::Return, 0, 0, 0),
    ];
    let p = Proto {
        spans: vec![rhue::Span::default(); code.len()],
        code,
        max_stack: 1,
        is_vararg: true,
        ..Proto::default()
    };
    rhue_bytecode::verify(&p).unwrap();
    let mut e = Engine::new();
    e.set_instruction_limit(Some(10_000));
    let err = e.run(&Rc::new(p)).unwrap_err();
    assert!(err.to_string().contains("instruction limit"), "{err}");
}

#[test]
fn unpack_ranges_that_overflow_are_rejected() {
    let mut e = Engine::new();
    for src in [
        "return table.unpack({1, 2}, -1e300, 1e300)",
        "return table.unpack({1, 2}, -9e18, 9e18)",
    ] {
        let err = e.eval(src).unwrap_err();
        assert_eq!(err.to_string(), "too many results to unpack");
    }
}

#[test]
fn the_engine_refuses_bytecode_that_does_not_verify() {
    use rhue_bytecode::{Instr, Op, Proto};
    use std::rc::Rc;
    let code = vec![
        Instr::abc(Op::Move, 9, 0, 0),
        Instr::abc(Op::Return, 0, 0, 0),
    ];
    let p = Proto {
        spans: vec![rhue::Span::default(); code.len()],
        code,
        max_stack: 2,
        ..Proto::default()
    };
    let mut e = Engine::new();
    let err = e.run(&Rc::new(p)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Compile);
    assert_eq!(
        err.to_string(),
        "invalid bytecode in <main> at 0: target register R9 is beyond max_stack 2"
    );
    // Compiled chunks are marked verified, so running them again is free.
    let p = e.compile("ok", "return 1").unwrap();
    assert!(p.verified.get());
}
