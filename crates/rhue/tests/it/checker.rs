//! Milestone 6: the type checker, through the engine. Directives and the
//! default mode, `ErrorKind::Type`, signatures declared for registered Rust
//! functions and userdata, globals shared between chunks, and type erasure.

use std::cell::RefCell;
use std::rc::Rc;

use rhue::{
    Context, Engine, ErrorKind, Function, ScriptError, TypeMode, Ud, UserData, Value, Variadic,
};

fn type_errors(e: &mut Engine, src: &str) -> Vec<String> {
    match e.check("t.rhue", src) {
        Ok(()) => Vec::new(),
        Err(err) => {
            assert_eq!(err.kind, ErrorKind::Type, "{err}");
            err.diagnostics.into_iter().map(|d| d.message).collect()
        }
    }
}

// ─ Modes ─────────────────────────────────────────────────────────────────────

#[test]
fn checking_is_opt_in_by_default() {
    let mut e = Engine::new();
    // no directive: runs as before, types are just erased
    assert_eq!(
        e.eval_as::<String>("local x: number = 'one' return x")
            .unwrap(),
        "one"
    );
    let err = e.eval("--!strict\nlocal x: number = 'one'").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Type);
    assert_eq!(
        err.to_string(),
        "Type '\"one\"' could not be converted into 'number'"
    );
    assert!(
        e.eval("--!nonstrict\nlocal x = 1 x = 'fine without annotations'")
            .is_ok()
    );
}

#[test]
fn the_default_mode_can_be_strict() {
    let mut e = Engine::new();
    e.set_default_type_mode(TypeMode::Strict);
    assert_eq!(e.eval("print(nope)").unwrap_err().kind, ErrorKind::Type);
    // expressions are checked too
    assert_eq!(e.eval_expr("1 + 'a'").unwrap_err().kind, ErrorKind::Type);
    // a chunk can opt out
    assert!(e.eval("--!nocheck\nlocal x: number = 'one'").is_ok());
}

#[test]
fn a_chunk_with_type_errors_does_not_run() {
    let mut e = Engine::new();
    let printed = Rc::new(RefCell::new(Vec::new()));
    let p = printed.clone();
    e.on_print(move |line| p.borrow_mut().push(line.to_string()));
    let src = "--!strict\nprint('before')\nlocal a: number = 'x'\nlocal b: string = 1\nprint(c)";
    let err = e.eval_named("bad.rhue", src).unwrap_err();
    assert!(printed.borrow().is_empty());
    let messages: Vec<&str> = err.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "Type '\"x\"' could not be converted into 'number'",
            "Type 'number' could not be converted into 'string'",
            "Unknown global 'c'",
        ]
    );
    // each points at its own line
    assert_eq!(err.diagnostics[2].span().text(src), "c");
    let rendered = err.render(false);
    assert!(rendered.contains("bad.rhue:5"), "{rendered}");
}

#[test]
fn check_is_strict_unless_the_source_says_otherwise() {
    let mut e = Engine::new();
    assert_eq!(
        type_errors(&mut e, "local x = 1 x = 'a'"),
        ["Type '\"a\"' could not be converted into 'number'"]
    );
    assert!(type_errors(&mut e, "--!nonstrict\nlocal x = 1 x = 'a'").is_empty());
    assert!(type_errors(&mut e, "--!nocheck\nlocal x: number = {}").is_empty());
    // checking doesn't define anything
    e.check("t", "function onlyChecked() end").unwrap();
    assert_eq!(e.global("onlyChecked"), Value::Nil);
}

// ─ Declarations from the Host ────────────────────────────────────────────────

