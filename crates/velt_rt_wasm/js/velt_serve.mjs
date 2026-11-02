// The host-neutral server core of the JS glue for `velt build --host <package>` builds
// (docs/internals/contracts/workers.md). The build writes it next to the module
// (`program.wasm`), with velt_web.mjs (whose `loadVelt` instantiates and drives the program),
// `velt_modules.mjs` (the package host modules the program imports) and the host package's
// entry, a thin adapter from the platform's way of handing over requests to this core:
//
//   import module from "./program.wasm";            // however the platform loads a module
//   import { createServer } from "./velt_serve.mjs";
//   import { modules } from "./velt_modules.mjs";
//   const server = createServer(module, { modules });
//   export default { fetch: (request, env, ctx) =>
//     server.serve(request, { context: { env, ctx }, waitUntil: (p) => ctx.waitUntil(p) }) };
//
// The core knows no platform. The module is instantiated once, on the first request, and `main`
// runs once (`velt_start`); the instance then serves every request. A request goes into a queue
// the program takes from with the async host function `velt:worker`.`accept()`, as a server
// accepts connections, so `main` may await anything before it starts accepting. The program
// reads the request and answers it through the other `velt:worker` functions (`workerImports`
// below).
//
// Some platforms (Cloudflare's Workers is one) tie I/O (a request's body, a `Response`, a
// stream, a timer, a fetch) to the request whose handler made it, cancel a request whose handler
// waits on nothing of its own, and end a request's context, with what it started, once its
// response and the promises given to its `waitUntil` are done. So each request has a pump: an
// async loop in its own handler that keeps a timer pending while it waits, and does the
// request's I/O when the program asks. The program's waits on a request are settled from that
// pump (`Deferred` of velt_web.mjs), so its turns run in the request's context. A pump lives on
// (with the adapter's `waitUntil`) after its answer while host promises a turn in its context
// started are pending, or while it holds the program's timer, which moves to the newest
// request's pump. On a platform without such contexts the pumps cost a promise per request.
//
// A panic, a trap, `process.exit`, an uncaught error, a host function that throws, or a `main`
// that ends after it accepted requests ends the instance (an exception that unwinds through the
// module leaves it unusable): the requests in flight get a 500, the end is logged, and the next
// request starts a new instance. A program that ends with 0 before accepting keeps its instance;
// its requests get a 500 saying it does not accept requests, logged once.

import { loadVelt, HostRefs, Deferred } from "./velt_web.mjs";

/** How often a pump with nothing to do wakes: a timer of its own, so its request never hangs. */
const KEEPALIVE_MS = 10_000;

/**
 * A server for a compiled Velt program, which the host package's entry hands requests to.
 * @param {WebAssembly.Module} module the program
 * @param {{ modules?: Record<string, (host: object) => object>,
 *           write?: (stream: number, bytes: Uint8Array) => void }} options the host modules a
 *   package provides, by import module (each a function making the module's functions, given
 *   `host`, whose properties are those of the `context` the adapter gave the request whose
 *   context runs the program now), and the sink of the program's output (default: console, a
 *   line at a time)
 * @returns {{ serve(request: Request, platform?: { context?: object,
 *             waitUntil?: (p: Promise<void>) => void, remoteAddr?: string }): Promise<Response> }}
 *   `serve` answers a request; `platform.context` is what host modules see for it,
 *   `platform.waitUntil` keeps its handler's context alive after the answer (platforms that end
 *   it with the answer), and `platform.remoteAddr` is the client's address if the platform says
 *   (a host name or `host:port`)
 */
export function createServer(module, options = {}) {
  let program = null;
  return {
    serve(request, platform = {}) {
      if (program === null || program.failed) program = new Program(module, options);
      return program.serve(request, platform);
    },
  };
}

const EMPTY = new Uint8Array(0);

/** A text answer of the glue's own. */
const answer = (status, text) =>
  new Response(text, { status, headers: { "content-type": "text/plain; charset=utf-8" } });

