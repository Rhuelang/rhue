//! Definitions files (3.0): a host's classes and globals written out as a
//! `.d.rhue`, and read back by an engine that doesn't have the host.

use rhue::{Engine, ErrorKind};

struct Handle;

/// A host with a bit of everything a definitions file can say.
fn host() -> Engine {
    let mut e = Engine::new();
    let node = e
        .define_class::<Handle>("Node")
        .property(
            "Name",
            |_: &Handle| String::new(),
            |_: &Handle, _: String| {},
        )
        .getter("ClassName", |_: &Handle| String::new())
        .method("Destroy", |_: &Handle| {})
        .method("GetAttribute", |_: &Handle, _: String| 0.0)
        .finish();
    e.declare_index("Node", "Node").unwrap();
    e.define_class::<Handle>("Shape")
        .parent(&node)
        .getter("Size", |_: &Handle| 1.0)
        .finish();
    e.register("wait", |s: f64| s);
    e.declare("world", "Node").unwrap();
    e.declare("settings", "{ debug: boolean, read version: string }")
        .unwrap();
    e.declare("spawn", "((() -> ()) -> ()) & ((Node) -> Node)")
        .unwrap();
    e
}

const HOST: &str = "declare class Node
    read ClassName: string
    Name: string
    function Destroy(self): ()
    function GetAttribute(self, arg1: string): number
    [string]: Node
end

declare class Shape extends Node
    read Size: number
end

declare settings: { debug: boolean, read version: string }
declare function spawn(arg1: () -> ()): ()
declare function spawn(arg1: Node): Node
declare function wait(arg1: number): number
declare world: Node
";

