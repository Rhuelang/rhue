# Moving to 3.0

3.0 is the class model: one Rust type behind a whole tree of classes, objects that keep their identity, weak tables, and definitions files so the tools know your host. It's all new API, so a 2.x host that just bumps the version keeps building and running. This is what to check, and how to move over if you want to.

## What you have to check

Nothing in `rhue` got removed or renamed. `#[derive(UserData)]`, `register_type`, `TypeBuilder`, `declare` and everything else work the same, and a type registered the old way is a class with no parent now, which you won't notice.

What scripts can tell:

- `__mode` on a metatable does something now. `"k"`, `"v"` or `"kv"` make a table weak, like Luau. A script that used `__mode` as a plain field for something else gets weak tables it didn't ask for.
- `@` is a token, for `@class` in definitions files. A stray `@` in a script used to be an `unexpected character` error and is a parse error now (``expected an expression, found `@` ``). Still an error, different words.

If you use the guts crates (`rhue_types`, `rhue_vm`) directly, which you shouldn't need to:

- `rhue_types::Env::declare_class` takes the parent, pass `None` for what you had.
- `rhue_types::ClassType` has `index` and `class_params` fields, so a struct literal of it needs them (or `..Default::default()`).
- `rhue_parser::ast::Declaration` and friends are new, and `rhue_lexer::Token` has `At`.

## Moving a handle host over

This is for a host where every object is the same handle into its own engine and the classes come from data, the kind that in 2.x ended up with one wrapper type per class, or one type and a `ClassName` property with no checker support. If your host is a few `#[derive(UserData)]` structs, none of this is for you.

1. **Classes.** Swap the wrapper types for `define_class::<Handle>("Name")` with `.parent(&parent)`. Members take `&Handle`, not `&mut`, so the handle does its own locking and a member that calls back into a script touching the same object stops hitting `T is already in use`. Hosts that read their classes from reflection data use the `_dyn` builders, which take the script type as a string.

   ```rust
   let node = engine.define_class::<Handle>("Node")
       .property("Name", |h: &Handle| h.name(), |h: &Handle, v: String| h.set_name(v))
       .finish();
   let part = engine.define_class::<Handle>("Part").parent(&node).finish();
   ```

2. **One script object per host object.** Hand objects over with `cx.object(&class, id, || handle)` instead of `create_object`, keyed by your engine's id for the thing. Then the same object reached twice is `==` and can be a table key, so any `__eq` or id-comparing helpers you had for that can go.

3. **Setters that can say no.** `setter_with` gets the `Context` and returns a `Result`, so validation and changed events go in the setter instead of a wrapper function scripts had to call.

4. **Children by name.** `.index(...)` on a class handles names that aren't members (`world.Box`), and `declare_index` tells the checker what it gives. Members always win, so a child can't hide a property.

5. **The tools.** Write the engine's view out once and check it in:

   ```rust
   std::fs::write("host.d.rhue", engine.definitions())?;
   ```

   `rhue --check --definitions host.d.rhue` and `rhue-lsp` (a `*.d.rhue` at the workspace root, or the `rhue.definitions` setting) then know your classes. A test with `check_definitions` keeps the file from going stale.

6. **Class names as strings.** Mark the arguments that take a class name, `declare_class_param("Node", "IsA", 0)` and `declare_global_class_param("Instance.new", 0)` (or `@class(param)` in the file), and strict scripts get `Part` back from `Instance.new("Part")` and narrowing from `if x:IsA("Part") then` without casts.

Scripts don't change for any of this, it's all on the host side. Method calls on these objects also get cached per call site, which is free.

The why behind each of these is in [DESIGN.md](DESIGN.md), and the plan they came from is [class-model.md](class-model.md).
