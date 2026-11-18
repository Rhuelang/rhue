fn add(o) { #{ x: this.x + o.x, y: this.y + o.y } }
fn len2() { this.x * this.x + this.y * this.y }
let acc = #{ x: 0, y: 0 };
let step = #{ x: 1, y: -1 };
let s = 0;
for i in 1..=400000 {
    acc = acc.add(step);
    s += acc.len2() % 13;
}
s
