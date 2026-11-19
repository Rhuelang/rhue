//! Milestone 21's engine side: the type of an expression, the way a REPL's
//! `:type` shows it. (The REPL and `rhue test` get tested in `rhue-cli`.)

use rhue::Engine;

#[test]
fn expression_types() {
    let mut e = Engine::new();
    let ty = |e: &mut Engine, src: &str| e.expression_type(src).map_err(|err| err.render(false));
    assert_eq!(ty(&mut e, "1 + 2").unwrap(), "number");
    assert_eq!(
        ty(&mut e, "{ x = 1, name = 'a' }").unwrap(),
        "{ x: number, name: string }"
    );
    assert_eq!(ty(&mut e, "string.upper").unwrap(), "(string) -> string");
    assert_eq!(ty(&mut e, "vector.create(1, 2) * 2").unwrap(), "vector");
    // globals the engine knows about are in scope, typed as declared
    e.declare("score", "number").unwrap();
    e.eval("--!strict\nfunction double(n: number): number return n * 2 end")
        .unwrap();
    assert_eq!(ty(&mut e, "double(score)").unwrap(), "number");
    // errors are errors, multi-line expressions work
    assert!(
        ty(&mut e, "double('x')")
            .unwrap_err()
            .contains("could not be converted")
    );
    assert!(ty(&mut e, "1 +").is_err());
    assert_eq!(ty(&mut e, "{\n  1,\n  2,\n}").unwrap(), "{number}");
    // nothing runs
    e.eval("count = 0").unwrap();
    ty(&mut e, "(function() count = 1 return count end)()").unwrap();
    assert_eq!(e.eval_as::<f64>("return count").unwrap(), 0.0);
}
