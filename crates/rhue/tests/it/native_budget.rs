//! Natives that loop on their own (the pattern matcher, `table.move`...) draw
//! on the same instruction budget scripts do, and interrupts reach them.

use std::time::{Duration, Instant};

use rhue::Engine;

/// Six lazy wildcards and a letter that's never there, about 600⁶ tries.
const BACKTRACK: &str = r#"return string.find(string.rep("a", 600), string.rep(".-", 6) .. "b")"#;

#[test]
fn the_limit_stops_a_backtracking_pattern() {
    let mut e = Engine::new();
    e.set_instruction_limit(Some(1_000_000));
    let t = Instant::now();
    let err = e.eval(BACKTRACK).unwrap_err();
    assert!(err.to_string().contains("instruction limit"), "{err}");
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    // and pcall can't catch it
    let err = e
        .eval(&format!("return pcall(function() {} end)", &BACKTRACK[7..]))
        .unwrap_err();
    assert!(err.to_string().contains("instruction limit"), "{err}");
}

#[test]
fn an_interrupt_reaches_a_backtracking_pattern() {
    let mut e = Engine::new();
    let handle = e.interrupt_handle();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        handle.interrupt();
    });
    let t = Instant::now();
    let err = e.eval(BACKTRACK).unwrap_err();
    assert!(err.to_string().contains("interrupted"), "{err}");
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

#[test]
fn an_interrupt_reaches_a_loop_of_pattern_calls() {
    // each call is cheap, but they have to keep using up the slice or the
    // interrupt never gets looked at
    for src in [
        r#"while true do string.match("x", "x") end"#,
        r#"while true do string.gsub("x", "x", function(c) return c end) end"#,
        r#"while true do for _ in string.gmatch("xx", "x") do end end"#,
    ] {
        let mut e = Engine::new();
        let handle = e.interrupt_handle();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            handle.interrupt();
        });
        let t = Instant::now();
        let err = e.eval(src).unwrap_err();
        assert!(err.to_string().contains("interrupted"), "{src}: {err}");
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "{src}: {:?}",
            t.elapsed()
        );
    }
}

#[test]
fn the_budget_comes_back_after_a_pattern_error() {
    let mut e = Engine::new();
    e.set_instruction_limit(Some(1_000_000));
    let got: f64 = e
        .eval_as(
            "local ok = pcall(string.find, 'a', '%') \
             local n = 0 for i = 1, 1000 do n += #string.gsub('abc', '%w', function(c) return c .. c end) end \
             return if ok then -1 else n",
        )
        .unwrap();
    assert_eq!(got, 6000.0);
}

#[test]
fn gmatch_and_gsub_are_charged_too() {
    for src in [
        r#"for _ in string.gmatch(string.rep("a", 600), string.rep(".-", 6) .. "b") do end"#,
        r#"string.gsub(string.rep("a", 600), string.rep(".-", 6) .. "b", "")"#,
    ] {
        let mut e = Engine::new();
        e.set_instruction_limit(Some(1_000_000));
        let err = e.eval(src).unwrap_err();
        assert!(
            err.to_string().contains("instruction limit"),
            "{src}: {err}"
        );
    }
}

#[test]
fn big_table_moves_are_charged() {
    let mut e = Engine::new();
    e.set_instruction_limit(Some(1_000_000));
    // 2^20 elements is 131072 instructions' worth, so a handful of these runs out
    let err = e
        .eval("local t = table.create(1048576, 0) for i = 1, 100 do table.move(t, 1, #t, 1) end")
        .unwrap_err();
    assert!(err.to_string().contains("instruction limit"), "{err}");
}

#[test]
fn host_natives_can_charge_the_budget() {
    let mut e = Engine::new();
    e.set_instruction_limit(Some(100_000));
    e.register(
        "spin",
        |cx: &mut rhue::Context, n: f64| -> Result<f64, rhue::ScriptError> {
            let mut i = 0.0;
            while i < n {
                cx.charge(1)?;
                i += 1.0;
            }
            Ok(i)
        },
    );
    assert_eq!(e.eval_as::<f64>("return spin(1000)").unwrap(), 1000.0);
    let err = e.eval("return pcall(spin, 1e12)").unwrap_err();
    assert!(err.to_string().contains("instruction limit"), "{err}");
}

#[test]
fn empty_and_bad_arguments_stay_cheap() {
    let mut e = Engine::new();
    e.set_instruction_limit(Some(1_000_000));
    // an empty result doesn't build 10^12 empty pieces
    assert_eq!(
        e.eval_as::<String>(r#"return string.rep("", 1e12)"#)
            .unwrap(),
        ""
    );
    assert_eq!(
        e.eval_as::<String>(r#"return string.rep("", 1e12, "")"#)
            .unwrap(),
        ""
    );
    // a j way past the end is still an error pcall catches, not the limit
    let ok: bool = e
        .eval_as(r#"return (pcall(table.concat, {}, "", 1, 1e15))"#)
        .unwrap();
    assert!(!ok);
}

#[test]
fn concat_is_charged_for_what_it_joins() {
    // #t is 0 when t[1] is nil, so a run starting at 2 has to be charged as
    // it goes, not by the table's length
    let mut e = Engine::new();
    e.set_instruction_limit(Some(5_000_000));
    let t = Instant::now();
    let err = e
        .eval(
            "local t = {} for i = 2, 200000 do t[i] = 'x' end \
             while true do table.concat(t, '', 2, 200000) end",
        )
        .unwrap_err();
    assert!(err.to_string().contains("instruction limit"), "{err}");
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
}
