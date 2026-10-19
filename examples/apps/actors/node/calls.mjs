// The Node side of `actors calls`: `c` callers make `n` calls in all to `Tiny.noop` through
// `host.dispatch`, on one warm key ("warm") or round robin over 1000 warm keys ("fan"), with
// the fixture of @sigx/actors' own benchmarks (benchmarks/src/host-fixture.ts).
//
//   SIGX_ACTORS_REPO=/path/to/actors node --conditions=production calls.mjs warm 64 100000
//
// SIGX_ACTORS_REPO is a BUILT signalxjs/actors checkout; Node 22.18+ runs its .ts sources.
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const repo = process.env.SIGX_ACTORS_REPO ?? join(import.meta.dirname, '../../../../../actors');
const load = (file) => import(pathToFileURL(join(repo, 'benchmarks', 'src', file)).href);
const { benchCall, createBenchHost, refsFor, warmActivations } = await load('host-fixture.ts');
const { Tiny } = await load('actors.ts');

const [scenario = 'warm', cArg = '64', nArg = '100000'] = process.argv.slice(2);
const c = Number(cArg);
const per = Math.trunc(Number(nArg) / c);
const fixture = await createBenchHost({ actors: [Tiny] });
const refs = scenario === 'fan' ? refsFor(Tiny.type, 1000) : [{ type: Tiny.type, key: 'warm' }];
await warmActivations(fixture.host, refs);
const call = benchCall();

async function caller(offset) {
    for (let i = 0; i < per; i++) {
        await fixture.host.dispatch(refs[(offset + i) % refs.length], 'noop', [], call);
    }
    return per;
}

const t0 = performance.now();
const done = (await Promise.all(Array.from({ length: c }, (_, w) => caller(w)))).reduce((a, b) => a + b, 0);
console.log(`node ${scenario} c=${c} calls=${done} ${(performance.now() - t0).toFixed(0)} ms`);
await fixture.stop();
