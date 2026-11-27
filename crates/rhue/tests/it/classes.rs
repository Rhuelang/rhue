//! Handle classes (3.0): one Rust type behind a whole class tree, a `Box` that's
//! a `Shape` that's a `Node`, with parents in the VM and in the checker.

use std::cell::{Cell, RefCell};

use rhue::{Class, Context, Engine, Function, ScriptError, Value};

/// The host's object handle. It does its own interior mutability, like a
/// real host's would.
struct Handle {
    name: RefCell<String>,
    size: Cell<f64>,
}

impl Handle {
    fn new(name: &str) -> Handle {
        Handle {
            name: RefCell::new(name.to_string()),
            size: Cell::new(1.0),
        }
    }
}

struct Tree {
    node: Class<Handle>,
    shape: Class<Handle>,
    boxed: Class<Handle>,
}

fn tree(e: &mut Engine) -> Tree {
    let node = e
        .define_class::<Handle>("Node")
        .property(
            "Name",
            |h: &Handle| h.name.borrow().clone(),
            |h: &Handle, v: String| *h.name.borrow_mut() = v,
        )
        .method("Describe", |h: &Handle| format!("node {}", h.name.borrow()))
        .finish();
    let shape = e
        .define_class::<Handle>("Shape")
        .parent(&node)
        .getter("Size", |h: &Handle| h.size.get())
        .method("Grow", |h: &Handle, by: f64| h.size.set(h.size.get() + by))
        .finish();
    let boxed = e
        .define_class::<Handle>("Box")
        .parent(&shape)
        .method("Describe", |h: &Handle| format!("box {}", h.name.borrow()))
        .finish();
    Tree { node, shape, boxed }
}

fn s(text: &str) -> Value {
    Value::from(text)
}

// ─ At Runtime ────────────────────────────────────────────────────────────────

#[test]
fn a_child_has_its_parents_members() {
    let mut e = Engine::new();
    let t = tree(&mut e);
    let b = e.context().create_object(&t.boxed, Handle::new("crate"));
    e.set_global("b", b);
    let got = e
        .eval("b:Grow(2) b.Name = 'lid' return b.Size, b.Name, b:Describe()")
        .unwrap();
    // Describe is Box's own, the rest come from Shape and Node
    assert_eq!(got, [Value::Number(3.0), s("lid"), s("box lid")]);
}

#[test]
fn typeof_is_the_root_and_tostring_the_class() {
    let mut e = Engine::new();
    let t = tree(&mut e);
    let b = e.context().create_object(&t.boxed, Handle::new("crate"));
    let n = e.context().create_object(&t.node, Handle::new("n"));
    e.set_global("b", b);
    e.set_global("n", n);
    let got = e
        .eval("return typeof(b), type(b), typeof(n), tostring(b):sub(1, 5)")
        .unwrap();
    assert_eq!(got, [s("Node"), s("userdata"), s("Node"), s("Box: ")]);
}

#[test]
fn members_added_to_a_parent_later_reach_its_children() {
    let mut e = Engine::new();
    let t = tree(&mut e);
    let b = e.context().create_object(&t.boxed, Handle::new("crate"));
    e.set_global("b", b);
    // added to Node after Shape and Box were made, and Describe again, which
    // Box has its own of
    e.extend_class(&t.node)
        .method("Kind", |_: &Handle| "node")
        .method("Describe", |_: &Handle| "changed");
    let got = e.eval("return b:Kind(), b:Describe()").unwrap();
    assert_eq!(got, [s("node"), s("box crate")]);
    // and the checker sees it through the chain too
    e.declare("bb", "Box").unwrap();
    e.check(
        "k.rhue",
        "--!strict
local k: string = bb:Kind()",
    )
    .unwrap();
}

#[test]
fn errors_name_the_class() {
    let mut e = Engine::new();
    let t = tree(&mut e);
    let b = e.context().create_object(&t.boxed, Handle::new("crate"));
    e.set_global("b", b);
    let err = e.eval("return b.Nope").unwrap_err().to_string();
    assert!(err.contains("'Nope' is not a valid member of Box"), "{err}");
    let err = e.eval("b.Size = 4").unwrap_err().to_string();
    assert!(err.contains("'Size' is a read-only member of Box"), "{err}");
    // a method called on something that isn't one of the tree's objects
    let err = e.eval("local g = b.Grow g({}, 1)").unwrap_err().to_string();
    assert!(
        err.contains("invalid argument #1 to 'Grow' (Shape expected, got table)"),
        "{err}"
    );
}

