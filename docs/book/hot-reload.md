# Hot reload with `velt dev`

`velt dev` is the fast edit loop: run your program once, then every save updates the running
program, usually in well under 100 ms and without losing its state.

```sh
velt dev                  # the current package
velt dev server.vlt       # one file
```

## A session

Take a server that counts requests:

```ts
import { serve } from "velt:http";

async function main() {
  const hits = shared(0);
  await serve({ port: 8080 }, async (req: Request): Promise<Response> => {
    hits.add(1);
    return new Response(`hello #${hits.get()}\n`);
  });
}
```

```
$ velt dev server.vlt
velt dev: started in 410 ms
```

In another terminal:

```
$ curl localhost:8080
hello #1
$ curl localhost:8080
hello #2
```

Change the greeting to `` `hi there #${hits.get()}\n` `` and save. `velt dev` prints:

```
velt dev: hot-swapped 3 functions in 82 ms
```

```
$ curl localhost:8080
hi there #3
```

The handler's code changed, but the counter kept its value and the server never stopped
listening: the changed functions were swapped into the running program. Now add a line to
`main` itself (say, `const start = 1;`) and save:

```
velt dev: restarted (main changed (it already ran)) in 1475 ms
```

```
$ curl localhost:8080
hi there #1
```

`main` has already run, so a change to it can't be swapped in: the program restarts and the
counter starts over. (These timings come from a debug build of `velt`; see
[Speed](../tooling/dev.md#speed) for release numbers.)

## Swap or restart

- **Function bodies** (handlers, helpers, methods, async functions, new functions and types)
  are swapped in place. New calls run the new code; requests already in flight finish on the
  old code.
- **Layout changes** restart the program: adding a field to a class, changing a function's
  signature, changing what a closure captures, or editing `main` itself. `velt dev` says why:
  `restarted (Point gained a field)`. The listening socket stays open across the restart, so
  no request is refused.
- **Compile errors** are printed, and the previous version keeps running until you fix them.

## Why it works without setup

Hot reload in most languages needs care around global state. Velt has none to migrate: module
scope holds only constants, functions and types, and state lives in values `main` creates (like
`hits` above). That rule is what makes a swap safe by default.

## Debugging and limits

`velt dev --exe` links a real executable per version so a debugger can attach; it restarts
instead of swapping. Loops that never return keep running their old code, and functions that
only ran at startup don't run again after a swap. Details, timings and platform notes:
[`velt dev`](../tooling/dev.md).

`velt test --watch` gives the same loop for tests.
