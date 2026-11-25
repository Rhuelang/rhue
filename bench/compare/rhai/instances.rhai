// Rhai maps only take string keys, so `near` counts by index instead of by
// object. Everything else matches instances.rhue.
let bodies = [];
for i in 1..=16 {
    bodies.push(new_body(i.to_float(), 0.0, -i.to_float(), (2 ** (i % 3)).to_float()));
}
let target = new_body(0.0, 0.0, 0.0, 1.0);
let near = [];
near.pad(16, 0);
let dt = 0.5;
let hits = 0;
let weight = 0.0;
for frame in 1..=6000 {
    let force = if frame % 16 < 8 { vec3(1.0, 0.0, -1.0) } else { vec3(-1.0, 0.0, 1.0) };
    for i in 0..16 {
        let b = bodies[i];
        b.position = b.position + b.velocity * dt;
        if frame % 8 == (i + 1) % 8 {
            b.push(force);
            weight += weigh(b);
        }
        let d = b.offset(target);
        if dot(d, d) < 400.0 {
            hits += 1;
            near[i] += 1;
        }
    }
}
let sum = 0.0;
for b in bodies {
    let p = b.position;
    sum += p.x - p.z;
}
let distinct = 0;
for n in near {
    if n > 0 { distinct += 1; }
}
[(sum * 8.0).floor().to_int(), hits, distinct, weight.to_int()]
