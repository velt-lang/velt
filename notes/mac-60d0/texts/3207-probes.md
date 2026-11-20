The reviewer's probes from #3201's review (handed-on sleeps), as asked for this issue. They ran on the builder at 1, 2, 4 and 8 workers.

### handed4.velt
```ts
// handed3.velt with the raw sleep handed on (no then): the second task polls the sleep itself.
// sleep (polled first by a task that has ended) is pending in the offering worker's block.
class Gate {
  stop: Atomic<i64> = new Atomic<i64>(0);
  fired: Atomic<i64> = new Atomic<i64>(0);
}

class Holder {
  p: Promise<void>;
  constructor(p: Promise<void>) {
    this.p = p;
  }
}

async function busy(gate: shared<Gate>): Promise<void> {
  while (gate.stop.load() == 0) {
    let x = 0;
    for (let i = 0; i < 20000; i++) {
      x += i % 7;
    }
    if (x < 0) {
      console.log("never");
    }
    await yieldNow();
  }
}

async function maker(gate: shared<Gate>, i: i64): Promise<Holder> {
  const s = sleep(5 + (i % 7));

  await Promise.race([s, yieldNow()]);
  return new Holder(s);
}

// Anchored by a pending sleep, queued again and again: what offers carry.
async function anchored(i: i64): Promise<void> {
  const long = sleep(30 + (i % 5));
  const short = sleep(1);
  await Promise.race([long, short]);
  for (let y = 0; y < 40; y++) {
    await yieldNow();
  }
}

async function user(gate: shared<Gate>, i: i64): Promise<void> {
  const h = await spawn(maker(gate, i));
  await Promise.race([h.p, yieldNow()]);
  for (let y = 0; y < 30 + (i % 7); y++) {
    await yieldNow();
  }
  await h.p;
  gate.fired.add(1);
}

async function main() {
  const gate = shared(new Gate());
  const busyOnes: Promise<void>[] = [];
  for (let i = 0; i < 8; i++) {
    busyOnes.push(spawn(busy(gate)));
  }
  for (let round = 0; round < 5; round++) {
    const ps: Promise<void>[] = [];
    for (let i = 0; i < 200; i++) {
      ps.push(spawn(user(gate, i)));
      ps.push(spawn(anchored(i)));
    }
    await Promise.all(ps);
  }
  gate.stop.store(1);
  await Promise.all(busyOnes);
  console.log(`fired ${gate.fired.load()}`);
}
```

### handed5.velt
```ts
// The race 14664a76d fixes: task K polls a sleep first (K anchored, S_OWNER = K), hands it to
// task X, which polls it too (its waker is now X's), and K then yields while worker 0 is busy, so
// an offer may carry K. Before the fix the sleep went with K, and its expiry touched X from K's
// new worker.
class Gate {
  stop: Atomic<i64> = new Atomic<i64>(0);
  fired: Atomic<i64> = new Atomic<i64>(0);
}

class Holder {
  p: Promise<void>;
  constructor(p: Promise<void>) {
    this.p = p;
  }
}

async function busy(gate: shared<Gate>): Promise<void> {
  while (gate.stop.load() == 0) {
    let x = 0;
    for (let i = 0; i < 20000; i++) {
      x += i % 7;
    }
    if (x < 0) {
      console.log("never");
    }
    await yieldNow();
  }
}

async function user(gate: shared<Gate>, h: Holder): Promise<void> {
  await Promise.race([h.p, yieldNow()]);
  await h.p;
  gate.fired.add(1);
}

async function maker(gate: shared<Gate>, i: i64): Promise<void> {
  const s = sleep(8 + (i % 5));
  await Promise.race([s, yieldNow()]);
  const u = spawn(user(gate, new Holder(s)));
  for (let y = 0; y < 40; y++) {
    await yieldNow();
  }
  await u;
}

async function main() {
  const gate = shared(new Gate());
  const busyOnes: Promise<void>[] = [];
  for (let i = 0; i < 8; i++) {
    busyOnes.push(spawn(busy(gate)));
  }
  for (let round = 0; round < 5; round++) {
    const ps: Promise<void>[] = [];
    for (let i = 0; i < 200; i++) {
      ps.push(spawn(maker(gate, i)));
    }
    await Promise.all(ps);
  }
  gate.stop.store(1);
  await Promise.all(busyOnes);
  console.log(`fired ${gate.fired.load()}`);
}
```

### The invariant check (mkinstr.sh), which instruments std's expiry with an ICE when a sleep expires off its task's home
```sh
#!/usr/bin/env bash
# Makes instrumented std copies: <dir>/any (panics when a sleep expires on a worker that is not its
# task's home) and <dir>/anch (only when that task is anchored), for base, old head, new head.
set -e
R=D:/velt-build/r3201/instr
mkdir -p $R
mk() {
  local name=$1 src=$2
  for v in any anch; do
    rm -rf $R/$name-$v; cp -r $src $R/$name-$v
    f=$R/$name-$v/lang/hooks/sched_sleep.velt
    if [ $v = any ]; then cond='m.ld(k + t.T_HOME) !== t.exec()'; else cond='m.ld(k + t.T_HOME) !== t.exec() \&\& t.has(k, t.ANCHORED)'; fi
    sed -i "s/^      pushDue(k, f);/      if ($cond) {\n        panic(\"ICE: probe: $v: a sleep expires off its task's home\");\n      }\n      pushDue(k, f);/" $f
    grep -c "ICE: probe" $f
  done
}
mk base D:/velt-build/r3201/base-src/std
mk old C:/Dev/velt/branches/r3201/std
mk new D:/velt-build/r3201/new/std
```

### stress.sh (how it was run)
```sh
#!/usr/bin/env bash
# stress.sh <label> <std dir> <runs> <prog...>: builds each prog against <std dir>, runs it <runs>
# times at VELT_THREADS=4 and 8 with the checking allocator; outcome counts (ICE lines included).
source D:/velt-build/r3201/env.sh
label=$1 std=$2 runs=$3; shift 3
export VELT_STD=$std
V=D:/velt-build/r3201/debug/velt.exe
P=D:/velt-build/r3201/probes
for n in "$@"; do
  exe=$P/$n.$label.exe
  VELT_SCHEDULER=std $V build $P/$n.velt -o $exe > $P/$n.$label.build 2>&1 || { echo "$label $n: build failed"; head $P/$n.$label.build; continue; }
  for th in 4 8; do
    out=$P/$n.$label.t$th.all; : > $out
    for i in $(seq $runs); do
      VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 timeout 120 $exe > $P/$n.$label.out 2> $P/$n.$label.err
      code=$?
      bal=$(grep -o 'blocks=[0-9]*/[0-9]*' $P/$n.$label.err | awk -F'[=/]' '{print ($2==$3)?"bal":"UNBAL"}')
      ice=$(grep -o 'ICE: probe: [a-z]*' $P/$n.$label.err | head -1)
      dbg=$(grep -c 'debug-alloc' $P/$n.$label.err)
      echo "code=$code $bal dbg=$dbg $ice | $(tr '\n' '|' < $P/$n.$label.out)" >> $out
    done
    echo "== $label $n T$th"; sort $out | uniq -c
  done
done
```

