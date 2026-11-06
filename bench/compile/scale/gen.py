#!/usr/bin/env python3
"""Generate a Velt program of about N lines, and the same program in Go, for the compile-speed
scale benchmark (bench/compile/scale/run.sh).

    gen.py <lines> <out dir>

The program is split into modules of UNITS_PER_MODULE units; a unit is about 100 lines: a class
with fields, a constructor and methods (strings, arrays, template literals, loops), a subclass
with an override, free functions over arrays, and uses of a generic class and a generic
function from the shared module with the unit's own class (one instance per unit, as in a real
program). `main` calls every unit's entry and prints the sum, so nothing is dead code.

The Go program has the same shape: one package per module, the generic `Box[T]` and `First[T]`
in a shared package, an interface for the override.
"""
import os
import sys

UNITS_PER_MODULE = 10

VELT_COMMON = """\
export class Box<T> {
  items: T[];
  constructor() {
    this.items = [];
  }
  put(x: T): void {
    this.items.push(x);
  }
  size(): number {
    return this.items.length;
  }
  get(i: number): T {
    return this.items[i];
  }
}

export function first<T>(xs: T[]): T {
  return xs[0];
}

export function count<T>(xs: T[], keep: (x: T) => boolean): number {
  let n = 0;
  for (const x of xs) {
    if (keep(x)) {
      n += 1;
    }
  }
  return n;
}
"""

VELT_UNIT = """\
export class Item@K@ {
  id: number;
  name: string;
  tags: string[];
  weight: number;

  constructor(id: number, name: string) {
    this.id = id;
    this.name = name;
    this.tags = [];
    this.weight = id * 2 + @K@;
  }

  addTag(t: string): void {
    if (this.tags.length < 8) {
      this.tags.push(t);
    }
  }

  describe(): string {
    return `${this.name}#${this.id} [${this.tags.join(",")}]`;
  }

  score(): number {
    let s = 0;
    for (let i = 0; i < this.tags.length; i++) {
      s += this.tags[i].length * (i + 1);
    }
    return s + this.weight;
  }
}

export class Special@K@ extends Item@K@ {
  bonus: number;

  constructor(id: number, name: string, bonus: number) {
    super(id, name);
    this.bonus = bonus;
  }

  override score(): number {
    return super.score() + this.bonus;
  }
}

export function sum@K@(xs: number[]): number {
  let t = 0;
  for (const x of xs) {
    if (x % 2 == 0) {
      t += x;
    } else {
      t -= 1;
    }
  }
  return t;
}

export function build@K@(n: number): Item@K@[] {
  const out: Item@K@[] = [];
  for (let i = 0; i < n; i++) {
    const it: Item@K@ = i % 3 == 0 ? new Special@K@(i, "s@K@", i + 1) : new Item@K@(i, "item@K@");
    it.addTag("t" + i);
    out.push(it);
  }
  return out;
}

export function run@K@(): number {
  const items = build@K@(6);
  const box = new Box<Item@K@>();
  for (const it of items) {
    box.put(it);
  }
  const heavy = count(items, (it: Item@K@) => it.score() > 10);
  const scores: number[] = [];
  for (let i = 0; i < box.size(); i++) {
    scores.push(box.get(i).score());
  }
  const label = first(items).describe();
  return sum@K@(scores) + heavy + label.length;
}
"""

GO_COMMON = """\
package common

type Box[T any] struct {
\titems []T
}

func NewBox[T any]() *Box[T] {
\treturn &Box[T]{items: []T{}}
}

func (b *Box[T]) Put(x T) {
\tb.items = append(b.items, x)
}

func (b *Box[T]) Size() int {
\treturn len(b.items)
}

func (b *Box[T]) Get(i int) T {
\treturn b.items[i]
}

func First[T any](xs []T) T {
\treturn xs[0]
}

func Count[T any](xs []T, keep func(T) bool) int {
\tn := 0
\tfor _, x := range xs {
\t\tif keep(x) {
\t\t\tn++
\t\t}
\t}
\treturn n
}
"""