#[test]
fn registered_functions_have_signatures() {
    let mut e = Engine::new();
    e.register("add", |a: f64, b: f64| a + b);
    e.register("count", |n: u32| n);
    e.register("greet", |name: Option<String>| {
        format!("hi {}", name.unwrap_or_default())
    });
    e.register("sum", |xs: Variadic<f64>| xs.iter().sum::<f64>());
    e.register("words", |s: String| {
        s.split(' ').map(String::from).collect::<Vec<_>>()
    });
    e.register("scores", std::collections::HashMap::<String, f64>::new);
    e.register("divmod", |a: i64, b: i64| (a / b, a % b));
    e.register("nothing", || ());
    e.register("half", |n: i64| -> Result<i64, String> { Ok(n / 2) });
    e.register(
        "apply",
        |cx: &mut Context, f: Function, x: f64| -> Result<f64, ScriptError> { cx.call(&f, x) },
    );
    e.register("anything", |v: Value| v);
    let sig = |e: &mut Engine, n: &str| e.global_type(n).unwrap();
    assert_eq!(sig(&mut e, "add"), "(number, number) -> number");
    assert_eq!(sig(&mut e, "count"), "(number) -> number");
    assert_eq!(sig(&mut e, "greet"), "(string?) -> string");
    assert_eq!(sig(&mut e, "sum"), "(...number) -> number");
    assert_eq!(sig(&mut e, "words"), "(string) -> {string}");
    assert_eq!(sig(&mut e, "scores"), "() -> { [string]: number }");
    assert_eq!(
        sig(&mut e, "divmod"),
        "(number, number) -> (number, number)"
    );
    assert_eq!(sig(&mut e, "nothing"), "() -> ()");
    assert_eq!(sig(&mut e, "half"), "(number) -> number");
    assert_eq!(
        sig(&mut e, "apply"),
        "((...any) -> ...any, number) -> number"
    );
    assert_eq!(sig(&mut e, "anything"), "(any) -> any");

    assert_eq!(
        type_errors(
            &mut e,
            "local n: number = add(1, 2)
             add('1', 2)
             local w: {number} = words('a b')
             local q, r = divmod(7, 2) local s: string = q
             greet() greet(nil) greet('x') greet(5)"
        ),
        [
            "Type '\"1\"' could not be converted into 'number'",
            "Type '{string}' could not be converted into '{number}'",
            "Type 'number' could not be converted into 'string'",
            "Type 'number' could not be converted into 'string?'",
        ]
    );
}

#[derive(UserData, Clone)]
struct Player {
    name: String,
    hp: f64,
    #[rhue(readonly)]
    id: u32,
    #[rhue(skip)]
    #[allow(dead_code)]
    secret: String,
    friend: Option<String>,
}

fn player(name: &str) -> Player {
    Player {
        name: name.into(),
        hp: 100.0,
        id: 1,
        secret: String::new(),
        friend: None,
    }
}

#[derive(UserData, Clone)]
#[rhue(name = "Vector2")]
struct Vec2 {
    x: f64,
    y: f64,
}

fn game_engine() -> Engine {
    let mut e = Engine::new();
    e.register_type::<Player>()
        .function("new", |name: String| player(&name))
        .method("heal", |p: &mut Player, n: f64| {
            p.hp += n;
            p.hp
        })
        .method("rename", |p: &mut Player, n: String| p.name = n);
    e.register_type::<Vec2>()
        .function("new", |x: f64, y: f64| Vec2 { x, y })
        .method("length", |v: &mut Vec2| v.x.hypot(v.y));
    e.register("distance", |a: Vec2, b: Vec2| (a.x - b.x).hypot(a.y - b.y));
    e.register("leader", |p: Ud<Player>| p);
    e.set_global("hero", player("Aalto"));
    e
}

#[test]
fn userdata_types_are_classes() {
    let mut e = game_engine();
    assert_eq!(e.global_type("hero").unwrap(), "Player");
    assert_eq!(
        e.global_type("distance").unwrap(),
        "(Vector2, Vector2) -> number"
    );
    assert_eq!(e.global_type("leader").unwrap(), "(Player) -> Player");
    assert_eq!(
        e.global_type("Player").unwrap(),
        "{ new: (string) -> Player }"
    );
    assert!(
        type_errors(
            &mut e,
            "local p = Player.new('Zae')
         local h: number = p:heal(5)
         p.hp = h * 2 p.name = 'Zed' p:rename('Z')
         local f: string? = p.friend
         local d: number = distance(Vector2.new(0, 0), Vector2.new(3, 4))
         local l = leader(hero) local n: string = l.name"
        )
        .is_empty()
    );
    assert_eq!(
        type_errors(
            &mut e,
            "hero.hp = 'lots'
             hero.id = 2
             local s = hero.secret
             hero:heal('x')
             hero:fly()
             distance(hero, Vector2.new(0, 0))
             local n: number = hero.friend"
        ),
        [
            "Type '\"lots\"' could not be converted into 'number'",
            "Property 'id' of 'Player' is read-only",
            "Key 'secret' not found in class 'Player'",
            "Type '\"x\"' could not be converted into 'number'",
            "Key 'fly' not found in class 'Player'",
            "Type 'Player' could not be converted into 'Vector2'",
            "Type 'string?' could not be converted into 'number'",
        ]
    );
}

#[test]
fn types_registered_inside_natives_are_declared_too() {
    // a userdata type first used inside a native (no register_type) still gets
    // its fields declared when the native's signature mentions it
    #[derive(UserData, Clone)]
    struct Token {
        text: String,
    }
    let mut e = Engine::new();
    e.register("token", |s: String| Token { text: s });
    assert_eq!(e.global_type("token").unwrap(), "(string) -> Token");
    assert_eq!(
        type_errors(
            &mut e,
            "local t = token('a') local s: string = t.text local n: number = t.text"
        ),
        ["Type 'string' could not be converted into 'number'"]
    );
}

