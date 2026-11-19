//! Modules for `require`. Some come as source, one compiled ahead of time (like
//! a host would ship it), and the rest get found by a loader. Types travel with
//! them, so a strict script gets checked against what it requires.
//!
//! Run with `cargo run -p rhue --example modules`.

use std::collections::HashMap;

use rhue::Engine;

fn main() -> Result<(), rhue::Error> {
    let mut engine = Engine::new();

    // from source
    engine.add_module(
        "vec2",
        r#"
        --!strict
        local M = {}
        function M.new(x: number, y: number) return { x = x, y = y } end
        function M.length(v: { x: number, y: number }): number
            return math.sqrt(v.x * v.x + v.y * v.y)
        end
        return M
        "#,
    );

    // compiled ahead of time. the bytes load without parsing or checking, go
    // through the bytecode verifier, and keep the module's types
    let mut build = Engine::new();
    let bytes = build.compile_to_bytes(
        "units",
        "--!strict\nreturn { metres = function(feet: number): number return feet * 0.3048 end }",
    )?;
    println!("units compiled to {} bytes", bytes.len());
    engine.add_module_bytes("units", &bytes)?;

    // anything else comes from a loader (a folder, an archive, a network
    // cache...), here it's a map
    let files: HashMap<&str, &str> =
        HashMap::from([("greet", "return function(n) return 'hello ' .. n end")]);
    engine.set_module_loader(move |name| files.get(name).map(|s| s.to_string()));

    let report: String = engine.eval_as(
        r#"
        --!strict
        local vec2 = require("vec2")
        local units = require("units")
        local greet = require("greet")
        local d = vec2.length(vec2.new(30, 40))
        return greet("Aalto") .. ": " .. d .. " ft = " .. units.metres(d) .. " m"
        "#,
    )?;
    println!("{report}");

    // the checker knows `vec2.length` returns a number
    let err = engine
        .check(
            "bad.rhue",
            "--!strict\nlocal s: string = require('vec2').length(require('vec2').new(1, 1))",
        )
        .unwrap_err();
    println!("checked: {err}");
    Ok(())
}