/** One instance of the program and the requests it serves. */
class Program {
  constructor(module, options) {
    this.module = module;
    this.options = options;
    this.ready = null;
    this.started = false;
    this.queue = []; // requests no `accept` took yet
    this.waiters = []; // `Deferred`s of the program's pending `accept()` calls
    this.requests = new Set(); // requests not answered yet, or whose body is still written
    this.pumps = []; // the pumps running, oldest first
    this.current = null; // the request in whose context the program runs now
    this.timer = null; // { delay, at, fire, holder }: the program's wake-up
    this.failed = false;
    this.ended = false;
    this.accepted = false;
    this.toldNeverAccepts = false;
  }

  /** Instantiate the module and run `main`, in `first`'s context. */
  async start(first) {
    // The context of the request whose context runs the program now, as the adapter gave it.
    const host = new Proxy(Object.create(null), {
      get: (_, key) => this.current?.context?.[key],
      has: (_, key) => key in (this.current?.context ?? {}),
    });
    // Host modules are made in the first request's context, which they may read at once.
    this.current = first;
    try {
      const imports = { "velt:worker": this.workerImports() };
      for (const [name, make] of Object.entries(this.options.modules ?? {})) {
        // Defined, not assigned: no module name reaches a setter (`__proto__`).
        Object.defineProperty(imports, name, { value: make(host), enumerable: true });
      }
      const rt = await loadVelt(this.module, {
        write: this.options.write,
        imports,
        refs: new RequestRefs(),
        setTimer: (ms, fire) => this.setTimer(ms, fire),
        owner: () => this.current?.pin(),
        context: () => this.current ?? undefined,
        within: (req, job) => req.run(job),
        resuming: (owner) => (this.current = owner ?? this.current),
        released: (owner) => owner?.unpin(),
        // Told at once: a reaction to `done` would run in the first request's context.
        ended: (code, e) => (e === undefined ? this.end(code) : this.fail(`stopped: ${e?.message ?? e}`)),
      });
      this.current = first;
      rt.enter(() => rt.exports.velt_start());
    } catch (e) {
      this.fail(`did not start: ${e?.message ?? e}`);
    }
  }

  async serve(request, platform) {
    const req = new WorkerRequest(request, this, platform.context, peer(platform.remoteAddr));
    const pumping = req.pump();
    // Awaited only while it runs: a promise of the first request's context.
    if (!this.started) await (this.ready ??= this.start(req).then(() => (this.started = true)));
    if (this.failed) {
      req.abandon(null);
    } else if (this.ended) {
      this.neverAccepted(req);
    } else {
      this.requests.add(req);
      this.queue.push(req);
      this.schedule();
      this.armTimer();
    }
    const response = await req.response;
    if (!req.finished()) platform.waitUntil?.(pumping);
    return response;
  }

  /** The `velt:worker` host module (docs/internals/contracts/workers.md). */
  workerImports() {
    return {
      accept: () => {
        const waiter = new Deferred((req) => {
          // Handed over, then dropped before the program took it: the next `accept` gets it.
          this.queue.unshift(req);
          this.schedule();
        });
        this.waiters.push(waiter);
        this.schedule();
        return waiter;
      },
      method: (req) => req.method,
      url: (req) => req.url,
      headers: (req) => req.headers,
      remoteAddr: (req) => req.remoteAddr,
      hasBody: (req) => req.body !== null,
      read: (req) => req.read(),
      respond: (req, status, headers, body) => req.respond(status, headers, body),
      respondStream: (req, status, headers) => req.respondStream(status, headers),
      write: (stream, chunk) => stream.write(chunk),
      close: (stream) => stream.close(),
    };
  }

  /**
   * When a request waits and the program waits in `accept()`: hand the request over from its own
   * pump, so the program takes it in the request's context.
   */
  schedule() {
    const req = this.queue[0];
    if (req === undefined || req.handing || !this.waiters.some((w) => w.pending)) return;
    req.handing = true;
    req.post(() => {
      req.handing = false;
      if (this.queue[0] !== req) return this.schedule();
      this.waiters = this.waiters.filter((w) => w.pending);
      const waiter = this.waiters.shift();
      if (waiter !== undefined && req.settle(waiter, true, req)) {
        this.queue.shift();
        this.accepted = true;
      }
      this.schedule();
    });
  }

