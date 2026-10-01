// The playground page: an editor, a Run button that posts the program to /api/compile, and a
// Web Worker (worker.js) that runs the returned WebAssembly module and streams its output.

const EXAMPLES = {
  "Hello": `function main() {
  const who = "Velt";
  console.log(\`Hello, \${who}!\`);
}
`,
  "Loops and functions": `function fib(n: i64): i64 {
  return n < 2 ? n : fib(n - 1) + fib(n - 2);
}

function main() {
  for (let i = 0; i <= 10; i++) {
    console.log(\`fib(\${i}) = \${fib(i)}\`);
  }
  const start = performance.now();
  const f = fib(30);
  console.log("fib(30) =", f, "in", Math.round(performance.now() - start), "ms");
}
`,
  "Classes": `class Animal {
  name: string;

  constructor(name: string) {
    this.name = name;
  }

  speak(): string {
    return \`\${this.name} makes a sound\`;
  }
}

class Dog extends Animal {
  constructor(name: string) {
    super(name);
  }

  override speak(): string {
    return \`\${this.name} barks\`;
  }
}

function main() {
  const animals: Animal[] = [new Animal("Generic"), new Dog("Rex")];
  for (const a of animals) {
    console.log(a.speak());
  }
}
`,
  "Arrays and closures": `function main() {
  const xs = [5, 3, 8, 1, 9, 2];
  const doubled = xs.map((x) => x * 2);
  const big = xs.filter((x) => x > 4);
  const total = xs.reduce((a, b) => a + b, 0);
  console.log(doubled, big, total);

  const counts = new Map<string, i64>();
  for (const w of "the quick brown fox jumps over the lazy dog the end".split(" ")) {
    counts.set(w, (counts.get(w) ?? 0) + 1);
  }
  console.log("the:", counts.get("the") ?? 0);
}
`,
  "Async": `async function delayed(ms: i64, v: i64): Promise<i64> {
  await sleep(ms);
  return v;
}

async function main() {
  const start = performance.now();
  const [a, b] = await Promise.all([delayed(200, 1), delayed(100, 2)]);
  console.log(a, b, "after", Math.round(performance.now() - start), "ms");
  const h = spawn(delayed(50, 42));
  console.log("spawned:", await h);
}
`,
  "JSON": `function main() {
  const v = JSON.parseValue(\`{"name":"velt","tags":["fast","typed"],"stars":3}\`);
  console.log(v.get("name")?.asString() ?? "?");
  console.log(v.get("tags")?.len() ?? 0, "tags");
  console.log(JSON.stringify(v));
  console.log(JSON.stringify([1, 2, 3]));
}
`,
};

const $ = (id) => document.getElementById(id);
const source = $("source");
const output = $("output");
const status = $("status");
let worker = null;

function setup() {
  for (const name of Object.keys(EXAMPLES)) {
    $("example").append(new Option(name, name));
  }
  $("example").addEventListener("change", () => {
    source.value = EXAMPLES[$("example").value];
  });
  source.value = EXAMPLES.Hello;
  $("run").addEventListener("click", run);
  $("stop").addEventListener("click", () => finish("Stopped."));
  source.addEventListener("keydown", onKey);
}

function onKey(e) {
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
    e.preventDefault();
    run();
  } else if (e.key === "Tab") {
    e.preventDefault();
    source.setRangeText("  ", source.selectionStart, source.selectionEnd, "end");
  }
}

function append(text, isError) {
  const span = document.createElement("span");
  if (isError) span.className = "err";
  span.textContent = text;
  output.append(span);
  output.scrollTop = output.scrollHeight;
}

async function run() {
  finish(null);
  output.textContent = "";
  status.textContent = "Compiling…";
  $("run").disabled = true;
  const started = performance.now();
  const release = $("release").checked ? "?release=1" : "";
  let resp;
  try {
    resp = await fetch(`api/compile${release}`, { method: "POST", body: source.value });
  } catch (e) {
    $("run").disabled = false;
    status.textContent = `Cannot reach the playground server: ${e}`;
    return;
  }
  const compileMs = Math.round(performance.now() - started);
  if (!resp.ok) {
    $("run").disabled = false;
    status.textContent = `Compile error (${compileMs} ms)`;
    append(await resp.text(), true);
    return;
  }
  const module = await resp.arrayBuffer();
  status.textContent = `Compiled in ${compileMs} ms · running…`;
  start(module, compileMs);
}

function start(module, compileMs) {
  const decoders = { 1: new TextDecoder(), 2: new TextDecoder() };
  const runStart = performance.now();
  worker = new Worker("worker.js", { type: "module" });
  $("stop").disabled = false;
  worker.onmessage = ({ data }) => {
    if (data.kind === "output") {
      append(decoders[data.stream].decode(data.bytes, { stream: true }), data.stream === 2);
    } else if (data.kind === "exit") {
      const runMs = Math.round(performance.now() - runStart);
      finish(`Compiled in ${compileMs} ms · ran in ${runMs} ms · exit code ${data.code}`);
    } else {
      finish(`Failed: ${data.message}`);
    }
  };
  worker.postMessage({ module }, [module]);
}

function finish(message) {
  if (worker) {
    worker.terminate();
    worker = null;
  }
  $("run").disabled = false;
  $("stop").disabled = true;
  if (message) status.textContent = message;
}

setup();
