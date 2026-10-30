# velt/collections

`import { PriorityQueue } from "velt/collections"`. A binary heap ordered by a
comparator. `cmp(a, b) < 0` means `a` comes out first, so `(a, b) => a - b` gives a min-heap.

- `new PriorityQueue<T>(cmp)`, `size` / `length`, `isEmpty()`, `clear()`.
- `push(v)`, `pop(): T | undefined`: both O(log n). `peek(): T | undefined` returns a clone (`undefined` when empty).
- `toSortedArray()`: clones, stable for equal elements. `toArray()`: clones in heap order.

```ts
import { PriorityQueue } from "velt/collections";

struct Job {
  name: string;
  priority: i64;
}

function main() {
  const jobs = new PriorityQueue<Job>((a, b) => b.priority - a.priority);
  jobs.push({ name: "backup", priority: 1 });
  jobs.push({ name: "deploy", priority: 5 });
  jobs.push({ name: "email", priority: 3 });
  while (!jobs.isEmpty()) {
    const j = jobs.pop();
    if (j != null) {
      console.log(j.priority, j.name); // 5 deploy, 3 email, 1 backup
    }
  }
}
```

Notes: `pop` order among equal elements is unspecified.