#[test]
fn members_can_call_back_with_the_same_object() {
    let mut e = Engine::new();
    let node = e
        .define_class::<Handle>("Node")
        .getter("Size", |h: &Handle| h.size.get())
        .method(
            "Fire",
            |cx: &mut Context, h: &Handle, f: Function| -> Result<f64, ScriptError> {
                h.size.set(h.size.get() + 1.0);
                cx.call::<f64>(&f, ())
            },
        )
        .finish();
    let n = e.context().create_object(&node, Handle::new("n"));
    e.set_global("n", n);
    // no RefCell, so a handler touching the object that fired it just works
    let got = e
        .eval("return n:Fire(function() return n.Size + n:Fire(function() return n.Size end) end)")
        .unwrap();
    assert_eq!(got, [Value::Number(5.0)]);
}

#[test]
fn the_host_gets_its_handle_back() {
    let mut e = Engine::new();
    let t = tree(&mut e);
    let b = e.context().create_object(&t.shape, Handle::new("s"));
    let Value::UserData(u) = &b else {
        panic!("{b:?}")
    };
    let h = u.handle::<Handle>().expect("a Handle");
    h.size.set(9.0);
    e.set_global("b", b.clone());
    assert_eq!(e.eval("return b.Size").unwrap(), [Value::Number(9.0)]);
    assert!(u.handle::<String>().is_none());
    assert_eq!(t.shape.name(), "Shape");
}

#[test]
#[should_panic(expected = "a class from one Engine was used in another")]
fn a_class_belongs_to_its_engine() {
    let mut a = Engine::new();
    let mut b = Engine::new();
    let node = a.define_class::<Handle>("Node").finish();
    let _ = b.context().create_object(&node, Handle::new("x"));
}

#[test]
fn accessors_can_fail_and_call_back() {
    let mut e = Engine::new();
    let node = e
        .define_class::<Handle>("Node")
        .getter_with("Name", |_: &mut Context, h: &Handle| {
            let name = h.name.borrow();
            if name.is_empty() {
                return Err(ScriptError::new("the object was destroyed"));
            }
            Ok(name.clone())
        })
        .getter("Size", |h: &Handle| h.size.get())
        .setter_with("Size", |cx: &mut Context, h: &Handle, v: f64| {
            if v < 0.0 {
                return Err(ScriptError::new("Size can't be negative"));
            }
            h.size.set(v);
            // a changed signal, more or less: the handler reads the same
            // object, which needs no borrow to be free
            let changed: Function = cx.global("changed")?;
            cx.call::<()>(&changed, ())
        })
        .finish();
    let n = e.context().create_object(&node, Handle::new("n"));
    let gone = e.context().create_object(&node, Handle::new(""));
    e.set_global("n", n);
    e.set_global("gone", gone);
    let got = e
        .eval(
            "local seen
changed = function() seen = n.Size end
n.Size = 4
local ok, err = pcall(function() n.Size = -1 end)
local ok2, err2 = pcall(function() return gone.Name end)
return seen, n.Size, ok, err, ok2, err2",
        )
        .unwrap();
    assert_eq!(
        got[..3],
        [Value::Number(4.0), Value::Number(4.0), Value::Bool(false)]
    );
    let err = got[3].to_string();
    assert!(err.contains("Size can't be negative"), "{err}");
    // raised where the script wrote it, like any native's error
    assert!(err.contains(":4:"), "{err}");
    assert_eq!(got[4], Value::Bool(false));
    assert!(got[5].to_string().contains("the object was destroyed"));
}

