let parts = [];
for i in 1..=100000 { parts.push("item" + i); }
let joined = "";
let first = true;
for p in parts {
    if !first { joined += ","; }
    joined += p;
    first = false;
}
let count = 0;
for i in 0..100000 {
    let s = parts[i];
    if s.sub_string(0, 4) == "item" { count += s.len(); }
}
let up = 0;
for i in 0..30000 {
    let u = parts[i];
    u.make_upper();
    up += u.len() + (`${i + 1}:${parts[i]}`).len();
}
[joined.len(), count, up]
