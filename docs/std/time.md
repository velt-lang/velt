# velt/time

`import { DateTime, Duration, delay, Ticker } from "velt/time"`. Time:

- [`DateTime` and `Duration`](time/datetime.md): a UTC-first instant (JavaScript's `Date`
  without its local-time pitfalls; months are 1-12) and a signed span with millisecond
  precision, with parsing, formatting and calendar fields (`DateParts`); `DateTimeError`.
- [Timers](time/timers.md): `setTimeout`, `setInterval`, `setImmediate` and their `clear…`
  functions (also globals, as in Node) returning a `Timer`; `delay(ms)`; `Ticker`, a drift-free
  periodic schedule to `for await` over, stopped from any task with a `TickerStop`.
