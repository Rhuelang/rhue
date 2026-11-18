fn counter() {
    let n = 0;
    |by| { n += by; n }
}
let total = 0;
for i in 1..=2000 {
    let c = counter();
    for j in 1..=200 { total += c.call(j); }
}
fn map(t, f) {
    let out = [];
    for x in t { out.push(f.call(x)); }
    out
}
let xs = [];
for i in 1..=1000 { xs.push(i); }
let scale = 3;
for round in 1..=300 {
    xs = map(xs, |x| (x * scale) % 1009);
}
[total, xs[0]]
