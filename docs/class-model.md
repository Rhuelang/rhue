# The 3.0 class model

How host classes work in 3.0, so one Rust type can stand behind a whole class tree. This is the plan the step 4 PRs build, agreed before any of it got written. DESIGN.md gets a paragraph per decision as each part lands.

The examples use a made-up host whose objects are all one handle type, `Handle`, with classes `Node`, `Shape` and `Box` (`Box` is a `Shape`, `Shape` is a `Node`).

## What it has to do

A host can put every class behind one Rust type, an object handle driven by its own reflection data. 2.x assumes one Rust type per script class with a fixed member list, so:

| A host needs | 2.x |
| --- | --- |
| `Node`, `Shape` and `Box` as separate classes, a `Box` accepted where a `Node` is expected, `typeof(box)` still `"Node"` | A class is a Rust type's `NAME`, and the checker's class has no parent |
| Names that aren't registered falling back to the host (`world.Box` finds a child by name) | Methods, then getters, then `'x' is not a valid member of T` |
| A callback touching the object whose method fired it | Methods take `&mut T` out of a `RefCell`, so that's `T is already in use` |
| The same object reached twice being `==` and the same table key | Every conversion makes a new userdata |
| Weak tables, so per-object caches don't keep objects alive | No `__mode` |
| `box.Position` cheap enough to run every frame | Two hash lookups every access, the inline cache only knows tables |
| `rhue-lsp` and `rhue --check` knowing the host's classes | Types only come from registered Rust code, at run time |

A small host with a few `#[derive(UserData)]` types mustn't pay for any of it. derive and `TypeBuilder` stay the easy way in.

## Classes

A class is a VM object with a parent, and the host picks an object's class when it hands the object over. The Rust type stops being the class and becomes what a class tree holds.

```rust
struct Class {
    name: Rc<str>,
    parent: Option<ClassId>,
    /// the Rust type every object in this tree holds, checked on downcast
    holds: TypeId,
    /// own and inherited members, flattened. slots never move
    resolved: Vec<(StrRef, Member)>,
    by_name: FxHashMap<StrRef, u32>,
    index: Option<IndexHook>,
    typeof_name: Rc<str>, // the root's name unless the host says otherwise
}

enum Member { Method(Value), Property { get: Option<NativeFn>, set: Option<NativeFn> } }
```

- One Rust type per tree, not per class. Making a child of a class that holds a different type is refused, so a downcast can't go wrong.
- The object carries its class (`UserData.class`). The `TypeId` map stays as derive's shortcut to a root class.
- Lookup is one hash into a flattened list, never a walk up the parents. Inherited members keep their parent's slot and new ones get appended, so a slot never moves. That's what makes the cache a single compare.
- Adding a member to a class later appends it to that class and every class under it. Removing members isn't a thing, same as 2.x.
- `typeof(box)` is the root's name, `"Node"`. `type(box)` stays `"userdata"`, and `tostring` stays `"Box: 0x..."` until step 5 adds `__tostring`.
- Events are read-only properties that return the host's signal object. The VM doesn't need a third kind of member.

derive keeps working: `#[derive(UserData)]` makes a root class holding that type, with no parent, and `TypeBuilder` adds members to it. Handle trees use a builder beside it:

```rust
let node = engine.define_class::<Handle>("Node");
let shape = engine.define_class::<Handle>("Shape").parent(&node);
let boxed = engine
    .define_class::<Handle>("Box")
    .parent(&shape)
    .property("Size", |h: &Handle| h.size(), Some(|h: &Handle, v: Vector3| h.set_size(v)))
    .method("Destroy", |h: &Handle| h.destroy())
    .finish();
```

A host that registers its classes in a loop over reflection data can't write a closure per type, so the builder takes the script type as a string too:

```rust
class.property_dyn("Size", "Vector3", true, getter, setter); // getter and setter take a Context and can fail
class.method_dyn("FindFirstChild", "(Node, string, boolean?) -> Node?", call);
```

An object whose class was never registered gets handed over as the root class.

In the checker, `ClassType` gets a parent, `declare_class(name)` becomes `declare_class(name, parent)`, and `Class(x)` is a subtype of `Class(y)` when `y` is on `x`'s parent chain. Member lookup goes up the chain, nearest first.

## Index hooks

One read hook per class, for names that aren't members. No write hook.

```rust
node.index(|cx: &mut Context<'_>, h: &Handle, name: &str| -> Result<Option<Value>, ScriptError> {
    Ok(h.find_child(name).map(|c| cx.object(&class_of(&c), c.id(), || c)))
});
```

