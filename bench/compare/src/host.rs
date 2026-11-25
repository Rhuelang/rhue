//! The `Body` type `instances.rhue` calls into, registered on each engine the
//! way a host would. It's the same type as in the Rhue bench runner
//! (`crates/rhue/examples/bench.rs`), so keep the two in step.
//!
//! Vectors are three `f32`s everywhere, which is what Rhue and Luau use, so
//! all three engines round the same way.

use std::cell::RefCell;
use std::rc::Rc;

// ─ Rhue ──────────────────────────────────────────────────────────────────────

#[derive(rhue::UserData, Clone)]
struct Body {
    position: [f32; 3],
    velocity: [f32; 3],
    mass: f32,
}

pub fn rhue(engine: &mut rhue::Engine) {
    engine
        .register_type::<Body>()
        .function("new", |x: f32, y: f32, z: f32, mass: f32| Body {
            position: [x, y, z],
            velocity: [0.0; 3],
            mass,
        })
        .method("push", |b: &mut Body, force: [f32; 3]| {
            let m = b.mass;
            for (v, f) in b.velocity.iter_mut().zip(force) {
                *v += f / m;
            }
        })
        .method("offset", |b: &mut Body, other: rhue::Ud<Body>| {
            let o = other.borrow().position;
            [0, 1, 2].map(|k| b.position[k] - o[k])
        });
    engine.register("weigh", |b: rhue::Ud<Body>| b.borrow().mass);
}

// ─ Luau ──────────────────────────────────────────────────────────────────────

struct LuauBody {
    position: mlua::Vector,
    velocity: mlua::Vector,
    mass: f32,
}

fn parts(v: mlua::Vector) -> [f32; 3] {
    [v.x(), v.y(), v.z()]
}

fn vector([x, y, z]: [f32; 3]) -> mlua::Vector {
    mlua::Vector::new(x, y, z)
}

impl mlua::UserData for LuauBody {
    fn add_fields<F: mlua::UserDataFields<Self>>(f: &mut F) {
        f.add_field_method_get("position", |_, b| Ok(b.position));
        f.add_field_method_set("position", |_, b, v: mlua::Vector| {
            b.position = v;
            Ok(())
        });
        f.add_field_method_get("velocity", |_, b| Ok(b.velocity));
        f.add_field_method_set("velocity", |_, b, v: mlua::Vector| {
            b.velocity = v;
            Ok(())
        });
        f.add_field_method_get("mass", |_, b| Ok(b.mass));
        f.add_field_method_set("mass", |_, b, m: f32| {
            b.mass = m;
            Ok(())
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(m: &mut M) {
        m.add_method_mut("push", |_, b, force: mlua::Vector| {
            let (v, f) = (parts(b.velocity), parts(force));
            b.velocity = vector([0, 1, 2].map(|k| v[k] + f[k] / b.mass));
            Ok(())
        });
        m.add_method("offset", |_, b, other: mlua::UserDataRef<LuauBody>| {
            let (p, o) = (parts(b.position), parts(other.position));
            Ok(vector([0, 1, 2].map(|k| p[k] - o[k])))
        });
    }
}

pub fn luau(lua: &mlua::Lua) -> mlua::Result<()> {
    let body = lua.create_table()?;
    body.set(
        "new",
        lua.create_function(|lua, (x, y, z, mass): (f32, f32, f32, f32)| {
            lua.create_userdata(LuauBody {
                position: mlua::Vector::new(x, y, z),
                velocity: mlua::Vector::new(0.0, 0.0, 0.0),
                mass,
            })
        })?,
    )?;
    lua.globals().set("Body", body)?;
    lua.globals().set(
        "weigh",
        lua.create_function(|_, b: mlua::UserDataRef<LuauBody>| Ok(b.mass))?,
    )?;
    Ok(())
}

// ─ Rhai ──────────────────────────────────────────────────────────────────────

/// Rhai copies values around, so a host object is a shared handle, the way a
/// Rhai host keeps its objects.
#[derive(Clone)]
struct RhaiBody(Rc<RefCell<Body>>);

#[derive(Clone, Copy)]
struct Vec3([f32; 3]);

pub fn rhai(engine: &mut rhai::Engine) {
    engine
        .register_type_with_name::<Vec3>("Vec3")
        .register_fn("vec3", |x: f64, y: f64, z: f64| {
            Vec3([x as f32, y as f32, z as f32])
        })
        .register_fn("+", |a: Vec3, b: Vec3| {
            Vec3([0, 1, 2].map(|k| a.0[k] + b.0[k]))
        })
        .register_fn("*", |a: Vec3, s: f64| Vec3(a.0.map(|c| c * s as f32)))
        .register_fn("dot", |a: Vec3, b: Vec3| {
            f64::from(a.0[0] * b.0[0] + a.0[1] * b.0[1] + a.0[2] * b.0[2])
        })
        .register_get("x", |v: &mut Vec3| f64::from(v.0[0]))
        .register_get("z", |v: &mut Vec3| f64::from(v.0[2]));
    engine
        .register_type_with_name::<RhaiBody>("Body")
        .register_fn("new_body", |x: f64, y: f64, z: f64, mass: f64| {
            RhaiBody(Rc::new(RefCell::new(Body {
                position: [x as f32, y as f32, z as f32],
                velocity: [0.0; 3],
                mass: mass as f32,
            })))
        })
        .register_get_set(
            "position",
            |b: &mut RhaiBody| Vec3(b.0.borrow().position),
            |b: &mut RhaiBody, v: Vec3| b.0.borrow_mut().position = v.0,
        )
        .register_get_set(
            "velocity",
            |b: &mut RhaiBody| Vec3(b.0.borrow().velocity),
            |b: &mut RhaiBody, v: Vec3| b.0.borrow_mut().velocity = v.0,
        )
        .register_fn("push", |b: &mut RhaiBody, force: Vec3| {
            let mut b = b.0.borrow_mut();
            let m = b.mass;
            for (v, f) in b.velocity.iter_mut().zip(force.0) {
                *v += f / m;
            }
        })
        .register_fn("offset", |b: &mut RhaiBody, other: RhaiBody| {
            let (p, o) = (b.0.borrow().position, other.0.borrow().position);
            Vec3([0, 1, 2].map(|k| p[k] - o[k]))
        })
        .register_fn("weigh", |b: RhaiBody| f64::from(b.0.borrow().mass));
}
