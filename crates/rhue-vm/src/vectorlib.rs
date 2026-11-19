//! `vector`, same as Luau: making and measuring vectors. Arithmetic on them (`+
//! - * / //`, unary `-`) and reading `v.x` live in the interpreter. Components
//! are 32-bit floats, so results get rounded to `f32`.

use std::rc::Rc;

use smallvec::smallvec;

use crate::stdlib::{bad_arg, num};
use crate::{NativeError, Rets, Value, Vm};

type R = Result<Rets, NativeError>;
type V = [f32; 3];

fn vec_arg(args: &[Value], i: usize, f: &str) -> Result<V, NativeError> {
    match args.get(i).copied().unwrap_or(Value::Nil) {
        Value::Vector(v) => Ok(v),
        other => Err(bad_arg(i + 1, f, "vector", other)),
    }
}

fn vector(v: V) -> R {
    Ok(smallvec![Value::Vector(v)])
}

fn number(n: f32) -> R {
    Ok(smallvec![Value::Number(f64::from(n))])
}

fn dot(a: V, b: V) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn magnitude(v: V) -> f32 {
    dot(v, v).sqrt()
}

/// Componentwise over one vector.
fn map(f: &'static str, op: fn(f32) -> f32) -> crate::NativeFn {
    Rc::new(move |_, a| vector(vec_arg(a, 0, f)?.map(op)))
}

/// Componentwise over any number of vectors (at least one).
fn fold(f: &'static str, op: fn(f32, f32) -> f32) -> crate::NativeFn {
    Rc::new(move |_, a| {
        let mut acc = vec_arg(a, 0, f)?;
        for i in 1..a.len() {
            let v = vec_arg(a, i, f)?;
            acc = [0, 1, 2].map(|k| op(acc[k], v[k]));
        }
        vector(acc)
    })
}

pub(crate) fn install(vm: &mut Vm) {
    let entries: &[(&str, crate::NativeFn)] = &[
        (
            "create",
            Rc::new(|_, a| {
                let z = match a.get(2) {
                    None | Some(Value::Nil) => 0.0,
                    Some(_) => num(a, 2, "create")?,
                };
                vector([
                    num(a, 0, "create")? as f32,
                    num(a, 1, "create")? as f32,
                    z as f32,
                ])
            }),
        ),
        (
            "magnitude",
            Rc::new(|_, a| number(magnitude(vec_arg(a, 0, "magnitude")?))),
        ),
        (
            "normalize",
            Rc::new(|_, a| {
                let v = vec_arg(a, 0, "normalize")?;
                // same as Luau, the zero vector comes back as NaNs
                let inv = 1.0 / magnitude(v);
                vector(v.map(|c| c * inv))
            }),
        ),
        (
            "cross",
            Rc::new(|_, a| vector(cross(vec_arg(a, 0, "cross")?, vec_arg(a, 1, "cross")?))),
        ),
        (
            "dot",
            Rc::new(|_, a| number(dot(vec_arg(a, 0, "dot")?, vec_arg(a, 1, "dot")?))),
        ),
        (
            "angle",
            Rc::new(|_, a| {
                // the angle from `a` to `b` in radians, in [0, pi]. with an
                // axis it's signed by which side of it the turn is on
                let (x, y) = (vec_arg(a, 0, "angle")?, vec_arg(a, 1, "angle")?);
                let c = cross(x, y);
                let angle = magnitude(c).atan2(dot(x, y));
                match a.get(2) {
                    None | Some(Value::Nil) => number(angle),
                    Some(_) => {
                        let axis = vec_arg(a, 2, "angle")?;
                        number(if dot(c, axis) < 0.0 { -angle } else { angle })
                    }
                }
            }),
        ),
        ("floor", map("floor", f32::floor)),
        ("ceil", map("ceil", f32::ceil)),
        ("abs", map("abs", f32::abs)),
        (
            "sign",
            map("sign", |c| {
                if c > 0.0 {
                    1.0
                } else if c < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            }),
        ),
        (
            "clamp",
            Rc::new(|_, a| {
                let (v, lo, hi) = (
                    vec_arg(a, 0, "clamp")?,
                    vec_arg(a, 1, "clamp")?,
                    vec_arg(a, 2, "clamp")?,
                );
                for (k, axis) in ["x", "y", "z"].iter().enumerate() {
                    if lo[k] > hi[k] {
                        return Err(format!(
                            "invalid argument #3 to 'clamp' (max.{axis} must be greater than or equal to min.{axis})"
                        )
                        .into());
                    }
                }
                vector([0, 1, 2].map(|k| v[k].max(lo[k]).min(hi[k])))
            }),
        ),
        ("max", fold("max", f32::max)),
        ("min", fold("min", f32::min)),
    ];
    let lib = crate::stdlib::library(vm, entries);
    if let Value::Table(t) = lib {
        let zero = vm.string("zero");
        let one = vm.string("one");
        vm.heap_mut().with_table(t, |t| {
            let _ = t.set(zero, Value::Vector([0.0; 3]));
            let _ = t.set(one, Value::Vector([1.0; 3]));
        });
    }
    vm.set_global("vector", lib);
}
