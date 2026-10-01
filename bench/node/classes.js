// Dynamic dispatch (same workload as classes.vlt).
class Shape {
  constructor(id) {
    this.id = id;
  }
  area() {
    return 0.0;
  }
  weight() {
    return this.id % 3;
  }
}

class Circle extends Shape {
  constructor(id, r) {
    super(id);
    this.r = r;
  }
  area() {
    return 3.0 * this.r * this.r;
  }
}

class Rect extends Shape {
  constructor(id, w, h) {
    super(id);
    this.w = w;
    this.h = h;
  }
  area() {
    return this.w * this.h;
  }
}

class Square extends Shape {
  constructor(id, side) {
    super(id);
    this.side = side;
  }
  area() {
    return this.side * this.side;
  }
}

class AddScorer {
  constructor(k) {
    this.k = k;
  }
  score(x) {
    return x + this.k;
  }
}

class MulScorer {
  constructor(k) {
    this.k = k;
  }
  score(x) {
    return (x * this.k) % 1000003;
  }
}

class XorScorer {
  constructor(k) {
    this.k = k;
  }
  score(x) {
    return x ^ this.k;
  }
}

const shapes = [];
for (let i = 0; i < 1000000; i++) {
  const size = (i % 17) + 0.5;
  if (i % 3 === 0) {
    shapes.push(new Circle(i, size));
  } else if (i % 3 === 1) {
    shapes.push(new Rect(i, size, 2.0));
  } else {
    shapes.push(new Square(i, size));
  }
}
let total = 0.0;
let weights = 0;
for (let pass = 0; pass < 20; pass++) {
  for (const s of shapes) {
    total += s.area();
    weights += s.weight();
  }
}
console.log(total, weights);

const scorers = [];
for (let i = 0; i < 1000000; i++) {
  if (i % 3 === 0) {
    scorers.push(new AddScorer(i % 11));
  } else if (i % 3 === 1) {
    scorers.push(new MulScorer((i % 13) + 1));
  } else {
    scorers.push(new XorScorer(i % 7));
  }
}
let acc = 1;
for (let pass = 0; pass < 20; pass++) {
  for (const sc of scorers) {
    acc = sc.score(acc) % 1000003;
  }
}
console.log(acc);
