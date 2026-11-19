let t = [];
for i in 1..=300000 { t.push(i * 2); }
let s = 0;
for v in t { s += v; }
for i in 0..t.len() { s += t[i]; }
let points = [];
for i in 1..=100000 { points.push(#{ x: i, y: i * 2, z: -i }); }
let acc = 0;
for i in 0..points.len() {
    let p = points[i];
    acc += p.x + p.y + p.z;
    points[i].x = p.y;
}
let h = #{};
for i in 1..=50000 { h["k" + (i % 5000)] = i; }
let n = 0;
for v in h.values() { n += v; }
[s, acc, n]
