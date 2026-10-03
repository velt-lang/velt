# velt:datetime

`import { DateTime, Duration } from "velt:datetime"`. `DateTime` is a UTC-first instant: epoch
milliseconds in a small struct. **Months are 1-12.** Local time is opt-in and DST-aware, using
the OS time zone. `TZ` overrides it: on macOS and Linux any value the C library understands; on
Windows only UTC (`UTC`, `Etc/UTC`, `GMT`, ...) and fixed offsets (`Etc/GMT-2` is UTC+2), other
names keep the system zone. Node differs there: it understands region names like
`Europe/Berlin`, and uses UTC for a name it doesn't recognise (a typo, the wrong case).

- Constructors:
  - `DateTime.now()`, `fromEpochMs(ms)`
  - `utc(year, month, day = 1, hour = 0, minute = 0, second = 0, ms = 0)`: fields roll over
    like `Date.UTC`
  - `parse(iso)`: ISO 8601 / RFC 3339; see the notes for what it accepts
  - `parseHttpDate(s)`: accepts all three RFC 7231 formats
  - `parse` and `parseHttpDate` throw `DateTimeError`.
- UTC getters: `year month day hour minute second millisecond dayOfWeek (0 = Sunday) dayOfYear`.
- `parts()` / `localParts(): DateParts`, a struct of all fields plus `offsetMinutes`.
  `localOffsetMinutes`.
- Output:
  - `toISOString()`: `2024-02-29T12:34:56.000Z`
  - `toLocalISOString()`: `…+01:00`
  - `toUTCString()`: an HTTP date
  - `format(pattern)` / `formatLocal(pattern)`: tokens `YYYY YY M MM MMM MMMM D DD DDDD ddd dddd
    H HH h hh A a m mm s ss SSS Z ZZ`; text in `[brackets]` is literal
  - `toEpochMs()`, `toEpochSeconds()`
- Arithmetic:
  - `addMs addSeconds addMinutes addHours addDays`
  - `addMonths` and `addYears`: clamp the day, so Jan 31 + 1 month is Feb 29 in a leap year
  - `add(Duration)`, `diffMs(other)`, `since(other): Duration`, `startOfDay()`
- Comparison: `compareTo` (it implements `Comparable`, so generic code can use `<`), `isBefore`,
  `isAfter`, `equals`.
- `Duration`:
  - constructors: `ofMs ofSeconds ofMinutes ofHours ofDays`
  - `toMs()`, `toSeconds(): f64`, `plus`, `minus`, `negated`, `compareTo`
  - `toString()`: ISO 8601, e.g. `P1DT12H`

```ts
import { DateTime, Duration } from "velt:datetime";

function main() {
  const d = DateTime.parse("2024-01-31T09:30:00+01:00");
  console.log(d.toISOString(), d.format("dddd D MMMM YYYY, HH:mm"));
  console.log(d.addMonths(1).toISOString(), d.addDays(1).toUTCString());
  const due = d.add(Duration.ofHours(36));
  console.log(due.since(d).toString(), due.isAfter(d)); // P1DT12H true
  try {
    DateTime.parse("2023-02-29");
  } catch (e) {
    console.log(e.message); // Invalid date: 2023-02-29
  }
}
```

Notes:
- `parse` accepts `YYYY`, `YYYY-MM`, `YYYY-MM-DD` or `±YYYYYY` years, then an optional time
  `THH:mm[:ss[.fraction]]` (`t` or a space also work), then an optional zone `Z`, `±HH:mm`,
  `±HHmm` or `±HH`.
- `parse` reads strings without an offset as UTC (JS reads date-times without one as local
  time), and it rejects impossible dates and times instead of rolling them over.
- There are no leap seconds and no time-zone database beyond the OS offset.