#[test]
fn members_can_come_from_data() {
    // what a host reading its own reflection data does: a loop, no closure per
    // Rust type, and the script types as strings
    let props = [("Size", "number", true), ("Id", "number", false)];
    let mut e = Engine::new();
    let mut b = e.define_class::<Handle>("Node");
    for (name, ty, writable) in props {
        let key = name.to_string();
        b.getter_dyn(name, ty, move |_, h: &Handle| {
            Ok(match &*key {
                "Size" => Value::Number(h.size.get()),
                _ => Value::Number(7.0),
            })
        })
        .unwrap();
        if writable {
            b.setter_dyn(name, ty, |_, h: &Handle, v| match v {
                Value::Number(n) => {
                    h.size.set(n);
                    Ok(())
                }
                other => Err(ScriptError::new(format!(
                    "Size wants a number, got {}",
                    other.type_of()
                ))),
            })
            .unwrap();
        }
    }
    b.method_dyn(
        "Rename",
        "(Node, string, boolean?) -> string",
        |_, h: &Handle, args| {
            let mut name = args.first().map(|v| v.to_string()).unwrap_or_default();
            if args.get(1) == Some(&Value::Bool(true)) {
                name = name.to_uppercase();
            }
            *h.name.borrow_mut() = name.clone();
            Ok(vec![Value::from(name.as_str())])
        },
    )
    .unwrap();
    // a type string the checker can't read is the host's error, not a panic
    let err = b
        .getter_dyn("Bad", "number |", |_, _| Ok(Value::Nil))
        .err()
        .unwrap();
    assert_eq!(err.kind, rhue::ErrorKind::Syntax);
    let err = b
        .method_dyn("Bad", "(Node) ->", |_, _, _| Ok(vec![]))
        .err()
        .unwrap();
    assert_eq!(err.kind, rhue::ErrorKind::Syntax);
    let node = b.finish();

    let n = e.context().create_object(&node, Handle::new("n"));
    e.set_global("n", n);
    let got = e
        .eval("n.Size = 3 return n.Size, n.Id, n:Rename('lid', true)")
        .unwrap();
    assert_eq!(got, [Value::Number(3.0), Value::Number(7.0), s("LID")]);
    let err = e.eval("n.Size = 'big'").unwrap_err().to_string();
    assert!(err.contains("Size wants a number, got string"), "{err}");
    let err = e.eval("n.Id = 1").unwrap_err().to_string();
    assert!(err.contains("'Id' is a read-only member of Node"), "{err}");

    // and the checker knows them from the strings
    e.declare("m", "Node").unwrap();
    e.check(
        "ok.rhue",
        "--!strict\nlocal z: number = m.Size + m.Id\nlocal r: string = m:Rename('x')",
    )
    .unwrap();
    let err = e
        .check("bad.rhue", "--!strict\nlocal r: number = m:Rename('x')")
        .unwrap_err();
    assert!(err.to_string().contains("string"), "{err}");
}

/// `tree` with an index hook on Node that finds a child for any name starting
/// with `child`, and counts how often it ran.
fn tree_with_children(e: &mut Engine) -> (Tree, std::rc::Rc<Cell<u32>>) {
    let t = tree(e);
    let calls = std::rc::Rc::new(Cell::new(0));
    let (node, seen) = (t.node.clone(), calls.clone());
    e.extend_class(&t.node).index(move |cx, _h: &Handle, name| {
        seen.set(seen.get() + 1);
        if name == "boom" {
            return Err(ScriptError::new("the host said no"));
        }
        Ok(name
            .starts_with("child")
            .then(|| cx.create_object(&node, Handle::new(name))))
    });
    (t, calls)
}

#[test]
fn an_index_hook_finds_names_that_arent_members() {
    let mut e = Engine::new();
    let (t, calls) = tree_with_children(&mut e);
    let b = e.context().create_object(&t.boxed, Handle::new("crate"));
    e.set_global("b", b);
    // Box gets Node's hook, and members never reach it
    let got = e
        .eval("return b.childA.Name, b.Name, b.Size, b:Describe()")
        .unwrap();
    assert_eq!(
        got,
        [s("childA"), s("crate"), Value::Number(1.0), s("box crate")]
    );
    assert_eq!(calls.get(), 1);
    // Ok(None) reads like there was no hook
    let err = e.eval("return b.Nope").unwrap_err().to_string();
    assert!(err.contains("'Nope' is not a valid member of Box"), "{err}");
    // the hook's own error raises at the read
    let err = e.eval("return b.boom").unwrap_err().to_string();
    assert!(err.contains("the host said no"), "{err}");
    // and writes don't go through it
    let err = e.eval("b.childA = 1").unwrap_err().to_string();
    assert!(
        err.contains("'childA' is not a valid member of Box"),
        "{err}"
    );
    assert_eq!(calls.get(), 3);
}

#[test]
fn a_child_class_can_have_its_own_index_hook() {
    let mut e = Engine::new();
    let (t, calls) = tree_with_children(&mut e);
    e.extend_class(&t.shape)
        .index(|_, h: &Handle, name| Ok(Some(Value::from(format!("{}/{name}", h.name.borrow())))));
    let b = e.context().create_object(&t.boxed, Handle::new("crate"));
    let n = e.context().create_object(&t.node, Handle::new("root"));
    e.set_global("b", b);
    e.set_global("n", n);
    // Box takes the nearest one, Shape's, and Node keeps its own
    let got = e.eval("return b.anything, n.childB.Name").unwrap();
    assert_eq!(got, [s("crate/anything"), s("childB")]);
    assert_eq!(calls.get(), 1);
}

/// An engine with `get(id)` handing out interned objects of `t.boxed`, and a
/// count of how many handles it had to make.
fn interning(e: &mut Engine) -> (Tree, std::rc::Rc<Cell<u32>>) {
    let t = tree(e);
    let made = std::rc::Rc::new(Cell::new(0));
    let (boxed, count) = (t.boxed.clone(), made.clone());
    e.register("get", move |cx: &mut Context, id: u32| {
        let count = count.clone();
        cx.object(&boxed, u64::from(id), move || {
            count.set(count.get() + 1);
            Handle::new(&format!("box{id}"))
        })
    });
    (t, made)
}

