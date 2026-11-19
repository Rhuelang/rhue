//! A small game-scripting host. Rust owns the world, a script drives it.
//!
//! Run with `cargo run -p rhue --example embed`.

use rhue::{Context, Engine, Function, ScriptError, UserData};

#[derive(UserData, Clone, Debug)]
struct Player {
    name: String,
    hp: f64,
    #[rhue(readonly)]
    level: u32,
}

fn main() -> Result<(), rhue::Error> {
    let mut engine = Engine::new();
    engine.set_instruction_limit(Some(1_000_000));

    // methods and a constructor for the Player type
    engine
        .register_type::<Player>()
        .function("new", |name: String| Player {
            name,
            hp: 100.0,
            level: 1,
        })
        .method("damage", |p: &mut Player, amount: f64| {
            p.hp = (p.hp - amount).max(0.0);
            p.hp == 0.0
        })
        .method("level_up", |p: &mut Player| p.level += 1);

    // a host function with a typed signature, and one that calls back
    engine.register("log", |msg: String| println!("[script] {msg}"));
    engine.register(
        "repeat_n",
        |cx: &mut Context, n: u32, f: Function| -> Result<(), ScriptError> {
            for i in 1..=n {
                cx.call::<()>(&f, i)?;
            }
            Ok(())
        },
    );

    engine.eval_named(
        "game.rhue",
        r#"
        hero = Player.new("Aalto")
        repeat_n(3, function(i)
            hero:damage(20 * i)
            log("hit " .. i .. ": " .. hero.name .. " has " .. hero.hp .. " hp")
        end)
        hero:level_up()

        function on_tick(dt)
            hero.hp = math.min(hero.hp + 10 * dt, 100)
            return hero.hp
        end
        "#,
    )?;

    // Rust drives the script's callback each "frame"
    for _ in 0..3 {
        let hp: f64 = engine.call_global("on_tick", 0.5)?;
        println!("tick -> {hp} hp");
    }

    let hero: Player = engine.get("hero")?;
    println!("{hero:?}");
    Ok(())
}
