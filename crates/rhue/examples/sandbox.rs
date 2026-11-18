//! Running untrusted plugins: each gets its own globals, and limits on
//! instructions and memory. A plugin that loops forever or hoards memory
//! fails on its own; the others, and the host, carry on.
//!
//! Run with `cargo run -p rhue --example sandbox`.

use rhue::{Engine, Environment};

const PLUGINS: &[(&str, &str)] = &[
    (
        "greeter",
        r#"
        greeting = "hi"   -- a global, but only in this plugin's environment
        log("greeter loaded")
        function on_join(player) return greeting .. ", " .. player end
        "#,
    ),
    ("spinner", "function on_join(player) while true do end end"),
    (
        "hoarder",
        "function on_join(player) local t = {} for i = 1, 1e8 do t[i] = player end end",
    ),
    (
        "snoop",
        // `greeting` belongs to another plugin, so here it's nil.
        "function on_join(player) return 'greeting is ' .. tostring(greeting) end",
    ),
];

fn main() -> Result<(), rhue::Error> {
    let mut engine = Engine::new();
    engine.set_instruction_limit(Some(1_000_000));
    engine.set_memory_limit(Some(8 << 20));
    // Something every plugin may use.
    engine.register("log", |msg: String| println!("  [log] {msg}"));

    let mut loaded: Vec<(&str, Environment)> = Vec::new();
    for &(name, source) in PLUGINS {
        let env = engine.create_environment();
        let proto = engine.compile_in(&env, name, source)?;
        engine.run_in(&env, &proto)?;
        loaded.push((name, env));
    }

    for (name, env) in &loaded {
        let result = engine.eval_in(env, "return on_join('Zae')");
        match result {
            Ok(values) => println!("{name}: {values:?}"),
            Err(e) => println!("{name} failed: {e}"),
        }
    }

    // The engine is still fine after the failures.
    let n: f64 = engine.eval_as("return 6 * 7")?;
    println!("host: {n}");
    Ok(())
}
