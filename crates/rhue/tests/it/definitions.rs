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

// ─ Class Names As Strings ────────────────────────────────────────────────────

const TREE: &str = "declare class Node
    Name: string
    @class(className) function FindFirstChildOfClass(self, className: string): Node?
    @class(className) function IsA(self, className: string): boolean
end
declare class Box extends Node
    Size: number
end
declare class Part extends Node end
declare world: Node
";

fn tree() -> Engine {
    let mut e = Engine::new();
    e.load_definitions("tree.d.rhue", TREE).unwrap();
    e
}

#[test]
fn a_class_literal_gives_that_class() {
    let mut e = tree();
    strict(
        &mut e,
        "local b: Box? = world:FindFirstChildOfClass('Box')
         if b then print(b.Size) end
         local n: Node? = world:FindFirstChildOfClass('Node')",
    )
    .unwrap();
    let err = strict(
        &mut e,
        "local p: Part? = world:FindFirstChildOfClass('Box')",
    )
    .unwrap_err();
    assert!(err.contains("could not be converted"), "{err}");
    // a string that isn't a literal gets the declared type
    let err = strict(
        &mut e,
        "local s = 'Box' local name: string = s local b: Box? = world:FindFirstChildOfClass(name)",
    )
    .unwrap_err();
    assert!(err.contains("could not be converted"), "{err}");
}

#[test]
fn is_a_narrows_in_an_if() {
    let mut e = tree();
    strict(
        &mut e,
        "local function f(n: Node)
             if n:IsA('Box') then print(n.Size) end
             if not n:IsA('Box') then print(n.Name) else print(n.Size) end
             if n:IsA('Node') then print(n.Name) end
         end",
    )
    .unwrap();
    let err = strict(
        &mut e,
        "local function f(n: Node) if not n:IsA('Box') then print(n.Size) end end",
    )
    .unwrap_err();
    assert_eq!(err, "Key 'Size' not found in class 'Node'");
    // a Box is already a Node, and isn't a Part
    let err = strict(
        &mut e,
        "local function f(b: Box) if b:IsA('Part') then print(b.Name) end end",
    )
    .unwrap_err();
    assert!(err.contains("never"), "{err}");
}

#[test]
fn a_literal_that_isnt_a_class_is_a_strict_error() {
    let mut e = tree();
    assert_eq!(
        strict(&mut e, "local n = world:FindFirstChildOfClass('Nope')").unwrap_err(),
        "'Nope' is not a class"
    );
    e.check(
        "loose",
        "--!nonstrict\nlocal n = world:FindFirstChildOfClass('Nope')",
    )
    .unwrap();
}

#[test]
fn class_params_round_trip_and_get_checked() {
    let out = tree().definitions();
    assert!(
        out.contains("    @class(arg1) function IsA(self, arg1: string): boolean\n"),
        "{out}"
    );
    let mut again = Engine::new();
    again.load_definitions("again.d.rhue", &out).unwrap();
    assert_eq!(again.definitions(), out);

    // an engine with the method but not the `@class`
    let mut e = Engine::new();
    e.load_definitions(
        "plain.d.rhue",
        &TREE.replace("@class(className) function IsA", "function IsA"),
    )
    .unwrap();
    assert_eq!(
        e.check_definitions("tree.d.rhue", TREE).unwrap(),
        ["Node:IsA takes a class name"]
    );
}

#[test]
fn class_param_mistakes() {
    assert_eq!(
        load_err("declare class A @class(nope) function X(self, a: string): () end"),
        (
            ErrorKind::Type,
            "'nope' isn't one of the function's parameters".into()
        )
    );
    assert_eq!(
        load_err("declare class A @class(a) function X(self, a: number): () end"),
        (
            ErrorKind::Type,
            "`@class(a)` needs 'a' to be a string".into()
        )
    );
    assert_eq!(
        load_err("declare class A @native function X(self): () end"),
        (
            ErrorKind::Syntax,
            "expected `class` after `@`, found identifier".into()
        )
    );
    assert_eq!(
        load_err("declare class A @class(a) Name: string end"),
        (ErrorKind::Syntax, "`@class` goes before a method".into())
    );
}