fn strict(e: &mut Engine, src: &str) -> Result<(), String> {
    e.check("test", &format!("--!strict\n{src}"))
        .map_err(|err| {
            err.diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
}

// ─ Writing ───────────────────────────────────────────────────────────────────

#[test]
fn an_engine_writes_what_it_declared() {
    assert_eq!(host().definitions(), HOST);
}

#[test]
fn an_engine_with_nothing_declared_writes_nothing() {
    assert_eq!(Engine::new().definitions(), "");
    assert_eq!(Engine::bare().definitions(), "");
}

#[test]
fn what_gets_written_reads_back_the_same() {
    let mut e = Engine::new();
    e.load_definitions("host.d.rhue", HOST).unwrap();
    assert_eq!(e.definitions(), HOST);
}

// ─ Reading ───────────────────────────────────────────────────────────────────

/// The class note's example, events typed with one generic alias.
const NOTE: &str = "
-- what the host has
type Signal<T...> = {
    Connect: (any, (T...) -> ()) -> Connection,
    Wait: (any) -> T...,
}

declare class Connection
    function Disconnect(self): ()
end

declare class Node
    Name: string
    Parent: Node?
    function Destroy(self): ()
end

declare class Shape extends Node
    read Touched: Signal<Shape>
end

declare world: Node
declare function spawn(f: () -> ()): ()
";

fn note() -> Engine {
    let mut e = Engine::new();
    e.load_definitions("host.d.rhue", NOTE).unwrap();
    e
}

#[test]
fn scripts_get_checked_against_a_file() {
    let mut e = note();
    strict(
        &mut e,
        "local n: Node = world
         n.Name = 'root'
         local function touched(s: Shape)
             local c = s.Touched:Connect(function(other: Shape) print(other.Name) end)
             c:Disconnect()
             local hit: Shape = s.Touched:Wait()
             s:Destroy()
         end
         spawn(function() end)",
    )
    .unwrap();
    assert_eq!(
        strict(&mut e, "world.Nope = 1").unwrap_err(),
        "Key 'Nope' not found in class 'Node'"
    );
    let err = strict(
        &mut e,
        "local function f(s: Shape) s.Touched = s.Touched end",
    )
    .unwrap_err();
    assert!(err.contains("Touched"), "{err}");
}

#[test]
fn scripts_can_name_the_aliases() {
    let mut e = note();
    strict(
        &mut e,
        "local function on(sig: Signal<Shape>) sig:Connect(function(s) print(s.Name) end) end",
    )
    .unwrap();
    // a script's own alias of the same name wins
    strict(&mut e, "type Signal = number local n: Signal = 1").unwrap();
}

#[test]
fn a_file_only_has_types() {
    let mut e = note();
    let err = e.eval("return world.Name").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Runtime);
    assert_eq!(e.global_type("world").as_deref(), Some("Node"));
}

#[test]
fn a_function_declared_twice_is_overloaded() {
    let mut e = Engine::new();
    e.load_definitions(
        "host.d.rhue",
        "declare function find(name: string): string?
         declare function find(id: number): number?",
    )
    .unwrap();
    strict(
        &mut e,
        "local a: string? = find('x') local b: number? = find(1)",
    )
    .unwrap();
}

#[test]
fn classes_can_come_in_any_order() {
    let mut e = Engine::new();
    e.load_definitions(
        "host.d.rhue",
        "declare class Box extends Shape end
         declare class Shape extends Node end
         declare class Node Name: string end",
    )
    .unwrap();
    strict(
        &mut e,
        "local function f(b: Box) local n: Node = b print(b.Name) end",
    )
    .unwrap();
}

fn load_err(src: &str) -> (ErrorKind, String) {
    let mut e = Engine::new();
    let before = e.definitions();
    let err = e.load_definitions("bad.d.rhue", src).unwrap_err();
    assert_eq!(e.definitions(), before, "nothing changes on an error");
    let messages: Vec<String> = err.diagnostics.iter().map(|d| d.message.clone()).collect();
    (err.kind, messages.join("\n"))
}

#[test]
fn mistakes_get_reported_and_nothing_changes() {
    assert_eq!(
        load_err("declare class A extends Nope end"),
        (ErrorKind::Type, "Unknown class 'Nope'".into())
    );
    assert_eq!(
        load_err("declare class A extends B end declare class B extends A end"),
        (
            ErrorKind::Type,
            "'B' can't extend 'A', which is under it".into()
        )
    );
    assert_eq!(
        load_err("declare class A Name: Nope end"),
        (ErrorKind::Type, "Unknown type 'Nope'".into())
    );
    assert_eq!(
        load_err("declare class A write Name: string end"),
        (
            ErrorKind::Type,
            "a class property can be `read`, not `write`".into()
        )
    );
    assert_eq!(
        load_err("declare class A [number]: string end"),
        (
            ErrorKind::Type,
            "a class can only have a `[string]: T` indexer".into()
        )
    );
    assert_eq!(
        load_err("type T = number type T = string"),
        (ErrorKind::Type, "Type 'T' is already defined".into())
    );
    assert_eq!(
        load_err("declare class A function Go(): () end"),
        (
            ErrorKind::Syntax,
            "a method's first parameter is `self`".into()
        )
    );
    assert_eq!(
        load_err("declare class A function Go(self: A): () end"),
        (
            ErrorKind::Syntax,
            "`self` is the class, it doesn't get a type".into()
        )
    );
    // every bad declaration, not just the first
    let (kind, messages) = load_err("local x = 1\ndeclare y: number\nprint(x)\ndeclare z number");
    assert_eq!(kind, ErrorKind::Syntax);
    assert_eq!(
        messages,
        "expected `declare` or `type`, found `local`\nexpected `declare` or `type`, found identifier\nexpected `:` after the declared name, found identifier"
    );
}

// ─ Checking A File Against The Host ──────────────────────────────────────────

#[test]
fn check_definitions_lists_what_the_host_is_missing() {
    let mut e = host();
    assert_eq!(
        e.check_definitions("host.d.rhue", HOST).unwrap(),
        Vec::<String>::new()
    );
    let stale = "declare class Node
        read ClassName: string
        Name: string
        read Size: number
        Parent: Node?
        function Destroy(self): ()
        function Clone(self): Node
    end
    declare class Shape extends Node
        Size: number
    end
    declare class Part extends Shape end
    declare class Model extends Node
        [string]: Node
    end
    declare world: Node
    declare game: Node";
    assert_eq!(
        e.check_definitions("stale.d.rhue", stale).unwrap(),
        [
            "Model",
            "Node.Size",
            "Node.Parent",
            "Node:Clone",
            "Part",
            "Shape.Size can be set",
            "game"
        ]
    );
}

#[test]
fn a_loaded_file_writes_back_with_its_aliases() {
    let out = note().definitions();
    assert!(out.starts_with("type Signal<T...> = {"), "{out}");
    assert!(out.contains("    read Touched: Signal<Shape>\n"), "{out}");
    assert!(
        out.contains("declare function spawn(arg1: () -> ()): ()\n"),
        "{out}"
    );
    let mut again = Engine::new();
    again.load_definitions("again.d.rhue", &out).unwrap();
    assert_eq!(again.definitions(), out);
}
