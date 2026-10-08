// The Node side of the comparison: the same two actors and the same wire endpoint, served by
// @sigx/actors itself (`createAppHandler` on node:http, memoryStorage), one process.
//
//   SIGX_ACTORS=/path/to/actors/packages/actors node --conditions=production server.mjs [port]
//
// SIGX_ACTORS points at a BUILT checkout of signalxjs/actors (`pnpm install && pnpm build`);
// the production bundles are loaded, as `pnpm bench` does.
import { createServer } from 'node:http';
import { pathToFileURL } from 'node:url';
import { join } from 'node:path';

const root = process.env.SIGX_ACTORS ?? join(import.meta.dirname, '../../../../../actors/packages/actors');
const load = (entry) => import(pathToFileURL(join(root, 'dist', `${entry}.prod.js`)).href);
const { defineActorApp, memoryStorage } = await load('host');
const { createAppHandler } = await load('node');

const app = defineActorApp({
    storage: memoryStorage(),
    defaults: { devSerializeChecks: false }
});

// examples/counter's Counter: increment saves before it answers.
const Counter = app.defineActor({
    type: 'Counter',
    allowAnonymous: true,
    state: () => ({ count: 0, lastVisit: 0 }),
    methods: (ctx) => ({
        async increment(by) {
            ctx.state.count += by;
            ctx.state.lastVisit = Date.now();
            await ctx.save();
            return ctx.state.count;
        },
        async current() {
            return { count: ctx.state.count, lastVisit: ctx.state.lastVisit };
        }
    })
});

// benchmarks/src/actors.ts's Tiny: noop touches nothing.
const Tiny = app.defineActor({
    type: 'Tiny',
    allowAnonymous: true,
    state: () => ({ count: 0 }),
    methods: (ctx) => ({
        noop() {
            return 0;
        },
        bump() {
            ctx.state.count += 1;
            return ctx.state.count;
        }
    })
});

await app.withActors([Counter, Tiny]).start();
// No browser in the loop: the same-origin policy (on by default) would refuse curl and wrk.
const handler = createAppHandler(app, { origin: false });
const port = Number(process.argv[2] ?? process.env.PORT ?? 5299);
const server = createServer((req, res) => void handler(req, res, () => void res.writeHead(404).end('not found')));
server.keepAliveTimeout = 60_000;
server.listen(port, '127.0.0.1', () => console.log(`sigx actors (node ${process.version}) on http://127.0.0.1:${port}/_sigx/actor/`));