#[test]
fn class_params_from_rust() {
    let mut e = Engine::new();
    let node = e
        .define_class::<Handle>("Node")
        .method("IsA", |_: &Handle, _: String| true)
        .method("Count", |_: &Handle, _: f64| 0.0)
        .finish();
    e.define_class::<Handle>("Box")
        .parent(&node)
        .getter("Size", |_: &Handle| 1.0)
        .finish();
    e.declare_class_param("Node", "IsA", 0).unwrap();
    strict(
        &mut e,
        "local function f(n: Node) if n:IsA('Box') then print(n.Size) end end",
    )
    .unwrap();
    // a class under it gets it too
    strict(
        &mut e,
        "local function f(b: Box) if b:IsA('Box') then print(b.Size) end end",
    )
    .unwrap();
    assert!(e.definitions().contains("@class(arg1) function IsA"));
    let msg = |r: Result<(), rhue::Error>| r.unwrap_err().diagnostics[0].message.clone();
    assert_eq!(
        msg(e.declare_class_param("Node", "Nope", 0)),
        "class 'Node' has no method 'Nope' of its own"
    );
    assert_eq!(
        msg(e.declare_class_param("Box", "IsA", 0)),
        "class 'Box' has no method 'IsA' of its own"
    );
    assert_eq!(
        msg(e.declare_class_param("Node", "Count", 0)),
        "argument 1 of 'Node:Count' isn't a string"
    );
}

const MAKE: &str = "declare class Instance
    Name: string
end
declare class Part extends Instance end
declare Instance: { count: number }
@class(className) declare function Instance.new(className: string): Instance
@class(c) declare function make(c: string): Instance?
";

#[test]
fn global_functions_take_class_names_too() {
    let mut e = Engine::new();
    e.load_definitions("make.d.rhue", MAKE).unwrap();
    strict(
        &mut e,
        "local p: Part = Instance.new('Part')
         local q: Part? = make('Part')
         local n: number = Instance.count",
    )
    .unwrap();
    assert_eq!(
        strict(&mut e, "local p = Instance.new('Nope')").unwrap_err(),
        "'Nope' is not a class"
    );
    // a local of the same name is something else
    strict(
        &mut e,
        "local Instance = { new = function(s: string): number return 1 end }
         local n: number = Instance.new('Part')",
    )
    .unwrap();

    let out = e.definitions();
    assert!(out.contains("declare Instance: { count: number }\n@class(arg1) declare function Instance.new(arg1: string): Instance\n"), "{out}");
    assert!(
        out.contains("@class(arg1) declare function make(arg1: string): Instance?\n"),
        "{out}"
    );
    let mut again = Engine::new();
    again.load_definitions("again.d.rhue", &out).unwrap();
    assert_eq!(again.definitions(), out);

    // from Rust
    let mut host = Engine::new();
    host.load_definitions(
        "plain.d.rhue",
        &MAKE
            .replace("@class(className) ", "")
            .replace("@class(c) ", ""),
    )
    .unwrap();
    assert_eq!(
        host.check_definitions("make.d.rhue", MAKE).unwrap(),
        ["Instance.new takes a class name", "make takes a class name"]
    );
    host.declare_global_class_param("Instance.new", 0).unwrap();
    host.declare_global_class_param("make", 0).unwrap();
    assert!(
        host.check_definitions("make.d.rhue", MAKE)
            .unwrap()
            .is_empty()
    );
    strict(&mut host, "local p: Part = Instance.new('Part')").unwrap();
    let msg = |r: Result<(), rhue::Error>| r.unwrap_err().diagnostics[0].message.clone();
    assert_eq!(
        msg(host.declare_global_class_param("Instance.old", 0)),
        "there's no global function 'Instance.old'"
    );
    assert_eq!(
        msg(host.declare_global_class_param("make", 1)),
        "argument 2 of 'make' isn't a string"
    );
}

#[test]
fn global_class_param_mistakes() {
    assert_eq!(
        load_err("@class(a) declare x: string"),
        (
            ErrorKind::Syntax,
            "`@class` goes before a method or a `declare function`".into()
        )
    );
    assert_eq!(
        load_err("@class(a) declare function f(a: number): ()"),
        (
            ErrorKind::Type,
            "`@class(a)` needs 'a' to be a string".into()
        )
    );
    assert_eq!(
        load_err("declare x: number\ndeclare function x.f(): ()"),
        (
            ErrorKind::Type,
            "'x' isn't a table, so it can't have 'f'".into()
        )
    );
}
