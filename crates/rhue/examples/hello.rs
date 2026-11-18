//! The smallest useful host: run a script, give it a Rust function, read
//! back what it computed.
//!
//! Run with `cargo run -p rhue --example hello`.

use rhue::Engine;

fn main() -> Result<(), rhue::Error> {
    let mut engine = Engine::new();

    // A Rust function scripts can call. Its types come from the signature,
    // so `--!strict` scripts are checked against it.
    engine.register("shout", |s: String| s.to_uppercase() + "!");

    // A value the script can read.
    engine.set_global("name", "Aalto");

    let greeting: String = engine.eval_as(
        r#"
        --!strict
        local function greet(who: string): string
            return shout("hello, " .. who)
        end
        return greet(name)
        "#,
    )?;
    println!("{greeting}");

    // Errors carry the source, so they can point at the problem.
    if let Err(e) = engine.eval_named("oops.rhue", "local x = 1 +\n  nil") {
        println!("{}", e.render(false));
    }
    Ok(())
}
