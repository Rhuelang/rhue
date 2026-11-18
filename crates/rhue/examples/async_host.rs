//! An async host on tokio: scripts call async Rust functions and wait on
//! timers without blocking anything, and a runaway script gets stopped.
//!
//! Run with `cargo run -p rhue --example async_host`.

use std::time::{Duration, Instant};

use rhue::Engine;

// ─ Host Functions ─────────────────────────────────────────────────────────────

/// Pretend network call, takes a bit and sometimes fails.
async fn weather(city: String) -> Result<String, String> {
    tokio::time::sleep(Duration::from_millis(300)).await;
    match city.as_str() {
        "Atlantis" => Err(format!("no station in {city}")),
        _ => Ok(format!("{city}: {}°", city.len() * 3)),
    }
}

// rhue futures aren't Send, so a current-thread runtime it is
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), rhue::Error> {
    let mut engine = Engine::new();
    engine.open_tasks();
    engine.set_sleep(tokio::time::sleep);
    engine.register_async("weather", weather);
    engine.register("log", |s: String| println!("  {s}"));

    // three lookups at once, so this takes ~300ms and not ~900ms
    let started = Instant::now();
    engine
        .eval_async(
            r#"
            local done = 0
            for _, city in { "Adams", "Watertown", "Atlantis" } do
                task.spawn(function()
                    local ok, report = pcall(weather, city)
                    log(if ok then report else "failed: " .. report)
                    done += 1
                end)
            end
            while done < 3 do task.wait(0.05) end
            "#,
        )
        .await?;
    println!("three lookups in {:?}", started.elapsed());

    // a script that never stops on its own. It never yields either, so the
    // runtime's own thread is stuck in it and the stop has to come from
    // another thread (the handle is Send for this)
    let handle = engine.interrupt_handle();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        handle.interrupt();
    });
    let err = engine.eval_async("while true do end").await.unwrap_err();
    println!(
        "runaway script: {err} (interrupted: {})",
        err.is_interrupted()
    );
    Ok(())
}
