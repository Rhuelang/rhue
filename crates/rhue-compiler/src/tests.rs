use rhue_parser::{parse_chunk, parse_expr};

use super::*;

fn proto(src: &str, fold: bool) -> Proto {
    let chunk = parse_chunk(src)
        .or_else(|_| parse_expr(src))
        .unwrap_or_else(|e| panic!("{}", e[0].message));
    let p = compile_chunk(
        &chunk,
        &SourceFile::new("test", src),
        CompileOptions {
            fold_constants: fold,
        },
    )
    .unwrap_or_else(|e| panic!("{}", e[0].message));
    assert_eq!(p.code.len(), p.spans.len());
    p
}

fn listing(src: &str, fold: bool) -> String {
    proto(src, fold).disassemble()
}

/// Just the main function's instruction lines, no header, trimmed.
fn code(src: &str) -> Vec<String> {
    listing(src, true)
        .lines()
        .skip(1)
        .take_while(|l| !l.is_empty())
        .map(|l| l.split_whitespace().skip(1).collect::<Vec<_>>().join(" "))
        .collect()
}

fn compile_err(src: &str) -> String {
    let chunk = parse_chunk(src).or_else(|_| parse_expr(src)).unwrap();
    compile_chunk(
        &chunk,
        &SourceFile::new("test", src),
        CompileOptions::default(),
    )
    .unwrap_err()
    .remove(0)
    .message
}

#[test]
fn folded_expression_is_a_single_load() {
    assert_eq!(
        code("(1 + 2) * 3"),
        ["LOADN R0 9", "RETURN R0 1", "RETURN R0 0"]
    );
}

#[test]
fn unfolded_uses_k_forms_and_reuses_the_target() {
    assert_eq!(
        listing("(1 + 2) * 3", false),
        "function <main> (5 instructions, 2 constants, 1 registers)\n\
         \x20    0  LOADN     R0 1\n\
         \x20    1  ADDK      R0 R0 K0        ; 2\n\
         \x20    2  MULK      R0 R0 K1        ; 3\n\
         \x20    3  RETURN    R0 1\n\
         \x20    4  RETURN    R0 0\n"
    );
}

#[test]
fn locals_are_read_in_place() {
    // `a` and `b` live in R0/R1, `a + b` reads them without copying
    assert_eq!(
        code("local a, b = 1, 2 return a + b"),
        [
            "LOADN R0 1",
            "LOADN R1 2",
            "ADD R2 R0 R1",
            "RETURN R2 1",
            "RETURN R0 0"
        ]
    );
}

#[test]
fn assignment_targets_the_local_directly() {
    assert_eq!(
        code("local x = 1 x = x + 1"),
        ["LOADN R0 1", "ADDK R0 R0 K0 ; 1", "RETURN R0 0"]
    );
}

#[test]
fn and_or_assignment_goes_through_a_temporary() {
    // writing `a` into `x` first would clobber the `x` read on the right
    assert_eq!(
        code("local x, a x = a and x"),
        [
            "LOADNIL R0",
            "LOADNIL R1",
            "MOVE R2 R1",
            "JMPIFNOT R2 -> 5",
            "MOVE R2 R0",
            "MOVE R0 R2",
            "RETURN R0 0"
        ]
    );
}

#[test]
fn globals_use_name_constants() {
    assert_eq!(
        code("g = 1 return g"),
        [
            "LOADN R0 1",
            "SETGLOBAL R0 K0 ; \"g\"",
            "GETGLOBAL R0 K0 ; \"g\"",
            "RETURN R0 1",
            "RETURN R0 0"
        ]
    );
}

#[test]
fn comparisons_compile_to_compare_and_jump() {
    assert_eq!(
        code("local a, b return a < b"),
        [
            "LOADNIL R0",
            "LOADNIL R1",
            "LT true R0 R1",
            "JMP -> 5",
            "LOADB R2 false skip",
            "LOADB R2 true",
            "RETURN R2 1",
            "RETURN R0 0",
        ]
    );
    // `a > b` swaps operands: `b < a`
    assert!(code("local a, b return a > b").contains(&"LT true R1 R0".to_string()));
    // `~=` is `==` with the expected result flipped
    assert!(code("local a, b return a ~= b").contains(&"EQ false R0 R1".to_string()));
}

