// JS glue for Velt programs built with `velt build --host workers`: a Cloudflare Worker
// (docs/internals/contracts/workers.md, docs/internals/design/cloudflare-workers.md). The build
// writes it next to the module, with velt_web.mjs (whose `loadVelt` instantiates and drives the
// program), the package host modules the program imports, and `worker.mjs`, the Worker's entry:
//
//   import module from "./app.wasm";
//   import { createWorker } from "./velt_worker.mjs";
//   import kv from "./hosts/kv/kv.mjs";
//   export default createWorker(module, { modules: { kv } });
//
// The module is instantiated once per isolate, on the first request, and `main` runs once
// (`velt_start`); the instance then serves every request. A request goes into a queue the program
// takes from with the async host function `velt:worker`.`accept()`, as a server accepts
// connections, so `main` may await anything before it starts accepting. The program reads the
// request and answers it through the other `velt:worker` functions (`workerImports` below). While
// the program has work that outlives a response (a timer, a host promise other than `accept`),
// `ctx.waitUntil` keeps the request's context alive until it has none.
//
// Workers allow a request's I/O (its body, the stream of its response) only in that request's
// context, and run a promise's reactions in the context that made it. So each request has a pump,
// an async loop in its own handler, that does its I/O when the program asks, and the program's
// waits on a request are settled from the request's own handler (`Deferred` of velt_web.mjs).
//
// A panic, a trap, `process.exit` with another code than 0, an uncaught error or a host function
// that throws ends the instance (they unwind through the module, which leaves it unusable): the
// requests in flight get a 500, the end is logged, and the next request starts a new instance. A
// program that ended with 0, or never accepts, keeps its instance; its requests get a 500 saying
// so, logged once.

import { loadVelt, HostRefs, Deferred } from "./velt_web.mjs";

/**
 * A Worker's handlers for a compiled Velt program.
 * @param {WebAssembly.Module} module the program
 * @param {{ modules?: Record<string, (host: { env: object, ctx: object }) => object>,
 *           write?: (stream: number, bytes: Uint8Array) => void }} options the host modules a
 *   package provides, by import module (each a function making the module's functions, given
 *   the Worker's `env` and the `ctx` of the latest request), and the sink of the program's output
 *   (default: console, a line at a time)
 */
export function createWorker(module, options = {}) {
  let program = null;
  return {
    fetch(request, env, ctx) {
      if (program === null || program.failed) program = new Program(module, options, env, ctx);
      return program.serve(request, env, ctx);
    },
  };
}

const EMPTY = new Uint8Array(0);

/** Text answers of the glue's own. */
const answer = (status, text) =>
  new Response(text, { status, headers: { "content-type": "text/plain; charset=utf-8" } });

/** One instance of the program and the requests it serves. */
class Program {
  constructor(module, options, env, ctx) {
    this.queue = []; // requests no `accept` took yet
    this.waiters = []; // `Deferred`s of the program's pending `accept()` calls
    this.requests = new Set(); // requests not answered yet
    this.idle = []; // what `untilIdle` waits on
    this.current = { env, ctx };
    this.failed = false;
    this.ended = false;
    this.toldNeverAccepts = false;
    const current = this.current;
    const host = {
      get env() {
        return current.env;
      },
      get ctx() {
        return current.ctx;
      },
    };
    this.ready = (async () => {
      const imports = { "velt:worker": this.workerImports() };
      for (const [name, make] of Object.entries(options.modules ?? {})) imports[name] = make(host);
      const rt = await loadVelt(module, {
        write: options.write,
        imports,
        refs: new RequestRefs(),
        turned: () => this.turned(),
      });
      rt.done.then(
        (code) => this.end(code),
        (e) => this.fail(`stopped: ${e?.message ?? e}`),
      );
      this.rt = rt;
      rt.enter(() => rt.exports.velt_start());
    })().catch((e) => this.fail(`did not start: ${e?.message ?? e}`));
  }

  async serve(request, env, ctx) {
    this.current.env = env;
    this.current.ctx = ctx;
    const req = new WorkerRequest(request, this);
    await this.ready;
    if (this.failed) return answer(500, "Internal Server Error");
    if (this.ended) return this.neverAccepted(req);
    this.requests.add(req);
    const pumping = req.pump();
    this.queue.push(req);
    this.dispatch();
    const response = await req.response;
    // A response whose body the program still writes: its pump goes on.
    if (!req.done) ctx.waitUntil(pumping);
    if (this.liveWork()) ctx.waitUntil(this.untilIdle());
    return response;
  }

  /**
   * Hand queued requests to the program's pending `accept()` calls. Called from a request's
   * handler, the program takes it in that request's context.
   */
  dispatch() {
    this.waiters = this.waiters.filter((w) => w.pending);
    while (this.queue.length > 0 && this.waiters.length > 0) {
      if (this.waiters.shift().resolve(this.queue[0])) this.queue.shift();
    }
  }

  /** The `velt:worker` host module (docs/internals/contracts/workers.md). */
  workerImports() {
    return {
      accept: () => {
        const waiter = new Deferred();
        this.waiters.push(waiter);
        // A request already waits: handed over once the program's turn is over.
        if (this.queue.length > 0) queueMicrotask(() => this.dispatch());
        return waiter;
      },
      method: (req) => req.method,
      url: (req) => req.url,
      headers: (req) => req.headers,
      read: (req) => req.read(),
      respond: (req, status, headers, body) => req.respond(status, headers, body),
      respondStream: (req, status, headers) => req.respondStream(status, headers),
      write: (stream, chunk) => stream.write(chunk),
      close: (stream) => stream.close(),
    };
  }

