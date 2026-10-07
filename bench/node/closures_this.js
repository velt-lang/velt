// Closures capturing `this` in constructors and methods (same workload as closures_this.vlt).
class Particle {
  constructor(x, v) {
    this.hits = 0;
    this.x = x;
    this.v = v;
    const wrap = () => {
      if (this.x > 1000) {
        this.x -= 1000;
        this.hits += 1;
      }
    };
    wrap();
  }

  step(k) {
    const move = (d) => {
      this.x += this.v * d;
      if (this.x > 1000) {
        this.x -= 1000;
        this.hits += 1;
      }
    };
    move(k);
    move(k + 1);
    const me = this;
    return me.x + me.hits;
  }
}

let total = 0;
const keep = [];
for (let i = 0; i < 5000000; i++) {
  const p = new Particle(i % 1500, (i % 7) + 1);
  total = (total + p.step(i % 3)) % 1000003;
  if (i % 100000 == 0) {
    keep.push(p);
  }
}
let hits = 0;
for (const p of keep) {
  hits += p.hits;
}
console.log(total, keep.length, hits);