#[test]
fn conditions_short_circuit_without_booleans() {
    // no LOADB, `a and b` in an `if` is two conditional jumps to the else
    let c = code("local a, b, x if a and b then x = 1 end");
    assert_eq!(
        c,
        [
            "LOADNIL R0",
            "LOADNIL R1",
            "LOADNIL R2",
            "JMPIFNOT R0 -> 6",
            "JMPIFNOT R1 -> 6",
            "LOADN R2 1",
            "RETURN R0 0"
        ]
    );
}

#[test]
fn constant_conditions_fold_away_the_test() {
    assert_eq!(
        code("local x while true do x = 1 end"),
        ["LOADNIL R0", "LOADN R0 1", "JMP -> 1", "RETURN R0 0"]
    );
    assert_eq!(
        code("local x if false then x = 1 end"),
        ["LOADNIL R0", "JMP -> 3", "LOADN R0 1", "RETURN R0 0"]
    );
}

#[test]
fn numeric_for_layout() {
    assert_eq!(
        code("local s = 0 for i = 1, 10 do s += i end return s"),
        [
            "LOADN R0 0",
            "LOADN R1 1",
            "LOADN R2 10",
            "LOADN R3 1",
            "FORPREP R1 -> 7",
            "ADD R0 R0 R4",
            "FORLOOP R1 -> 5",
            "RETURN R0 1",
            "RETURN R0 0",
        ]
    );
}

#[test]
fn break_and_continue_targets() {
    let c = code(
        "local i = 0 while i < 10 do i += 1 if i == 5 then continue end if i == 8 then break end end",
    );
    // continue → loop start (pc 1), break → after the loop
    let last = c.len() - 1;
    assert!(c.contains(&"JMP -> 1".to_string()), "{c:#?}");
    assert!(c.contains(&format!("JMP -> {last}")), "{c:#?}");
}

#[test]
fn registers_are_reused_across_statements_and_scopes() {
    let p = proto(
        "do local a, b, c = 1, 2, 3 end do local d = 4 end local e = (1 + 2) * 3",
        false,
    );
    assert_eq!(p.max_stack, 3);
}

#[test]
fn constants_are_deduplicated() {
    let p = proto("return 0.5 + 0.5 + 0.5, 'x', 'x'", false);
    assert_eq!(
        p.constants,
        vec![Constant::Number(0.5), Constant::String("x".into())]
    );
}

#[test]
fn negative_zero_is_kept() {
    let p = proto("-0", true);
    assert_eq!(p.code[0].op(), Some(Op::LoadK));
    let Constant::Number(z) = p.constants[0] else {
        panic!()
    };
    assert!(z == 0.0 && z.is_sign_negative());
}

#[test]
fn register_pressure_grows_with_right_nesting_only() {
    let left = format!("{}1{}", "(".repeat(100), " + true)".repeat(100));
    assert_eq!(proto(&left, true).max_stack, 2);
    let right = format!("{}true{}", "true + (".repeat(100), ")".repeat(100));
    assert_eq!(proto(&right, true).max_stack, 101);
}