  /** Whether something other than a request can wake the program: a timer, a host promise. */
  liveWork() {
    const rt = this.rt;
    if (rt === undefined || rt.ended) return false;
    const accepting = this.waiters.filter((w) => w.pending).length;
    return rt.timerArmed || rt.promises.size > accepting;
  }

  /** Settles once the program has no work left that a request does not start. */
  untilIdle() {
    return new Promise((resolve) => this.idle.push(resolve));
  }

  turned() {
    if (this.liveWork()) return;
    for (const resolve of this.idle.splice(0)) resolve();
  }

  /** The program ended with `code`: 0 keeps the instance, another code discards it. */
  end(code) {
    if (code !== 0) {
      this.fail(`ended with exit code ${code}`);
      return;
    }
    this.ended = true;
    for (const req of this.queue.splice(0)) this.neverAccepted(req);
    this.settleAll("the program ended without answering it");
    this.turned();
  }

  /** The instance cannot go on (`why`): answer what is in flight, and start anew next time. */
  fail(why) {
    if (this.failed) return;
    this.failed = true;
    console.error(
      `velt: the Worker's program ${why}; its ${this.requests.size} request(s) in flight get a ` +
        "500, and the next request starts a new instance",
    );
    this.queue = [];
    this.settleAll(null);
    for (const resolve of this.idle.splice(0)) resolve();
  }

  /** End every request in flight: a 500 for one not answered (`why` logged, unless null). */
  settleAll(why) {
    for (const req of [...this.requests]) req.abandon(why);
  }

  neverAccepted(req) {
    if (!this.toldNeverAccepts) {
      this.toldNeverAccepts = true;
      console.error("velt: the Worker's program does not accept requests; they get a 500");
    }
    req.finish(answer(500, "the program does not accept requests"));
    return req.response;
  }
}

/**
 * A request in the program's hands: what it reads, and the answer it gives. Its I/O runs in
 * `pump`, in the request's own handler.
 */
class WorkerRequest {
  constructor(request, program) {
    this.program = program;
    this.method = request.method;
    this.url = request.url;
    this.headers = [...request.headers].map(([k, v]) => `${k}: ${v}`).join("\n");
    this.body = request.body;
    this.reader = null;
    this.jobs = [];
    this.wake = null;
    this.done = false; // answered, and the response's body written
    this.answered = false;
    this.response = new Promise((resolve) => (this.resolveResponse = resolve));
  }

  /** Runs the request's I/O as the program asks for it, until the request is done. */
  async pump() {
    while (!this.done) {
      const job = this.jobs.shift() ?? (await new Promise((r) => (this.wake = r)));
      if (job !== null) await job();
    }
    this.reader?.cancel().catch(() => {});
  }

  /** Have the pump run `job` (null: just look at `done` again). */
  post(job) {
    if (this.wake !== null) {
      const wake = this.wake;
      this.wake = null;
      wake(job);
    } else {
      this.jobs.push(job);
    }
  }

  /** The next piece of the body; empty at its end. */
  read() {
    if (this.body === null || this.done) return EMPTY;
    const piece = new Deferred();
    this.post(async () => {
      try {
        this.reader ??= this.body.getReader();
        for (;;) {
          const { done, value } = await this.reader.read();
          if (done || value.length > 0) return void piece.resolve(done ? EMPTY : value);
        }
      } catch (e) {
        piece.reject(e);
      }
    });
    return piece;
  }

  respond(status, headers, body) {
    this.claim();
    this.finish(this.made(() => new Response(body.length === 0 ? null : body, init(status, headers))));
  }

  respondStream(status, headers) {
    this.claim();
    const stream = new ResponseStream(this);
    this.post(() => {
      const { readable, writable } = new TransformStream();
      stream.writer = writable.getWriter();
      this.resolveResponse(this.made(() => new Response(readable, init(status, headers))));
    });
    return stream;
  }

  /** The program answers once; a second answer is its bug, which stops it. */
  claim() {
    if (this.answered) throw new Error(`velt: ${this.method} ${this.url} was answered already`);
    this.answered = true;
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
    this.answered = true;
    this.resolveResponse(response);
    this.ended();
  }

  /** The answer is complete: the pump stops. */
  ended() {
    this.done = true;
    this.program.requests.delete(this);
    this.post(null);
  }

  /** The program will not answer, or finish its answer: a 500 (and `why` logged, unless null). */
  abandon(why) {
    if (this.answered) return this.stream?.abort();
    if (why !== null) console.error(`velt: ${this.method} ${this.url}: ${why}`);
    this.finish(answer(500, "Internal Server Error"));
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
  constructor(req) {
    this.req = req;
    this.writer = null; // made by the pump
    this.closed = false;
    req.stream = this;
  }

  write(chunk) {
    if (this.closed) return Promise.reject(new Error("the response's body is closed"));
    const written = new Deferred();
    this.req.post(async () => {
      try {
        await this.writer.write(chunk);
        written.resolve();
      } catch (e) {
        written.reject(e);
      }
    });
    return written;
  }

  close() {
    if (this.closed) return Promise.reject(new Error("the response's body is closed"));
    const closed = new Deferred();
    this.closed = true;
    this.req.post(async () => {
      try {
        await this.writer.close();
        closed.resolve();
      } catch (e) {
        closed.reject(e);
      }
      this.req.ended();
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
    if (typeof value?.dropped === "function") this.counts.set(value, (this.counts.get(value) ?? 0) + 1);
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