  /** The program asks to be woken in `delay` ms (negative: never). */
  setTimer(delay, fire) {
    const at = performance.now() + delay;
    const timer = this.timer;
    // Every turn asks again: the same deadline keeps the timeout armed.
    if (delay >= 0 && timer !== null && Math.abs(timer.at - at) < 1) return;
    if (timer !== null) disarm(timer);
    this.timer = delay < 0 ? null : { at, fire, holder: null, handle: null };
    this.armTimer();
  }

  /**
   * Arm the program's timer in the newest request not answered yet, whose context lasts while it
   * waits (else in the newest pump, kept by `waitUntil`); called when that may have changed.
   */
  armTimer() {
    const timer = this.timer;
    const holder = this.pumps.findLast((p) => !p.answered) ?? this.pumps.at(-1);
    if (timer === null || holder === undefined || timer.holder === holder) return;
    disarm(timer);
    timer.holder = holder;
    holder.post(() => {
      if (this.timer !== timer || timer.holder !== holder) return;
      timer.handle = setTimeout(() => {
        if (this.timer !== timer || timer.holder !== holder) return;
        this.timer = null;
        this.current = holder;
        timer.fire();
        holder.poke();
      }, Math.max(0, timer.at - performance.now()));
    });
  }

  /** The program ended with `code`. */
  end(code) {
    if (code !== 0 || this.accepted) {
      this.fail(`ended with exit code ${code}`);
      return;
    }
    this.ended = true;
    for (const req of this.queue.splice(0)) this.neverAccepted(req);
    for (const req of [...this.requests]) req.abandon("the program ended without answering it");
    for (const pump of this.pumps) pump.poke();
  }

  /** The instance cannot go on (`why`): answer what is in flight, and start anew next time. */
  fail(why) {
    if (this.failed) return;
    this.failed = true;
    console.error(
      `velt: the program ${why}; its ${this.requests.size} request(s) in flight get a ` +
        "500, and the next request starts a new instance",
    );
    this.queue = [];
    this.timer = null;
    for (const req of [...this.requests]) req.abandon(null);
    for (const pump of this.pumps) pump.poke();
  }

  neverAccepted(req) {
    if (!this.toldNeverAccepts) {
      this.toldNeverAccepts = true;
      console.error("velt: the program does not accept requests; they get a 500");
    }
    this.requests.delete(req);
    req.answered = true;
    req.post(() => req.finish(answer(500, "the program does not accept requests")));
  }
}

/**
 * A request in the program's hands: what it reads, the answer it gives, and the pump doing its
 * I/O in its own handler.
 */
class WorkerRequest {
  constructor(request, program, context, remoteAddr) {
    this.program = program;
    this.context = context;
    this.remoteAddr = remoteAddr;
    this.method = request.method;
    this.url = request.url;
    this.headers = [...request.headers].map(([k, v]) => `${k}: ${v}`).join("\n");
    this.body = request.body;
    this.reader = null;
    this.stream = null;
    this.jobs = [];
    this.inflight = 0; // jobs whose I/O goes on
    this.wake = null;
    this.pins = 0; // host promises started in this request's context, pending
    this.handing = false;
    this.answered = false; // the program answered (or will not)
    this.done = false; // the answer is complete
    this.response = new Promise((resolve) => (this.resolveResponse = resolve));
    // The client went away (with `request.signal`): nothing waits for the answer.
    request.signal?.addEventListener?.("abort", () => this.gone());
  }

  /** The client went away before the answer: the program no longer gets it from `accept()`. */
  gone() {
    const program = this.program;
    const queued = program.queue.indexOf(this);
    if (this.answered || queued < 0) return;
    program.queue.splice(queued, 1);
    this.handing = false;
    this.answered = true;
    this.post(() => this.finish(answer(499, "the client went away")));
    program.schedule();
    program.armTimer();
  }

