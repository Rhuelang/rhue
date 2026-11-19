//! Hot reload. A game loop runs an NPC script out of a folder of `.rhue` files,
//! and every frame checks if any module it loaded changed on disk. When one
//! does it gets reloaded in place, so the NPC picks up the new code and keeps
//! its state.
//!
//! ```text
//! cargo run -p rhue --example hot_reload              # demo, edits its own files
//! cargo run -p rhue --example hot_reload -- scripts/  # watch a folder of yours
//! ```
//!
//! Pointed at a folder it runs `main.rhue` from there and keeps going until
//! Ctrl-C, so you can edit the modules it requires and watch it change.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rhue::Engine;

const MAIN: &str = r#"
local npc = require("npc")
while true do
    npc.tick()
    task.wait(1)
end
"#;

const NPC_V1: &str = r#"
local M = { steps = 0 }

function M.tick()
    M.steps += 1
    say("walks (step " .. M.steps .. ")")
end

return M
"#;

const NPC_V2: &str = r#"
local M = { steps = 0 }

function M.tick()
    M.steps += 1
    say("runs! (step " .. M.steps .. ")")
end

return M
"#;

/// The file `require(name)` reads, same as the CLI.
fn module_path(dir: &Path, name: &str) -> Option<PathBuf> {
    rhue::module_paths(dir, name)
        .into_iter()
        .find(|p| p.is_file())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let watching = std::env::args().nth(1).map(PathBuf::from);
    let demo = watching.is_none();
    let dir = match &watching {
        Some(d) => d.clone(),
        None => {
            let d = std::env::temp_dir().join(format!("rhue-hot-reload-{}", std::process::id()));
            std::fs::create_dir_all(&d)?;
            std::fs::write(d.join("main.rhue"), MAIN)?;
            std::fs::write(d.join("npc.rhue"), NPC_V1)?;
            d
        }
    };

    let mut engine = Engine::new();
    engine.open_tasks();
    engine.register("say", |line: String| println!("npc {line}"));
    let loader_dir = dir.clone();
    engine.set_module_loader(move |name| {
        std::fs::read_to_string(module_path(&loader_dir, name)?).ok()
    });

    let main = std::fs::read_to_string(dir.join("main.rhue"))?;
    engine.spawn("main.rhue", &main)?;

    // what each loaded module looked like on disk last time we checked. reading
    // them is cheap and dodges coarse mtimes
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    let frames = if demo { 6 } else { usize::MAX };
    for frame in 1..=frames {
        if demo && frame == 4 {
            println!("-- (pretend you just saved npc.rhue)");
            std::fs::write(dir.join("npc.rhue"), NPC_V2)?;
        }

        for name in engine.loaded_modules() {
            let now = module_path(&dir, &name).and_then(|p| std::fs::read_to_string(p).ok());
            let before = seen.insert(name.clone(), now.clone());
            // the first look just records it
            if before.is_some_and(|b| b != now) {
                match engine.reload(&name) {
                    Ok(r) => println!("-- reloaded {name}, swapped {} function(s)", r.functions),
                    // a broken edit leaves the old code running, fix it and save again
                    Err(e) => eprint!("{}", e.render(true)),
                }
            }
        }

        for e in engine.step(1.0) {
            eprint!("{}", e.render(true));
        }
        if !demo {
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    if demo {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}
