// RSS per live activation in @sigx/actors, on the same metric `actors bench` reports for Velt
// (the sigx harness's mem/per-actor-footprint reports V8 heapUsed instead). Activates N `Tiny`
// actors with 64 concurrent callers through host.dispatch and reads the RSS delta.
//   SIGX_ACTORS=…/actors/packages/actors node --conditions=production --expose-gc mem.mjs [N]
import { pathToFileURL } from 'node:url';
import { join } from 'node:path';

const root = process.env.SIGX_ACTORS ?? join(import.meta.dirname, '../../../../../actors/packages/actors');
const load = (entry) => import(pathToFileURL(join(root, 'dist', `${entry}.prod.js`)).href);
const { defineActorApp, memoryStorage } = await load('host');

const app = defineActorApp({
    storage: memoryStorage(),
    defaults: { idleAfterMs: 3_600_000, sweepIntervalMs: 3_600_000, devSerializeChecks: false }
});
const Tiny = app.defineActor({
    type: 'Tiny',
    allowAnonymous: true,
    state: () => ({ count: 0 }),
    methods: () => ({ noop: () => 0 })
});
const host = await app.withActors([Tiny]).start();

const settle = () => { for (let i = 0; i < 4; i++) globalThis.gc?.(); };
for (const n of (process.argv[2] ? [Number(process.argv[2])] : [20_000, 100_000])) {
    settle();
    const before = process.memoryUsage();
    const t0 = performance.now();
    const per = Math.trunc(n / 64);
    await Promise.all(Array.from({ length: 64 }, async (_, w) => {
        for (let i = w * per; i < (w + 1) * per; i++) {
            await host.dispatch({ type: 'Tiny', key: `mem${n}-${i}` }, 'noop', [], { callChain: [], callId: `c${i}` });
        }
    }));
    const rate = (per * 64) / (performance.now() - t0) * 1000;
    settle();
    const after = process.memoryUsage();
    console.log(`n=${n}  activations_per_sec(c=64) ${(rate / 1000).toFixed(1)}k  rss_delta/actor ${((after.rss - before.rss) / (per * 64)).toFixed(0)} B  heap_delta/actor ${((after.heapUsed - before.heapUsed) / (per * 64)).toFixed(0)} B`);
    for (let i = 0; i < per * 64; i++) await host.deactivate({ type: 'Tiny', key: `mem${n}-${i}` });
}
await host.stop({ timeoutMs: 5_000 });
