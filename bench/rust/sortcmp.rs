// Sorting with comparators (same workload as sortcmp.vlt), with the stable `sort_by` like
// JavaScript's `sort(compareFn)`; class instances are boxed, numbers are f64.
struct Person {
    name: String,
    age: f64,
    score: f64,
}

fn main() {
    let mut nums: Vec<f64> = Vec::new();
    let mut x: i64 = 1;
    for _ in 0..1_000_000 {
        x = (x * 48271) % 2147483647;
        nums.push((x % 1000000) as f64);
    }
    nums.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut people: Vec<Box<Person>> = Vec::new();
    for _ in 0..300_000 {
        x = (x * 48271) % 2147483647;
        people.push(Box::new(Person {
            name: format!("p{}", x % 50000),
            age: (x % 90) as f64,
            score: (x % 10007) as f64 / 7.0,
        }));
    }
    people.sort_by(|a, b| a.age.partial_cmp(&b.age).unwrap().then_with(|| a.name.cmp(&b.name)));
    let mut by_score: Vec<&Person> = people[..100_000].iter().map(|b| &**b).collect();
    by_score.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    println!("{} {} {} {} {}", nums[0], nums[500000], nums[999999], people[0].name, people[299999].name);
    println!("{:.3} {:.3}", by_score[0].score, by_score[99999].score);
}
