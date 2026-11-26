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