  /** Whether the pump may stop: answered, and nothing of the program's lives in its context. */
  finished() {
    const program = this.program;
    const held = program.timer?.holder === this;
    const pinned = this.pins > 0 && !program.failed && !program.ended;
    return this.done && this.jobs.length === 0 && this.inflight === 0 && !held && !pinned;
  }

  /** Runs the request's I/O as the program asks for it, in the request's own handler. */
  async pump() {
    const pumps = this.program.pumps;
    pumps.push(this);
    while (!this.finished()) {
      const job = this.jobs.shift();
      if (job === undefined) {
        await new Promise((resolve) => {
          const keepalive = setTimeout(() => this.poke(), KEEPALIVE_MS);
          this.wake = () => {
            clearTimeout(keepalive);
            this.wake = null;
            resolve();
          };
        });
        continue;
      }
      // A job's I/O (a read waiting for the client, a write under backpressure) goes on while
      // the pump runs the next jobs: a timer it holds, an answer, a read of another context's
      // object. The pump lives while one is in flight.
      this.program.current = this;
      const pending = job();
      if (typeof pending?.then === "function") {
        this.inflight++;
        const done = () => {
          this.inflight--;
          this.poke();
        };
        pending.then(done, done);
      }
    }
    pumps.splice(pumps.indexOf(this), 1);
    this.pumpEnded = true;
    this.reader?.cancel().catch(() => {});
  }

  /**
   * Run `job` (async) in this request's handler and give its promise: I/O of an object this
   * request's context made (a `fetch` response), wherever the program asks for it. Once the pump
   * has ended, the context is gone and `job` runs where it is asked.
   */
  run(job) {
    if (this.pumpEnded) return job();
    return new Promise((resolve, reject) => this.post(() => job().then(resolve, reject)));
  }

  /** Have the pump run `job`. */
  post(job) {
    this.jobs.push(job);
    this.poke();
  }

  /** Have the pump look at its work again. */
  poke() {
    this.wake?.();
  }

  pin() {
    this.pins++;
    return this;
  }

  /** Settle `deferred`, running the program's turn in this request's context. */
  settle(deferred, ok, value) {
    this.program.current = this;
    return ok ? deferred.resolve(value) : deferred.reject(value);
  }

  unpin() {
    this.pins--;
    this.poke();
  }

  /** The next piece of the body; empty at its end. */
  read() {
    if (this.body === null || this.done) return EMPTY;
    const piece = new Deferred();
    this.post(async () => {
      if (this.done) return void this.settle(piece, true, EMPTY);
      try {
        this.reader ??= this.body.getReader();
        for (;;) {
          const { done, value } = await this.reader.read();
          if (done || value.length > 0) return void this.settle(piece, true, done ? EMPTY : value);
        }
      } catch (e) {
        this.settle(piece, false, e);
      }
    });
    return piece;
  }

  respond(status, headers, body) {
    if (!this.claim()) return;
    this.post(() =>
      this.finish(this.made(() => new Response(body.length === 0 ? null : body, init(status, headers)))),
    );
  }

  respondStream(status, headers) {
    if (!this.claim()) return new ResponseStream(this, true);
    const stream = new ResponseStream(this);
    this.post(() => {
      const { readable, writable } = new TransformStream();
      stream.writer = writable.getWriter();
      this.resolveResponse(this.made(() => new Response(readable, init(status, headers))));
    });
    return stream;
  }

  /**
   * The program answers once: whether this is the first answer. A second one is the program's
   * bug, logged and ignored, so other requests in flight are not affected.
   */
  claim() {
    if (this.answered) {
      console.error(`velt: ${this.method} ${this.url} was answered already; the second answer is ignored`);
      return false;
    }
    this.answered = true;
    this.program.armTimer();
    return true;
  }

  /** `make()`'s response, or a 500 when the program's status or headers make none. */
  made(make) {
    try {
      return make();
    } catch (e) {
      console.error(`velt: the program's answer to ${this.method} ${this.url} is invalid: ${e.message}`);
      return answer(500, "Internal Server Error");
    }
  }

  /** Answer with `response`, whose body is complete. */
  finish(response) {
    this.resolveResponse(response);
    this.ended();
  }

  /** The answer is complete. */
  ended() {
    this.done = true;
    this.program.requests.delete(this);
  }

