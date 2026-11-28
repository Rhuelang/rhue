//! Operators on host classes (3.0): `a + b`, `-a`, `==`, `<`, `#`, `..`,
//! calls, iteration and `tostring`, with overloads picked by operand types.

use std::cell::Cell;
use std::rc::Rc;

use rhue::{AnyUserData, Context, Engine, Function, Operator, UserData, Value};

#[derive(UserData, Clone, Debug, PartialEq)]
struct Vec2 {
    x: f64,
    y: f64,
}

fn v(x: f64, y: f64) -> Vec2 {
    Vec2 { x, y }
}

#[derive(UserData, Clone, Debug)]
struct Bag {
    #[rhue(skip)]
    items: Vec<f64>,
}

fn engine() -> Engine {
    let mut e = Engine::new();
    e.register_type::<Vec2>()
        .function("new", |x: f64, y: f64| v(x, y))
        .operator(Operator::Add, |a: Vec2, b: Vec2| v(a.x + b.x, a.y + b.y))
        .operator(Operator::Sub, |a: Vec2, b: Vec2| v(a.x - b.x, a.y - b.y))
        .operator(Operator::Mul, |a: Vec2, b: Vec2| v(a.x * b.x, a.y * b.y))
        .operator(Operator::Mul, |a: Vec2, n: f64| v(a.x * n, a.y * n))
        .operator(Operator::Mul, |n: f64, a: Vec2| v(a.x * n, a.y * n))
        .operator(Operator::Unm, |a: Vec2| v(-a.x, -a.y))
        .operator(Operator::Eq, |a: Vec2, b: Vec2| a == b)
        .operator(Operator::Lt, |a: Vec2, b: Vec2| {
            a.x * a.x + a.y * a.y < b.x * b.x + b.y * b.y
        })
        .operator(Operator::Le, |a: Vec2, b: Vec2| {
            a.x * a.x + a.y * a.y <= b.x * b.x + b.y * b.y
        })
        .operator(Operator::Len, |a: Vec2| (a.x * a.x + a.y * a.y).sqrt())
        .operator(Operator::ToString, |a: Vec2| format!("({}, {})", a.x, a.y))
        .operator(Operator::Concat, |a: Vec2, s: String| {
            format!("({}, {}){s}", a.x, a.y)
        })
        .operator(
            Operator::Call,
            |a: Vec2, i: f64| if i == 1.0 { a.x } else { a.y },
        );
    e.register_type::<Bag>()
        .function("of", |a: f64, b: f64, c: f64| Bag {
            items: vec![a, b, c],
        })
        .operator(Operator::Iter, |cx: &mut Context, b: Bag| -> Function {
            let at = Rc::new(Cell::new(0));
            cx.create_function("bag iterator", move || {
                let i = at.get();
                at.set(i + 1);
                match b.items.get(i) {
                    Some(&x) => (Some(i as f64 + 1.0), Some(x)),
                    None => (None, None),
                }
            })
        });
    e
}

