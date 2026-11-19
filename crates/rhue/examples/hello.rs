//! The smallest useful host. Runs a script, hands it a Rust function, reads
//! back what it computed.
//!
//! Run with `cargo run -p rhue --example hello`.

use rhue::Engine;

fn main() -> Result<(), rhue::Error> {
    let mut engine = Engine::new();

    // a Rust function scripts can call. its types come from the signature, so
    // `--!strict` scripts get checked against it
    engine.register("shout", |s: String| s.to_uppercase() + "!");

    // a value the script can read
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

    // errors carry the source, so they can point at the problem
    if let Err(e) = engine.eval_named("oops.rhue", "local x = 1 +\n  nil") {
        println!("{}", e.render(false));
    }
    Ok(())
}