#[test]
fn the_same_host_object_is_the_same_value() {
    let mut e = Engine::new();
    let (t, made) = interning(&mut e);
    let got = e
        .eval(
            "local a, b, c = get(1), get(1), get(2)
             local seen = {[a] = 'first'}
             a.Name = 'renamed'
             return a == b, rawequal(a, b), a == c, seen[b], b.Name",
        )
        .unwrap();
    assert_eq!(
        got,
        [
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(false),
            s("first"),
            s("renamed")
        ]
    );
    assert_eq!(made.get(), 2);
    // ids are per tree, so a Node with id 1 is that same Box
    let n = e
        .context()
        .object(&t.node, 1, || unreachable!("it's interned already"));
    e.set_global("n", n);
    let got = e
        .eval("return n == get(1), typeof(n), tostring(n):sub(1, 4)")
        .unwrap();
    assert_eq!(got, [Value::Bool(true), s("Node"), s("Box:")]);
    assert_eq!(made.get(), 2);
}

#[test]
fn an_interned_object_goes_when_scripts_let_go() {
    let mut e = Engine::new();
    let (_t, made) = interning(&mut e);
    e.eval(
        "seen = setmetatable({}, {__mode = 'k'})
         local function touch() seen[get(7)] = true end
         touch()",
    )
    .unwrap();
    e.gc();
    // nothing held it, so the weak key went and get(7) makes a new one
    let got = e.eval("return next(seen), get(7) ~= nil").unwrap();
    assert_eq!(got, [Value::Nil, Value::Bool(true)]);
    assert_eq!(made.get(), 2);
    // while something holds it, it's the same one
    e.eval("held = get(8)").unwrap();
    e.gc();
    assert_eq!(
        e.eval("return held == get(8)").unwrap(),
        [Value::Bool(true)]
    );
    assert_eq!(made.get(), 3);
}

// ─ In the Checker ────────────────────────────────────────────────────────────

#[test]
fn a_child_is_accepted_where_its_parent_is_expected() {
    let mut e = Engine::new();
    let _t = tree(&mut e);
    e.declare("b", "Box").unwrap();
    e.declare("n", "Node").unwrap();
    // members through the chain, and Box where Node or Shape is wanted
    e.check(
        "ok.rhue",
        "--!strict
local function grow(s: Shape) s:Grow(1) end
local function name(x: Node): string return x.Name end
grow(b)
local a: string = name(b)
local z: number = b.Size
local d: string = b:Describe()",
    )
    .unwrap();
    // but not a parent where a child is wanted
    let err = e
        .check(
            "bad.rhue",
            "--!strict\nlocal function grow(s: Shape) end\ngrow(n)",
        )
        .unwrap_err();
    assert!(err.to_string().contains("Node"), "{err}");
    // and the member rules come along too
    let err = e.check("ro.rhue", "--!strict\nb.Size = 2").unwrap_err();
    assert!(err.to_string().contains("read-only"), "{err}");
    let err = e
        .check("no.rhue", "--!strict\nlocal x = b.Nope")
        .unwrap_err();
    assert!(
        err.to_string().contains("not found in class 'Box'"),
        "{err}"
    );
}

#[test]
fn the_checker_knows_what_an_index_hook_returns() {
    let mut e = Engine::new();
    let _ = tree_with_children(&mut e);
    e.declare("b", "Box").unwrap();
    let err = e
        .check("before.rhue", "--!strict\nlocal c = b.childA")
        .unwrap_err();
    assert!(
        err.to_string().contains("not found in class 'Box'"),
        "{err}"
    );
    e.declare_index("Node", "Node").unwrap();
    e.check(
        "ok.rhue",
        "--!strict\nlocal c: Node = b.childA\nlocal n: string = b.childA.childB.Name",
    )
    .unwrap();
    // a Node isn't a Box, and writes still need a property
    let err = e
        .check("bad.rhue", "--!strict\nlocal c: Box = b.childA")
        .unwrap_err();
    assert!(err.to_string().contains("Node"), "{err}");
    let err = e
        .check("write.rhue", "--!strict\nb.childA = b")
        .unwrap_err();
    assert!(
        err.to_string().contains("not found in class 'Box'"),
        "{err}"
    );
    assert_eq!(
        e.declare_index("Node", "Node |").unwrap_err().kind,
        rhue::ErrorKind::Syntax
    );
}
