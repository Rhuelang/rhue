use crate::ast::*;
use crate::{MAX_ERRORS, parse_chunk, parse_expr};
use rhue_common::Span;

fn expr_of(chunk: &Chunk) -> &Expr {
    &chunk.body.ret.as_ref().unwrap().values[0]
}

fn sexpr(src: &str) -> String {
    match parse_expr(src) {
        Ok(c) => expr_of(&c).to_string(),
        Err(errs) => panic!("parse error in {src:?}: {}", errs[0].message),
    }
}

fn expr_err(src: &str) -> String {
    parse_expr(src)
        .expect_err("expected a parse error")
        .remove(0)
        .message
}

fn chunk(src: &str) -> Chunk {
    parse_chunk(src).unwrap_or_else(|e| panic!("parse error in {src:?}: {}", e[0].message))
}

fn chunk_errs(src: &str) -> Vec<String> {
    parse_chunk(src)
        .expect_err("expected parse errors")
        .into_iter()
        .map(|d| d.message)
        .collect()
}

#[test]
fn literals() {
    assert_eq!(sexpr("42"), "42");
    assert_eq!(sexpr("nil"), "nil");
    assert_eq!(sexpr("true"), "true");
    assert_eq!(sexpr("(1)"), "(group 1)");
    assert_eq!(sexpr(r#""hi\n""#), r#""hi\n""#);
    assert_eq!(sexpr("x"), "x");
}

#[test]
fn precedence_table() {
    let cases = [
        ("1 + 2 * 3", "(+ 1 (* 2 3))"),
        ("1 * 2 + 3", "(+ (* 1 2) 3)"),
        ("1 - 2 - 3", "(- (- 1 2) 3)"),
        ("8 / 4 / 2", "(/ (/ 8 4) 2)"),
        ("7 // 2 % 3", "(% (// 7 2) 3)"),
        ("2 ^ 3 ^ 2", "(^ 2 (^ 3 2))"),
        ("-2 ^ 2", "(- (^ 2 2))"),
        ("2 ^ -3", "(^ 2 (- 3))"),
        ("-2 * 3", "(* (- 2) 3)"),
        ("- -2", "(- (- 2))"),
        ("not 1 == 2", "(== (not 1) 2)"),
        ("1 + 2 .. 3", "(.. (+ 1 2) 3)"),
        ("1 .. 2 .. 3", "(.. 1 (.. 2 3))"),
        ("1 < 2 == true", "(== (< 1 2) true)"),
        ("1 or 2 and 3", "(or 1 (and 2 3))"),
        ("1 and 2 or 3 and 4", "(or (and 1 2) (and 3 4))"),
        ("#s + 1", "(+ (# s) 1)"),
        ("(1 + 2) * 3", "(* (group (+ 1 2)) 3)"),
        ("2 ^ 3 * 4", "(* (^ 2 3) 4)"),
        ("a .. b == c", "(== (.. a b) c)"),
        ("if a then b else c", "(if a b c)"),
        ("if a then 1 elseif b then 2 else 3", "(if a 1 b 2 3)"),
        ("1 + if a then 2 else 3 + 4", "(+ 1 (if a 2 (+ 3 4)))"),
    ];
    for (src, want) in cases {
        assert_eq!(sexpr(src), want, "source: {src}");
    }
}

#[test]
fn spans_cover_whole_expressions() {
    let c = parse_expr("  1 + (2 * 3)").unwrap();
    let e = expr_of(&c);
    assert_eq!(e.span, Span::new(2, 13));
    let ExprKind::Binary { op_span, rhs, .. } = &e.kind else {
        panic!()
    };
    assert_eq!(*op_span, Span::new(4, 5));
    assert_eq!(rhs.span, Span::new(6, 13));
}

#[test]
fn name_ids_are_dense_and_unique() {
    let c = chunk("local a, b = 1, a\nb = a + b");
    assert_eq!(c.name_count, 6);
    let StmtKind::Local { names, values, .. } = &c.body.stmts[0].kind else {
        panic!()
    };
    assert_eq!((names[0].id, names[1].id), (0, 1));
    let ExprKind::Name(n) = &values[1].kind else {
        panic!()
    };
    assert_eq!((&*n.text, n.id), ("a", 2));
}

#[test]
fn statements() {
    let c = chunk(
        "local x, y = 1\n\
         x = 2; y += x\n\
         s ..= 'a'\n\
         a, b = b, a\n\
         do local z end\n\
         while x < 10 do x += 1 end\n\
         repeat x -= 1 until x == 0\n\
         if x then y = 1 elseif y then y = 2 else y = 3 end\n\
         for i = 1, 10, 2 do break end\n\
         for i = 1, 3 do continue end\n\
         return x, y",
    );
    let kinds: Vec<&str> = c
        .body
        .stmts
        .iter()
        .map(|s| match &s.kind {
            StmtKind::Local { .. } => "local",
            StmtKind::Assign { .. } => "assign",
            StmtKind::CompoundAssign { .. } => "compound",
            StmtKind::Do(_) => "do",
            StmtKind::While { .. } => "while",
            StmtKind::Repeat { .. } => "repeat",
            StmtKind::If { .. } => "if",
            StmtKind::NumericFor { .. } => "for",
            StmtKind::Break => "break",
            StmtKind::Continue => "continue",
            StmtKind::Call(_) => "call",
            StmtKind::Function { .. } => "function",
            StmtKind::LocalFunction { .. } => "local function",
            StmtKind::GenericFor { .. } => "for in",
            StmtKind::TypeAlias(_) => "type",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "local", "assign", "compound", "compound", "assign", "do", "while", "repeat", "if",
            "for", "for"
        ]
    );
    assert_eq!(c.body.ret.unwrap().values.len(), 2);
}

#[test]
fn if_statement_shape() {
    let c = chunk("if a then elseif b then x = 1 end");
    let StmtKind::If {
        branches,
        else_block,
    } = &c.body.stmts[0].kind
    else {
        panic!()
    };
    assert_eq!(branches.len(), 2);
    assert!(else_block.is_none());
    assert_eq!(branches[1].1.stmts.len(), 1);
}

#[test]
fn continue_is_contextual() {
    let c = chunk("while true do continue end");
    let StmtKind::While { body, .. } = &c.body.stmts[0].kind else {
        panic!()
    };
    assert_eq!(body.stmts[0].kind, StmtKind::Continue);
    // still assignable as a plain variable
    let c = chunk("continue = 1");
    assert!(matches!(c.body.stmts[0].kind, StmtKind::Assign { .. }));
}

#[test]
fn chunk_with_returns() {
    let c = chunk("return 1, 2 + 3;");
    let r = c.body.ret.unwrap();
    assert_eq!(r.values.len(), 2);
    assert_eq!(r.values[1].to_string(), "(+ 2 3)");
    assert!(chunk("return").body.ret.unwrap().values.is_empty());
    assert!(chunk("-- just a comment\n").body.ret.is_none());
}

#[test]
fn expression_errors() {
    assert_eq!(
        expr_err("1 +"),
        "expected an expression, found end of input"
    );
    assert_eq!(
        expr_err("(1 + 2"),
        "expected `)` to close `(`, found end of input"
    );
    assert_eq!(
        expr_err("1 2"),
        "expected end of input after expression, found number"
    );
    assert_eq!(expr_err("* 2"), "expected an expression, found `*`");
    assert_eq!(expr_err("1 $ 2"), "unexpected character `$`");
    // a token, for `@class` in definitions files
    assert_eq!(
        expr_err("1 @ 2"),
        "expected end of input after expression, found `@`"
    );
    assert_eq!(
        expr_err("if a then b"),
        "expected `else` in `if` expression (the `else` branch is required), found end of input"
    );
    assert_eq!(
        expr_err(r#""s"(1)"#),
        "a string literal can't be called directly"
    );
    assert_eq!(
        expr_err("t:m"),
        "expected arguments after method `m`, found end of input"
    );
    assert_eq!(
        expr_err("{1, 2"),
        "expected `}` to close the table, found end of input"
    );
    assert_eq!(
        expr_err("t[1"),
        "expected `]` to close `[`, found end of input"
    );
    assert_eq!(expr_err(r#""\q""#), "invalid escape sequence");
}

#[test]
fn statement_errors() {
    assert_eq!(chunk_errs("1 + 2"), ["expected a statement, found number"]);
    assert_eq!(
        chunk_errs("x"),
        ["incomplete statement: expected an assignment"]
    );
    assert_eq!(
        chunk_errs("x + 1 = 2"),
        ["cannot assign to this expression"]
    );
    assert_eq!(chunk_errs("(x) = 2"), ["cannot assign to this expression"]);
    assert_eq!(
        chunk_errs("return 1 return 2"),
        ["expected end of block after `return`, found `return`"]
    );
    assert_eq!(
        chunk_errs("local function f(x: ) end"),
        ["expected a type, found `)`"]
    );
    assert_eq!(
        chunk_errs("f() + 1"),
        ["incomplete statement: expected an assignment"]
    );
    assert_eq!(
        chunk_errs("t:m() = 1"),
        ["cannot assign to this expression"]
    );
    assert_eq!(
        chunk_errs("local x: {number = 1"),
        ["expected `}` to close the array type `{T}`, found `=`"]
    );
}

#[test]
fn missing_end_points_at_opener() {
    let errs = parse_chunk("while x do\n  x = 1\n").unwrap_err();
    assert_eq!(
        errs[0].message,
        "expected `end` to close `while`, found end of input"
    );
    assert_eq!(errs[0].secondary[0].span, Span::new(0, 5));
}

#[test]
fn recovers_and_reports_several_errors() {
    let errs = chunk_errs(
        "local a = \n\
         local b = 2\n\
         c = * 3\n\
         if b then d = ) end\n\
         local ok = 1",
    );
    assert_eq!(
        errs,
        [
            "expected an expression, found `local`",
            "expected an expression, found `*`",
            "expected an expression, found `)`",
        ]
    );
}

#[test]
fn type_errors_do_not_cascade() {
    assert_eq!(
        chunk_errs(
            "local a: number | = 1\nlocal function g<T = number>() end\nlocal b: (number, string) = 2\nz = 1 +"
        ),
        [
            "expected a type, found `=`",
            "default type parameters are only allowed in type aliases",
            "expected `->` after a function type's parameters, found `=`",
            "expected an expression, found end of input",
        ]
    );
}

#[test]
fn generic_packs_and_defaults() {
    let src = "type F<T, U = T, R... = (number, ...string)> = (T, R...) -> R...\nlocal function f<A...>(...: A...): (string, A...) return 'x', ... end\nlocal g: F<number, string, ()>\nlocal h: H<...number>";
    let chunk = chunk(src);
    let shown = format!("{:?}", chunk);
    assert!(shown.contains("GenericPack(\"A\")"), "{shown}");
    assert!(shown.contains("pack: true"), "{shown}");
}

#[test]
fn calls() {
    assert_eq!(sexpr("f()"), "(call f)");
    assert_eq!(sexpr("f(1, g(2))"), "(call f 1 (call g 2))");
    assert_eq!(sexpr("f 'x'"), "(call f \"x\")");
    assert_eq!(sexpr("f(1)(2)"), "(call (call f 1) 2)");
    assert_eq!(sexpr("(f)(1)"), "(call (group f) 1)");
    assert_eq!(sexpr("-f(1) ^ 2"), "(- (^ (call f 1) 2))");
    let c = chunk("print('hi') f()(1)");
    assert!(matches!(c.body.stmts[0].kind, StmtKind::Call(_)));
    assert!(matches!(c.body.stmts[1].kind, StmtKind::Call(_)));
}

#[test]
fn functions() {
    assert_eq!(sexpr("function(a, b, ...) end"), "(function (a b ...))");
    assert_eq!(sexpr("function() return 1 end"), "(function ())");
    let c = chunk("local function fact(n) if n <= 1 then return 1 end return n * fact(n - 1) end");
    let StmtKind::LocalFunction { name, func } = &c.body.stmts[0].kind else {
        panic!()
    };
    assert_eq!(&*name.text, "fact");
    assert_eq!(func.name.as_deref(), Some("fact"));
    assert_eq!(func.params.len(), 1);
    assert_eq!(func.body.stmts.len(), 1);
    assert!(func.body.ret.is_some());
    let c = chunk("function greet(...) return ... end");
    let StmtKind::Function { func, .. } = &c.body.stmts[0].kind else {
        panic!()
    };
    assert!(func.is_vararg && func.params.is_empty());
}

#[test]
fn missing_function_end_points_at_keyword() {
    let errs = parse_chunk("local function f()\n  return 1\n").unwrap_err();
    assert_eq!(
        errs[0].message,
        "expected `end` to close `function`, found end of input"
    );
    assert_eq!(errs[0].secondary[0].span, Span::new(0, 5));
}

#[test]
fn stray_closers_are_reported_once_each() {
    assert_eq!(chunk_errs("x = 1 end y = 2"), ["unexpected `end`"]);
}

#[test]
fn error_count_is_capped() {
    let src = "x = *\n".repeat(100);
    assert_eq!(parse_chunk(&src).unwrap_err().len(), MAX_ERRORS);
}

#[test]
fn unclosed_paren_points_at_opener() {
    let d = parse_expr("(1 + 2").unwrap_err().remove(0);
    assert_eq!(d.secondary[0].span, Span::new(0, 1));
}

#[test]
fn nesting_limit() {
    let deep = format!("{}1{}", "(".repeat(400), ")".repeat(400));
    assert!(parse_expr(&deep).is_ok());
    let too_deep = format!("{}1{}", "(".repeat(1200), ")".repeat(1200));
    assert_eq!(expr_err(&too_deep), "expression nests too deeply");
    let chain = format!("1{}", " + 1".repeat(1200));
    assert_eq!(expr_err(&chain), "expression nests too deeply");
    assert!(parse_expr(&format!("1{}", " + 1".repeat(900))).is_ok());
    let unary = format!("{}1", "- ".repeat(1200));
    assert_eq!(expr_err(&unary), "expression nests too deeply");
}

#[test]
fn block_nesting_limit() {
    let ok = format!("{}{}", "do ".repeat(300), "end ".repeat(300));
    assert!(parse_chunk(&ok).is_ok());
    let deep = format!("{}{}", "do ".repeat(5000), "end ".repeat(5000));
    let errs = parse_chunk(&deep).unwrap_err();
    assert_eq!(errs[0].message, "blocks nest too deeply");
    let fns = format!(
        "{}{}",
        "local f = function() ".repeat(5000),
        "end ".repeat(5000)
    );
    assert_eq!(
        parse_chunk(&fns).unwrap_err()[0].message,
        "blocks nest too deeply"
    );
}

#[test]
fn tables_and_indexing() {
    assert_eq!(sexpr("{}"), "(table)");
    assert_eq!(sexpr("{1, 2; 3,}"), "(table 1 2 3)");
    assert_eq!(
        sexpr("{x = 1, [k] = 2, 'v'}"),
        "(table [\"x\"]=1 [k]=2 \"v\")"
    );
    assert_eq!(sexpr("t.a.b"), "(index (index t \"a\") \"b\")");
    assert_eq!(sexpr("t[i + 1]"), "(index t (+ i 1))");
    assert_eq!(sexpr("t.f(1)"), "(call (index t \"f\") 1)");
    assert_eq!(sexpr("obj:m(1, 2)"), "(method obj m 1 2)");
    assert_eq!(sexpr("obj:m 's'"), "(method obj m \"s\")");
    assert_eq!(sexpr("f{1}"), "(call f (table 1))");
    assert_eq!(
        sexpr("a.b:c().d"),
        "(index (method (index a \"b\") c) \"d\")"
    );
    assert_eq!(sexpr("#t.list"), "(# (index t \"list\"))");
    assert_eq!(
        sexpr("{f = function() end}"),
        "(table [\"f\"]=(function ()))"
    );
}

#[test]
fn index_assignment_and_method_statements() {
    let c = chunk("t.x = 1 t[1], t.y = 2, 3 t.n += 1 obj:go()");
    assert!(matches!(c.body.stmts[0].kind, StmtKind::Assign { .. }));
    assert!(matches!(
        c.body.stmts[2].kind,
        StmtKind::CompoundAssign { .. }
    ));
    assert!(matches!(c.body.stmts[3].kind, StmtKind::Call(_)));
}

#[test]
fn dotted_and_method_function_names() {
    let c = chunk("function a.b.c() end function a.b:m(x) return self end");
    let StmtKind::Function { target, func } = &c.body.stmts[0].kind else {
        panic!()
    };
    assert_eq!(target.to_string(), "(index (index a \"b\") \"c\")");
    assert_eq!(func.name.as_deref(), Some("a.b.c"));
    let StmtKind::Function { func, .. } = &c.body.stmts[1].kind else {
        panic!()
    };
    assert_eq!(func.name.as_deref(), Some("a.b:m"));
    let params: Vec<&str> = func.params.iter().map(|p| &*p.text).collect();
    assert_eq!(params, ["self", "x"]);
}

#[test]
fn generic_for() {
    let c = chunk("for k, v in pairs(t) do end for x in t do end");
    let StmtKind::GenericFor { names, exprs, .. } = &c.body.stmts[0].kind else {
        panic!()
    };
    assert_eq!(names.len(), 2);
    assert_eq!(exprs[0].to_string(), "(call pairs t)");
    assert!(matches!(c.body.stmts[1].kind, StmtKind::GenericFor { .. }));
}

// ─ Types (milestone 6) ───────────────────────────────────────────────────────

/// The annotation on the first `local` in `src`, printed back.
fn local_type(src: &str) -> String {
    let c = chunk(src);
    let StmtKind::Local { types, .. } = &c.body.stmts[0].kind else {
        panic!("expected a local");
    };
    types[0].as_ref().expect("annotated").to_string()
}

#[test]
fn type_syntax_round_trips() {
    for (src, want) in [
        ("number", "number"),
        ("nil", "nil"),
        ("string?", "string?"),
        ("number | string | nil", "(number | string | nil)"),
        ("| number | string", "(number | string)"),
        ("A & B", "(A & B)"),
        ("{number}", "{[number]: number}"),
        ("{x: number, y: string?}", "{x: number, y: string?}"),
        (
            "{x: number; [string]: boolean}",
            "{x: number, [string]: boolean}",
        ),
        ("{[\"weird key\"]: number}", "{weird key: number}"),
        ("{}", "{}"),
        ("Map<string, {T}>", "Map<string, {[number]: T}>"),
        ("mod.Type", "mod.Type"),
        ("\"on\" | \"off\"", "(\"on\" | \"off\")"),
        ("true", "true"),
        ("(number) -> string", "(number) -> (string)"),
        ("(a: number, b: string) -> ()", "(number, string) -> ()"),
        (
            "(number, ...string) -> ...boolean",
            "(number, ...string) -> (...boolean)",
        ),
        ("() -> (number, string)", "() -> (number, string)"),
        ("<T>(T, {T}) -> T?", "<T>(T, {[number]: T}) -> (T?)"),
        ("((number) -> number)?", "(number) -> (number)?"),
        ("(number | string)?", "(number | string)?"),
        ("typeof(print)", "typeof(print)"),
        (
            "(number) -> (number) -> number",
            "(number) -> ((number) -> (number))",
        ),
    ] {
        assert_eq!(local_type(&format!("local x: {src}")), want, "{src}");
    }
}

#[test]
fn annotations_everywhere() {
    let c = chunk(
        "local a: number, b = 1, 2
         local function f<T>(x: T, y: number?, ...: string): (T, number)
             return x, 1
         end
         function obj:m(n: number): () end
         for i: number = 1, 10 do end
         for k: string, v: number in pairs(t) do end
         type Point = {x: number, y: number}
         export type Pair<A, B> = {first: A, second: B}
         local z = (y :: number) + w :: number",
    );
    let s = &c.body.stmts;
    let StmtKind::Local { types, .. } = &s[0].kind else {
        panic!()
    };
    assert_eq!(types.len(), 2);
    assert!(types[0].is_some() && types[1].is_none());

    let StmtKind::LocalFunction { func, .. } = &s[1].kind else {
        panic!()
    };
    assert_eq!(func.generics.len(), 1);
    assert_eq!(func.param_types[0].as_ref().unwrap().to_string(), "T");
    assert_eq!(func.param_types[1].as_ref().unwrap().to_string(), "number?");
    assert_eq!(func.vararg_type.as_ref().unwrap().to_string(), "string");
    assert_eq!(
        func.return_type.as_ref().unwrap().to_string(),
        "(T, number)"
    );

    let StmtKind::Function { func, .. } = &s[2].kind else {
        panic!()
    };
    assert!(func.is_method);
    assert_eq!(func.params[0].text.as_ref(), "self");
    assert_eq!(func.param_types.len(), 2);
    assert!(func.param_types[0].is_none());
    assert_eq!(func.return_type.as_ref().unwrap().to_string(), "()");

    let StmtKind::NumericFor { var_type, .. } = &s[3].kind else {
        panic!()
    };
    assert!(var_type.is_some());
    let StmtKind::GenericFor { types, .. } = &s[4].kind else {
        panic!()
    };
    assert_eq!(types.iter().flatten().count(), 2);

    let StmtKind::TypeAlias(a) = &s[5].kind else {
        panic!()
    };
    assert!(!a.exported && a.generics.is_empty());
    let StmtKind::TypeAlias(a) = &s[6].kind else {
        panic!()
    };
    assert!(a.exported);
    assert_eq!(a.generics.len(), 2);

    // `::` binds tighter than `+`
    let StmtKind::Local { values, .. } = &s[7].kind else {
        panic!()
    };
    assert_eq!(
        values[0].to_string(),
        "(+ (group (:: y number)) (:: w number))"
    );
}

#[test]
fn type_and_export_stay_ordinary_names() {
    let c = chunk("type = 1 export = type(x) local t = type\ntype(t)");
    assert_eq!(c.body.stmts.len(), 4);
}

#[test]
fn mode_directives() {
    let mode = |src: &str| parse_chunk(src).unwrap().mode.map(|(m, _)| m);
    assert_eq!(mode("--!strict\nlocal x = 1"), Some(TypeMode::Strict));
    assert_eq!(
        mode("-- header\n\n  --!nonstrict\nx = 1"),
        Some(TypeMode::NonStrict)
    );
    assert_eq!(mode("--!native\n--!nocheck\n"), Some(TypeMode::NoCheck));
    assert_eq!(mode("x = 1\n--!strict"), None, "only before the first code");
    assert_eq!(mode("--[[ block ]]\n--!strict"), None);
    assert_eq!(mode("local x = 1"), None);
    let c = parse_chunk("\n--!strict").unwrap();
    assert_eq!(c.mode.unwrap().1.text("\n--!strict"), "--!strict");
}