- Order: the class's members, then the nearest hook up the parents, then the error. A hook only sees names that aren't members, so it can't shadow a property.
- `Ok(None)` gives the same `'x' is not a valid member of Box` as now.
- The name comes from the intern table's `Rc<str>`, so nothing gets allocated per lookup.
- Writing to a name that isn't a property still errors. A child is a reference, not something to overwrite by name, and a write hook would send every typo in a property write somewhere quietly.
- The checker learns what a hook returns from `declare_index("Node", "Node")`, so `world.Box` checks as `Node` in strict scripts. Without one, unknown members stay an error.
- Enums don't use the hook: `Enum` and `Enum.Shape` are frozen tables, and the items in them are userdata the host interns, so `typeof` says `EnumItem` and every route to an item gives the same value.

## Borrowing

A class says how its objects are stored.

- Handle classes (`define_class`) hold `Rc<H>` and every member gets `&H`. No `RefCell`, so re-entry just works. The handle does its own locking.
- Value classes (derive, `TypeBuilder`) keep `Rc<RefCell<T>>`. Methods can take `&T` (shared, shipped in 2.3) or `&mut T` (exclusive, and re-entering one is still `T is already in use`).
- Getters and setters can take a `Context` and return `Result`, so a setter can refuse a write or fire a changed signal, and either can return a thread kill.

## Identity and weak tables

The VM interns handle objects by a key the host gives, and the collector clears dead entries the same way it already clears dead interned strings.

- `cx.object(&class, id, || handle)` returns the live userdata for that `u64` id if there is one and makes it otherwise. The key is scoped to the tree's root class. Same id, same `UdRef`, so `==` and table keys just work, no hash hook.
- The map lives on the heap beside the string intern table, and the userdata sweep removes an object's entry when it frees it. Nothing in the map keeps anything alive.
- The VM does nothing special for an object the host considers destroyed. The userdata holds its handle, and the host's members decide what a destroyed object does.
- `__mode` with `"k"`, `"v"` and `"kv"`, as in Luau. Weak keys, not ephemerons. The collector skips marking through weak parts, then clears unreached entries. Only collectable values count as weak, strings and numbers never get cleared. The mode gets read once, at `setmetatable`.

A weak-keyed entry for an object goes when no script holds that object's userdata, even if the host still has the object. Asking for it again makes a new userdata. Roblox has the same quirk.

## Caching

The per-instruction cache tables use gets a userdata arm. For userdata the cell holds a tag bit, a 15-bit class id and a 16-bit slot. The key at `GETFIELD`, `SETFIELD` and `SELF` is a constant of the instruction, and slots never move, so a matching class means the cached slot is right: one compare, no version to keep in step. Debug builds also check the slot's name. Up to 32,767 classes and 65,535 members a class get cached, past that a class just isn't.

## Definitions files

Luau's declaration syntax in a `.d.rhue` file, so the standalone tools know a host's classes.

```lua
type Signal<T...> = {
    Connect: (any, (T...) -> ()) -> Connection,
    Once: (any, (T...) -> ()) -> Connection,
    Wait: (any) -> T...,
}

declare class Node
    Name: string
    Parent: Node?
    function Destroy(self): ()
    @class(className) function FindFirstChildOfClass(self, className: string): Node?
    @class(className) function IsA(self, className: string): boolean
end

declare class Shape extends Node
    read Touched: Signal<Shape>
end

declare world: Node
```

- `Engine::definitions()` writes one from everything the engine has declared, in a stable order so it diffs cleanly, and works on an engine that's never run a script.
- `Engine::load_definitions`, `rhue --check --definitions`, and a `rhue.definitions` setting (or a `.d.rhue` at the workspace root) for `rhue-lsp` read one.
- `Engine::check_definitions` lists what a file declares that the engine doesn't have, for a test in the host's own suite.
- Events are typed with one generic `Signal` alias, since the checker already has generic aliases with type packs.

### Class names as strings

`@class(param)` marks a string parameter as naming a class. A string literal naming a declared class `C` makes a class-returning function return `C` (`FindFirstChildOfClass("Box")` is `Box?`), and makes a boolean one narrow the receiver in an `if` (`if n:IsA("Box") then`). A literal that isn't a class is a strict-mode error, and a string that isn't a literal gets the declared type. From Rust it's `declare_class_param`.

## Order

1. Classes with parents (VM, checker, derive).
2. Shared borrows and accessors that can fail.
3. Index hooks.
4. Interning and weak tables, one PR. These can be built beside 2 and 3.
5. Definitions files.
6. Class names as strings.
7. The cache, last, with before and after numbers from `bench/instances.rhue`.

Everything lands on the `3.0` branch, and `main` gets merged into it as 2.x work lands.
