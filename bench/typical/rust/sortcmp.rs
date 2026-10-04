struct Person { name: String, age: i64, score: f64 }
fn main() {
    let mut nums: Vec<i64> = Vec::new();
    let mut x: i64 = 1;
    for _ in 0..1000000 { x = (x * 48271) % 2147483647; nums.push(x % 1000000); }
    nums.sort_by(|a, b| a.cmp(b));
    let mut people: Vec<Box<Person>> = Vec::new();
    for _ in 0..300000 { x = (x * 48271) % 2147483647; people.push(Box::new(Person { name: format!("p{}", x % 50000), age: x % 90, score: (x % 10007) as f64 / 7.0 })); }
    people.sort_by(|a, b| a.age.cmp(&b.age).then_with(|| a.name.cmp(&b.name)));
    let mut by: Vec<&Person> = people[..100000].iter().map(|b| &**b).collect();
    by.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    println!("{} {} {} {} {} {:.3}", nums[0], nums[500000], nums[999999], people[0].name, people[299999].name, by[0].score);
}