#[test]
fn too_many_registers_is_a_diagnostic() {
    let nested = |levels: usize| format!("{}true{}", "true + (".repeat(levels), ")".repeat(levels));
    assert_eq!(proto(&nested(249), true).max_stack, 250);
    assert_eq!(compile_err(&nested(250)), "function is too complex");
    let many_locals = format!(
        "local {}",
        (0..260)
            .map(|i| format!("v{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    assert_eq!(compile_err(&many_locals), "function is too complex");
}

#[test]
fn resolver_errors_surface_through_compile() {
    assert_eq!(compile_err("break"), "`break` outside a loop");
}

#[test]
fn calls_put_arguments_above_the_callee() {
    assert_eq!(
        code("local f f(1, 2)"),
        [
            "LOADNIL R0",
            "MOVE R1 R0",
            "LOADN R2 1",
            "LOADN R3 2",
            "CALL R1 2 -> 0",
            "RETURN R0 0"
        ]
    );
    // a trailing call passes all its results through (`B = 0`, `C = 0`), and
    // `return f(...)` is a tail call
    assert_eq!(
        code("local f, g return f(g())"),
        [
            "LOADNIL R0",
            "LOADNIL R1",
            "MOVE R2 R0",
            "MOVE R3 R1",
            "CALL R3 0 -> ...",
            "TAILCALL R2 ...",
            "RETURN R2 ...",
            "RETURN R0 0"
        ]
    );
    // parentheses keep one result, so that's a normal call
    assert_eq!(
        code("local f return (f())"),
        [
            "LOADNIL R0",
            "MOVE R1 R0",
            "CALL R1 0 -> 1",
            "RETURN R1 1",
            "RETURN R0 0"
        ]
    );
}

#[test]
fn closures_capture_registers_and_upvalues() {
    let p = proto(
        "local a local function f() return function() return a end end",
        true,
    );
    let f = &p.protos[0];
    let inner = &f.protos[0];
    // f captures main's register 0, the inner function reuses f's upvalue 0
    assert_eq!(
        (
            f.upvalues[0].in_stack,
            f.upvalues[0].index,
            &*f.upvalues[0].name
        ),
        (true, 0, "a")
    );
    assert_eq!(
        (inner.upvalues[0].in_stack, inner.upvalues[0].index),
        (false, 0)
    );
}

#[test]
fn close_is_emitted_only_for_captured_locals() {
    let plain = code("do local x = 1 end");
    assert!(!plain.iter().any(|l| l.starts_with("CLOSE")), "{plain:?}");
    let captured = code("local f do local x = 1 f = function() return x end end");
    assert!(captured.iter().any(|l| l == "CLOSE R1"), "{captured:?}");
}

#[test]
fn varargs_and_multiple_results() {
    assert_eq!(code("local a, b = ..."), ["VARARG R0 2", "RETURN R0 0"]);
    assert_eq!(
        code("return ..."),
        ["VARARG R0 ...", "RETURN R0 ...", "RETURN R0 0"]
    );
}

#[test]
fn nested_functions_are_listed_after_their_parent() {
    let out = listing("local function add(a, b) return a + b end", true);
    assert!(
        out.contains("function add (3 instructions, 0 constants, 3 registers, 2 params)"),
        "{out}"
    );
}

#[test]
fn field_access_uses_constant_keys() {
    assert_eq!(
        code("local t = {} t.x = 1 return t.x"),
        [
            "NEWTABLE R0 0 0",
            "LOADN R1 1",
            "SETFIELD R0 K0 R1 ; \"x\"",
            "GETFIELD R1 R0 K0 ; \"x\"",
            "RETURN R1 1",
            "RETURN R0 0"
        ]
    );
}

#[test]
fn constructors_batch_positional_values() {
    assert_eq!(
        code("return {1, 2, k = 3}"),
        [
            "DUPTABLE R0 T0 ; {k}",
            "LOADN R1 1",
            "LOADN R2 2",
            "LOADN R3 3",
            "SETFIELD R0 K0 R3 ; \"k\"",
            "SETLIST R0 2",
            "DATA 0",
            "RETURN R0 1",
            "RETURN R0 0"
        ]
    );
    let big = format!("return {{{}}}", vec!["0"; 120].join(","));
    let c = code(&big);
    let data: Vec<&String> = c.iter().filter(|l| l.starts_with("DATA")).collect();
    assert_eq!(data, ["DATA 0", "DATA 50", "DATA 100"]);
}

#[test]
fn method_calls_use_self() {
    assert_eq!(
        code("local o o:m(1)"),
        [
            "LOADNIL R0",
            "SELF R1 R0 K0 ; \"m\"",
            "LOADN R3 1",
            "CALL R1 2 -> 0",
            "RETURN R0 0"
        ]
    );
}

#[test]
fn generic_for_layout() {
    assert_eq!(
        code("for k, v in t do end"),
        [
            "GETGLOBAL R0 K0 ; \"t\"",
            "LOADNIL R1",
            "LOADNIL R2",
            "JMP -> 4",
            "TFORCALL R0 2",
            "TFORLOOP R0 -> 4",
            "RETURN R0 0"
        ]
    );
}

#[test]
fn compound_index_assignment_evaluates_once() {
    assert_eq!(
        code("local t t.n += 1"),
        [
            "LOADNIL R0",
            "GETFIELD R1 R0 K0 ; \"n\"",
            "ADDK R1 R1 K1 ; 1",
            "SETFIELD R0 K0 R1 ; \"n\"",
            "RETURN R0 0"
        ]
    );
}