#[test]
fn set_global_and_declare() {
    let mut e = Engine::new();
    e.set_global("version", "1.0");
    e.set_global("limit", 10);
    e.set_global("flags", vec![true, false]);
    e.set_global("anything", Value::Nil);
    assert_eq!(e.global_type("version").unwrap(), "string");
    assert_eq!(e.global_type("flags").unwrap(), "{boolean}");
    assert_eq!(e.global_type("anything").unwrap(), "any");
    e.declare("config", "{ debug: boolean, name: string? }")
        .unwrap();
    assert_eq!(
        type_errors(
            &mut e,
            "local v: number = version local l: number = limit config.debug = 1 local n = config.name .. ''"
        ),
        [
            "Type 'string' could not be converted into 'number'",
            "Type 'number' could not be converted into 'boolean'",
            "Type 'string?' could not be converted into 'string | number'",
        ]
    );
    let err = e.declare("broken", "(number ->").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Syntax);
}

// ─ Chunks See Each Other's Globals ───────────────────────────────────────────

#[test]
fn later_chunks_see_earlier_globals() {
    let mut e = Engine::new();
    e.eval("--!strict\nfunction greet(name: string): string return 'hi ' .. name end\ncount = 0")
        .unwrap();
    assert_eq!(e.global_type("greet").unwrap(), "(string) -> string");
    assert_eq!(
        type_errors(&mut e, "local n: number = greet('x') count = 'many'"),
        [
            "Type 'string' could not be converted into 'number'",
            "Type '\"many\"' could not be converted into 'number'",
        ]
    );
    // unchecked chunks define globals too (as `any`), so strict code can use
    // them
    e.eval("helper = function(x) return x end").unwrap();
    assert!(type_errors(&mut e, "local y = helper(1)").is_empty());
    // a script's global can be redefined (as in a REPL session)
    e.eval("--!strict\nfunction greet(n: number): number return n end")
        .unwrap();
    assert_eq!(e.global_type("greet").unwrap(), "(number) -> number");
    // host declarations can't be redefined by scripts
    assert_eq!(
        type_errors(&mut e, "print = 5"),
        ["Type 'number' could not be converted into '(...any) -> ()'"]
    );
}

#[test]
fn a_chunk_that_fails_to_check_defines_nothing() {
    let mut e = Engine::new();
    assert!(
        e.eval("--!strict\nfunction f(): number return 1 end local x: string = 1")
            .is_err()
    );
    assert_eq!(e.global_type("f"), None);
}

// ─ Types Don't Change What Code Does ─────────────────────────────────────────

#[test]
fn types_are_erased() {
    let mut e = Engine::new();
    let src = "--!strict
        type Point = {x: number, y: number}
        local function add<T>(a: Point, b: Point): Point
            return {x = a.x + b.x, y = a.y + b.y}
        end
        local p: Point = add({x = 1, y = 2}, {x = 3, y = 4})
        local n = (p.x :: number) + p.y
        local function two(): (number, number) return 1, 2 end
        local t = {two() :: number}
        local s: string = (5 :: any)
        return n, #t, type(s)";
    let (n, len, kind): (f64, f64, String) = e.eval_as(src).unwrap();
    assert_eq!(
        (n, len, kind.as_str()),
        (10.0, 1.0, "number"),
        "a cast converts nothing"
    );
    let plain = e
        .eval_as::<(f64, f64, String)>(&src.replacen("--!strict", "--!nocheck", 1))
        .unwrap();
    assert_eq!(plain, (10.0, 1.0, "number".to_string()));
}

#[test]
fn a_typed_program() {
    // something realistic, start to finish
    let mut e = Engine::new();
    let src = r#"--!strict
        type Item = { name: string, price: number, tags: {string}? }
        type Cart = { items: {Item}, coupon: string? }

        local function total(cart: Cart): number
            local sum = 0
            for _, item in cart.items do
                sum += item.price
            end
            if cart.coupon == "HALF" then
                sum /= 2
            end
            return sum
        end

        local function tagged(cart: Cart, tag: string): {string}
            local out: {string} = {}
            for _, item in ipairs(cart.items) do
                local tags = item.tags
                if not tags then
                    continue
                end
                if table.find(tags, tag) then
                    table.insert(out, item.name)
                end
            end
            table.sort(out, function(a, b) return a < b end)
            return out
        end

        local cart: Cart = {
            items = {
                { name = "tea", price = 4, tags = {"drink"} },
                { name = "scone", price = 3 },
                { name = "coffee", price = 5, tags = {"drink", "hot"} },
            },
            coupon = "HALF",
        }
        return total(cart), table.concat(tagged(cart, "drink"), ",")
    "#;
    let (sum, drinks): (f64, String) = e.eval_as(src).unwrap();
    assert_eq!(sum, 6.0);
    assert_eq!(drinks, "coffee,tea");
}
