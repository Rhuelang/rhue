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
