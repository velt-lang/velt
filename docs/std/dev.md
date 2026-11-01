# velt/dev

`import { joinStop, drained } from "velt/dev"`. What a program run by `velt dev`
([hot reload](../book/hot-reload.md)) takes part in: the graceful stop before its next version
starts. A server joins it so that no request in flight is cut off; the HTTP server of
`packages/http-server` does, and a server of your own can do the same.

- `joinStop(): AbortSignal | null` joins the graceful stop. The signal is aborted once
  `velt dev` asks the program to stop for its next version: stop taking work (close the
  listener; the supervisor keeps the listening socket, so new connections wait for the next
  version), finish what is in flight, then call `drained()`. Outside `velt dev` it returns
  `null`, and nothing waits.
- `drained()` says that what joined has finished. The program exits once everything that
  joined has drained, or after a bounded wait (1 s); the supervisor stops a program that is
  still running 1.5 s after asking.

```ts
import { joinStop, drained } from "velt/dev";
import { listen } from "velt/net";

async function main() {
  const listener = await listen("127.0.0.1:0");
  const stop = joinStop();
  if (stop != null) {
    // Under `velt dev`: stop accepting when the next version is about to start.
    const closing = spawn(
      (async () => {
        await stop.whenAborted();
        listener.close();
      })(),
    );
  }
  // ... accept connections until the listener closes, finish them ...
  listener.close();
  if (stop != null) {
    drained();
  }
}
```
