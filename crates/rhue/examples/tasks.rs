//! Scripts that wait: a game loop advances the clock, and NPC scripts use
//! Luau's `task` library (`task.wait`, `task.delay`, `task.spawn`) to act
//! over time. Nothing runs on its own; time only passes in `step`.
//!
//! Run with `cargo run -p rhue --example tasks`.

use std::cell::Cell;
use std::rc::Rc;

use rhue::Engine;

fn main() -> Result<(), rhue::Error> {
    let mut engine = Engine::new();
    engine.open_tasks();

    // The host's clock, so scripts can print when things happen.
    let now = Rc::new(Cell::new(0.0f64));
    let clock = now.clone();
    engine.register("say", move |who: String, line: String| {
        println!("[{:>4.1}s] {who}: {line}", clock.get());
    });

    engine.spawn(
        "guard",
        r#"
        say("guard", "on patrol")
        for post = 1, 3 do
            task.wait(1.0)
            say("guard", "reached post " .. post)
        end
        say("guard", "done")
        "#,
    )?;
    engine.spawn(
        "merchant",
        r#"
        task.delay(1.5, function() say("merchant", "fresh bread!") end)
        task.spawn(function()
            while true do
                task.wait(2.0)
                say("merchant", "still open")
            end
        end)
        "#,
    )?;

    // The game loop: half-second frames.
    for _ in 0..10 {
        now.set(now.get() + 0.5);
        for e in engine.step(0.5) {
            println!("a task failed: {e}");
        }
    }
    Ok(())
}