fn run(e: &mut Engine, src: &str) -> String {
    let vals: Vec<Value> = e
        .eval_as::<rhue::Variadic<Value>>(src)
        .unwrap_or_else(|err| panic!("{src}\n{}", err.render(false)))
        .0;
    vals.iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn error(e: &mut Engine, src: &str) -> String {
    e.eval(src).unwrap_err().diagnostic().message.clone()
}

#[test]
fn arithmetic_on_host_objects() {
    let mut e = engine();
    e.eval("a = Vec2.new(1, 2) b = Vec2.new(3, 4)").unwrap();
    assert_eq!(
        run(&mut e, "local c = a + b return c.x, c.y, (b - a).x, (-a).y"),
        "4 | 6 | 2 | -2"
    );
    // overloads by the operands' types, and the number on the left works
    assert_eq!(
        run(&mut e, "return (a * b).y, (a * 3).y, (2 * a).x"),
        "8 | 6 | 2"
    );
    assert_eq!(
        error(&mut e, "return a * true"),
        "attempt to perform arithmetic (mul) on Vec2 and boolean"
    );
    assert_eq!(
        error(&mut e, "return a / b"),
        "attempt to perform arithmetic (div) on userdata and userdata"
    );
}

#[test]
fn comparing_and_equality() {
    let mut e = engine();
    e.eval("a = Vec2.new(1, 2) b = Vec2.new(3, 4) c = Vec2.new(1, 2)")
        .unwrap();
    assert_eq!(
        run(
            &mut e,
            "return a == c, a ~= c, a == b, a == a, a < b, b <= a, b > a"
        ),
        "true | false | false | true | true | false | true"
    );
    // table keys stay on identity, like Luau
    assert_eq!(
        run(&mut e, "local t = {} t[a] = 1 return t[c], t[a]"),
        "nil | 1"
    );
    // nothing but another object gets to __eq
    assert_eq!(run(&mut e, "return a == 1, a == 'x'"), "false | false");
}

#[test]
fn length_concat_call_and_tostring() {
    let mut e = engine();
    e.eval("a = Vec2.new(3, 4)").unwrap();
    assert_eq!(
        run(
            &mut e,
            "return #a, a .. '!', a(1), a(2), tostring(a), `at {a}`"
        ),
        "5 | (3, 4)! | 3 | 4 | (3, 4) | at (3, 4)"
    );
    let printed = Rc::new(std::cell::RefCell::new(String::new()));
    let out = printed.clone();
    e.on_print(move |s| out.borrow_mut().push_str(s));
    e.eval("print(a)").unwrap();
    assert_eq!(printed.borrow().trim(), "(3, 4)");
}

#[test]
fn iterating_a_host_object() {
    let mut e = engine();
    assert_eq!(
        run(
            &mut e,
            "local s, n = 0, 0 for i, x in Bag.of(1, 2, 3) do s += x n = i end return s, n"
        ),
        "6 | 3"
    );
}

#[test]
fn scripts_cant_get_at_the_operators() {
    let mut e = engine();
    e.eval("a = Vec2.new(1, 2)").unwrap();
    assert_eq!(
        run(&mut e, "return getmetatable(a)"),
        "The metatable is locked"
    );
    assert!(error(&mut e, "setmetatable(a, {})").contains("table expected"));
}

#[test]
fn operators_survive_collection() {
    let mut e = engine();
    e.gc();
    assert_eq!(
        run(
            &mut e,
            "collectgarbage() return (Vec2.new(1, 2) + Vec2.new(1, 1)).y"
        ),
        "3"
    );
}

// ─ Handle classes ────────────────────────────────────────────────────────────

struct Part {
    id: u64,
}

#[test]
fn handle_classes_inherit_operators_and_keep_identity() {
    let mut e = Engine::new();
    let eq_calls = Rc::new(Cell::new(0));
    let calls = eq_calls.clone();
    let node = e
        .define_class::<Part>("Node")
        .operator(Operator::Eq, move |a: AnyUserData, b: AnyUserData| {
            calls.set(calls.get() + 1);
            a.handle::<Part>().unwrap().id % 10 == b.handle::<Part>().unwrap().id % 10
        })
        .operator(Operator::ToString, |a: AnyUserData| {
            format!("Node {}", a.handle::<Part>().unwrap().id)
        })
        .finish();
    let part = e
        .define_class::<Part>("Part")
        .parent(&node)
        .operator(Operator::ToString, |a: AnyUserData| {
            format!("Part {}", a.handle::<Part>().unwrap().id)
        })
        .finish();
    let mut cx = e.context();
    let n1 = cx.object(&node, 1, || Part { id: 1 });
    let p1 = cx.object(&part, 11, || Part { id: 11 });
    let p1_again = cx.object(&part, 11, || Part { id: 11 });
    let p2 = cx.object(&part, 2, || Part { id: 2 });
    e.set_global("n1", n1);
    e.set_global("p1", p1);
    e.set_global("p1_again", p1_again);
    e.set_global("p2", p2);
    // the same object is == without asking __eq
    assert_eq!(run(&mut e, "return p1 == p1_again"), "true");
    assert_eq!(eq_calls.get(), 0);
    // a child has the parent's __eq, and its own __tostring
    assert_eq!(
        run(
            &mut e,
            "return n1 == p1, p1 == p2, tostring(n1), tostring(p2)"
        ),
        "true | false | Node 1 | Part 2"
    );
    assert_eq!(eq_calls.get(), 2);
}

// ─ Checker ───────────────────────────────────────────────────────────────────

fn check(e: &mut Engine, src: &str) -> Result<(), String> {
    e.check("t.rhue", &format!("--!strict\n{src}"))
        .map_err(|err| err.render(false))
}

#[test]
fn operators_in_the_checker() {
    let mut e = engine();
    check(
        &mut e,
        "local a, b = Vec2.new(1, 2), Vec2.new(3, 4)
local c: Vec2 = a + b
local d: Vec2 = a * 2
local f: Vec2 = 2 * a
local g: Vec2 = a * b
local h: Vec2 = -a
local n: number = #a
local s: string = a .. '!'
local lt: boolean = a < b
local x: number = a(1)
for i, x in Bag.of(1, 2, 3) do end",
    )
    .unwrap();
    let err = check(&mut e, "local a = Vec2.new(1, 2) return a * true").unwrap_err();
    assert!(
        err.contains("No overload of '__mul' accepts operands of type (Vec2, true)"),
        "{err}"
    );
    let err = check(&mut e, "local a = Vec2.new(1, 2) local s: string = a + a").unwrap_err();
    assert!(err.contains("Vec2"), "{err}");
    assert!(check(&mut e, "local a = Vec2.new(1, 2) return a / a").is_err());
}

#[test]
fn operators_in_definitions_files() {
    let mut e = engine();
    let defs = e.definitions();
    assert!(
        defs.contains("    function __add(self, arg1: Vec2): Vec2\n"),
        "{defs}"
    );
    assert!(
        defs.contains("    function __mul(arg1: number, arg2: Vec2): Vec2\n"),
        "{defs}"
    );
    assert_eq!(
        e.check_definitions("host.d.rhue", &defs).unwrap(),
        Vec::<String>::new()
    );
    // a fresh engine with just the file checks the same scripts
    let mut fresh = Engine::new();
    fresh.load_definitions("host.d.rhue", &defs).unwrap();
    check(
        &mut fresh,
        "local a = Vec2.new(1, 2)\nlocal f: Vec2 = 2 * a + a\nlocal n: number = #a",
    )
    .unwrap();
    assert!(check(&mut fresh, "local a = Vec2.new(1, 2) return a * true").is_err());
    // and a plain engine is missing the operators
    let mut plain = Engine::new();
    plain.register_type::<Vec2>();
    let missing = plain.check_definitions("host.d.rhue", &defs).unwrap();
    assert!(missing.contains(&"Vec2 __add".to_string()), "{missing:?}");
}
