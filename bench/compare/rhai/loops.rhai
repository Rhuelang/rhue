let sum = 0;
for i in 1..=3000000 {
    let x = i % 7;
    if x < 3 {
        sum += x * 2;
    } else if x == 5 {
        sum -= 1;
    } else {
        sum += i / 1000;
    }
}
sum