  /** The program will not answer, or not finish its answer: a 500 (`why` logged, unless null). */
  abandon(why) {
    if (this.answered) return this.stream?.abort();
    this.answered = true;
    this.program.armTimer();
    if (why !== null) console.error(`velt: ${this.method} ${this.url}: ${why}`);
    this.post(() => this.finish(answer(500, "Internal Server Error")));
  }

  /** The program dropped its last `HostRef` to the request. */
  dropped() {
    if (!this.answered && !this.program.failed) {
      this.abandon("the program dropped the request without answering it");
    }
  }
}

/** The body of a response the program writes while it is sent. */
class ResponseStream {
  /** `ignored`: the stream of a second answer, whose writes fail (`claim`). */
  constructor(req, ignored = false) {
    this.req = req;
    this.writer = null; // made by the pump
    this.closed = ignored;
    if (!ignored) req.stream = this;
  }

  write(chunk) {
    if (this.closed) return Promise.reject(new Error("the response's body is closed"));
    const written = new Deferred();
    this.req.post(async () => {
      try {
        await this.writer.write(chunk);
        this.req.settle(written, true);
      } catch (e) {
        this.req.settle(written, false, e);
      }
    });
    return written;
  }

  close() {
    if (this.closed) return Promise.reject(new Error("the response's body is closed"));
    this.closed = true;
    const closed = new Deferred();
    this.req.post(async () => {
      this.req.ended();
      try {
        await this.writer.close();
        this.req.settle(closed, true);
      } catch (e) {
        this.req.settle(closed, false, e);
      }
    });
    return closed;
  }

  /** The body ends with an error (the program dropped the stream, or stopped). */
  abort() {
    if (this.closed) return;
    this.closed = true;
    this.req.post(async () => {
      await this.writer?.abort(new Error("the program stopped writing the response")).catch(() => {});
      this.req.ended();
    });
  }

  dropped() {
    this.abort();
  }
}

/**
 * The client's address as `velt:worker`.`remoteAddr` gives it, from the address the adapter
 * passed (`platform.remoteAddr`, a host name or `host:port`): `host:port`, `[v6]:port` for an IPv6
 * address, port 0 when the adapter gave none; empty without one.
 */
function peer(addr) {
  if (typeof addr !== "string" || addr === "") return "";
  if (addr.startsWith("[")) return addr.includes("]:") ? addr : `${addr}:0`;
  const colons = addr.split(":").length - 1;
  if (colons > 1) return `[${addr}]:0`;
  return colons === 1 ? addr : `${addr}:0`;
}

/** Take `timer` from its pump: its timeout is cleared there, in the context that made it. */
function disarm(timer) {
  const { holder, handle } = timer;
  timer.holder = null;
  timer.handle = null;
  if (holder === null) return;
  if (handle !== null) holder.post(() => clearTimeout(handle));
  holder.poke();
}

/** A `Response`'s init from the program's status and `name: value` lines. */
function init(status, headers) {
  const fields = new Headers();
  for (const line of headers.split("\n")) {
    if (line === "") continue;
    const colon = line.indexOf(":");
    if (colon <= 0) throw new TypeError(`the header line ${JSON.stringify(line)} has no name`);
    fields.append(line.slice(0, colon), line.slice(colon + 1).trim());
  }
  return { status, headers: fields };
}

/**
 * The table of `HostRef`s, telling a request or a response stream when the program dropped its
 * last `HostRef` to it.
 */
class RequestRefs extends HostRefs {
  counts = new Map();
  add(value) {
    const slot = super.add(value);
    if (watched(value)) this.counts.set(value, (this.counts.get(value) ?? 0) + 1);
    return slot;
  }
  release(slot) {
    const value = this.values.get(slot);
    super.release(slot);
    const count = this.counts.get(value);
    if (count === undefined) return;
    if (count > 1) {
      this.counts.set(value, count - 1);
    } else {
      this.counts.delete(value);
      value.dropped();
    }
  }
}

const watched = (value) => value instanceof WorkerRequest || value instanceof ResponseStream;