GO_UNIT = """\
type Scorer@K@ interface {
\tScore() int
\tDescribe() string
\tAddTag(t string)
}

type Item@K@ struct {
\tid     int
\tname   string
\ttags   []string
\tweight int
}

func NewItem@K@(id int, name string) *Item@K@ {
\treturn &Item@K@{id: id, name: name, tags: []string{}, weight: id*2 + @K@}
}

func (it *Item@K@) AddTag(t string) {
\tif len(it.tags) < 8 {
\t\tit.tags = append(it.tags, t)
\t}
}

func (it *Item@K@) Describe() string {
\treturn fmt.Sprintf("%s#%d [%s]", it.name, it.id, strings.Join(it.tags, ","))
}

func (it *Item@K@) Score() int {
\ts := 0
\tfor i := 0; i < len(it.tags); i++ {
\t\ts += len(it.tags[i]) * (i + 1)
\t}
\treturn s + it.weight
}

type Special@K@ struct {
\tItem@K@
\tbonus int
}

func NewSpecial@K@(id int, name string, bonus int) *Special@K@ {
\treturn &Special@K@{Item@K@: *NewItem@K@(id, name), bonus: bonus}
}

func (s *Special@K@) Score() int {
\treturn s.Item@K@.Score() + s.bonus
}

func Sum@K@(xs []int) int {
\tt := 0
\tfor _, x := range xs {
\t\tif x%2 == 0 {
\t\t\tt += x
\t\t} else {
\t\t\tt -= 1
\t\t}
\t}
\treturn t
}

func Build@K@(n int) []Scorer@K@ {
\tout := []Scorer@K@{}
\tfor i := 0; i < n; i++ {
\t\tvar it Scorer@K@
\t\tif i%3 == 0 {
\t\t\tit = NewSpecial@K@(i, "s@K@", i+1)
\t\t} else {
\t\t\tit = NewItem@K@(i, "item@K@")
\t\t}
\t\tit.AddTag("t" + strconv.Itoa(i))
\t\tout = append(out, it)
\t}
\treturn out
}

func Run@K@() int {
\titems := Build@K@(6)
\tbox := common.NewBox[Scorer@K@]()
\tfor _, it := range items {
\t\tbox.Put(it)
\t}
\theavy := common.Count(items, func(it Scorer@K@) bool { return it.Score() > 10 })
\tscores := []int{}
\tfor i := 0; i < box.Size(); i++ {
\t\tscores = append(scores, box.Get(i).Score())
\t}
\tlabel := common.First(items).Describe()
\treturn Sum@K@(scores) + heavy + len(label)
}
"""


def main():
    lines, out = int(sys.argv[1]), sys.argv[2]
    unit_lines = VELT_UNIT.count("\n") + 1
    units = max(1, lines // unit_lines)
    modules = (units + UNITS_PER_MODULE - 1) // UNITS_PER_MODULE
    velt, go = os.path.join(out, "velt"), os.path.join(out, "go")
    os.makedirs(velt, exist_ok=True)
    os.makedirs(os.path.join(go, "common"), exist_ok=True)
    open(os.path.join(velt, "common.velt"), "w").write(VELT_COMMON)
    open(os.path.join(go, "common", "common.go"), "w").write(GO_COMMON)
    open(os.path.join(go, "go.mod"), "w").write("module scale\n\ngo 1.22\n")
    total = VELT_COMMON.count("\n")
    entries = []
    for m in range(modules):
        ks = [m * UNITS_PER_MODULE + j for j in range(UNITS_PER_MODULE)]
        ks = [k for k in ks if k < units]
        src = ['import { Box, first, count } from "./common";\n\n']
        src += [VELT_UNIT.replace("@K@", str(k)) + "\n" for k in ks]
        text = "".join(src)
        open(os.path.join(velt, f"m{m}.velt"), "w").write(text)
        total += text.count("\n")
        pkg = os.path.join(go, f"m{m}")
        os.makedirs(pkg, exist_ok=True)
        gsrc = [f"package m{m}\n\nimport (\n\t\"fmt\"\n\t\"strconv\"\n\t\"strings\"\n\n"
                "\t\"scale/common\"\n)\n\n"]
        gsrc += [GO_UNIT.replace("@K@", str(k)) + "\n" for k in ks]
        open(os.path.join(pkg, f"m{m}.go"), "w").write("".join(gsrc))
        entries.append((m, ks))
    vmain = []
    for m, ks in entries:
        names = ", ".join(f"run{k}" for k in ks)
        vmain.append(f'import {{ {names} }} from "./m{m}";\n')
    vmain.append("\nfunction main() {\n  let t = 0;\n")
    for m, ks in entries:
        vmain += [f"  t += run{k}();\n" for k in ks]
    vmain.append("  console.log(t);\n}\n")
    text = "".join(vmain)
    open(os.path.join(velt, "main.velt"), "w").write(text)
    total += text.count("\n")
    gmain = ["package main\n\nimport (\n\t\"fmt\"\n\n"]
    gmain += [f'\t"scale/m{m}"\n' for m, _ in entries]
    gmain.append(")\n\nfunc main() {\n\tt := 0\n")
    for m, ks in entries:
        gmain += [f"\tt += m{m}.Run{k}()\n" for k in ks]
    gmain.append("\tfmt.Println(t)\n}\n")
    open(os.path.join(go, "main.go"), "w").write("".join(gmain))
    print(f"{total} Velt lines, {units} units, {modules} modules")


main()
