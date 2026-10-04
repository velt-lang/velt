// Sorting with comparators (same workload as sortcmp.vlt).
class Person {
  constructor(name, age, score) {
    this.name = name;
    this.age = age;
    this.score = score;
  }
}

const nums = [];
let x = 1;
for (let i = 0; i < 1000000; i++) {
  x = (x * 48271) % 2147483647;
  nums.push(x % 1000000);
}
nums.sort((a, b) => a - b);
const people = [];
for (let i = 0; i < 300000; i++) {
  x = (x * 48271) % 2147483647;
  people.push(new Person(`p${x % 50000}`, x % 90, (x % 10007) / 7));
}
people.sort((a, b) =>
  a.age !== b.age ? (a.age < b.age ? -1 : 1) : a.name < b.name ? -1 : a.name > b.name ? 1 : 0,
);
const byScore = people.slice(0, 100000);
byScore.sort((a, b) => (b.score > a.score ? 1 : b.score < a.score ? -1 : 0));
console.log(nums[0], nums[500000], nums[999999], people[0].name, people[299999].name);
console.log(byScore[0].score.toFixed(3), byScore[99999].score.toFixed(3));
