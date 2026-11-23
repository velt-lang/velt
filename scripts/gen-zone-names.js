#!/usr/bin/env node
// Generates std/time/zone_names_table.velt and std/time/zone_names_windows_table.velt: the long
// zone name `Date.prototype.toString()` and `toTimeString()` end with, `(Central European
// Standard Time)`, as Node prints it for every time zone `TZ` can name.
//
//   node scripts/gen-zone-names.js
//
// Node's name is V8's `DateCache::LocalTimezone(t)`: ICU's `TimeZone::getDisplayName(isDst,
// LONG)` of the process' zone, with `isDst` whether ICU's zone is on daylight time at `t`.
// - ICU takes the name from the zone's metazone *today*, not at `t` (Lisbon in 1993, on CET, is
//   "Western European Standard Time"); a zone without a metazone gets ICU's GMT form of its
//   current offset (`GMT-05:00` for `Etc/GMT+5`). `Intl.DateTimeFormat`'s `timeZoneName: "long"`
//   names a date by its metazone at that date, which differs, so the names are read from
//   `toString()` itself.
// - Outside 1970-01-01 .. 2038-01-19T03:14:07Z (`0 <= t <= kMaxEpochTimeInMs`), V8 asks about
//   the same UTC month, day and time in an "equivalent year" of 2008..2035 with the same leap
//   year and weekday of January 1 (`EquivalentTime`): Berlin in 1850 is "Central European
//   Standard Time" in January and "Central European Summer Time" in July. zone_name.velt does
//   the same, so the table needs only that range.
//
// One Node process per zone id, with `TZ` set to it, samples `toString()` every 12 hours over
// that range and finds each change of (offset, name) to the second by bisection. The table keys
// a name by the offset at the instant: per zone, runs from an instant on, each a map from offset
// (seconds) to name. A new run starts only where an offset's name changes (London at +01:00:
// "Greenwich Mean Time" on British Standard Time until October 1971, "British Summer Time"
// after). A name that is just the offset's GMT form (`GMT-05:00`, which zone_name.velt derives)
// is not stored. A zone `TZ` can't name is printed in that form too, as Node does (`TZ=XYZ3`:
// `(GMT-03:00)`). The worker checks the table against `toString()` inside the range and, through
// the equivalent time, at instants from the year 1000 to 9999.
//
// `toString()` runs the zone ICU detects from `TZ`, which depends on the OS: Node on Linux runs
// `TZ=Eire` at +01:00 all year. So each sample's offset is checked against
// `Intl.DateTimeFormat`'s for the id, which is the same everywhere; a zone where they differ
// takes the record of the zone Intl resolves its id to (`Eire` takes `Europe/Dublin`'s, which on
// macOS is the record `Eire` has itself), and the table is the same on every OS. The numbers are
// written as strings of digits (`digits`), cheaper to compile than array literals.
//
// The zone ids are Node's own (`Intl.supportedValuesOf("timeZone")`) and every alias of CLDR's
// bcp47/timezone.xml Node accepts (`US/Eastern`, `GMT0`, `EST5EDT`); Windows' registry keys map
// to ids through CLDR's windowsZones.xml (territory 001). Both come from the CLDR release Node's
// ICU uses, read into scripts/gen-zone-names-cldr.json by
//
//   node scripts/gen-zone-names.js --extract <windowsZones.xml> <timezone.xml>
//
// (from https://github.com/unicode-org/cldr/tree/release-48/common/supplemental/windowsZones.xml
// and .../common/bcp47/timezone.xml).
//
// It runs only on the Node, ICU, CLDR and tz versions below, so a later run on another Node
// can't change the table without this file saying so: update the versions, run it, and check the
// `date_zone_name` golden and std/time/zone_name.test.velt against that Node.

"use strict";
const fs = require("fs");
const path = require("path");
const { spawn } = require("child_process");

const NODE_MAJOR = "22";
const ICU = "78.3";
const CLDR = "48.0";
const TZDATA = "2026e";
const CLDR_TAG = "release-48";

const v = process.versions;
if (v.node.split(".")[0] !== NODE_MAJOR || v.icu !== ICU || v.cldr !== CLDR || v.tz !== TZDATA) {
  throw new Error(
    `the table is Node ${NODE_MAJOR}'s (ICU ${ICU}, CLDR ${CLDR}, tz ${TZDATA}); ` +
      `this is Node ${v.node} (ICU ${v.icu}, CLDR ${v.cldr}, tz ${v.tz})`,
  );
}

const ROOT = path.join(__dirname, "..");
const CLDR_JSON = path.join(__dirname, "gen-zone-names-cldr.json");

/** The sampled range (V8's own: `EquivalentTime` maps the rest into it) and step, in ms. */
const FROM = 0;
const TO = 2147483647 * 1000;
const STEP = 12 * 3600 * 1000;

/** V8's `EquivalentYear`: a year of 2008..2035 with `year`'s leap year and weekday of January 1. */
function equivalentYear(year) {
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const days = Math.floor(Date.UTC(year, 0, 1) / 86400000);
  const weekDay = (((days + 4) % 7) + 7) % 7;
  const recent = (leap ? 1956 : 1967) + ((weekDay * 12) % 28);
  return 2008 + ((recent + 3 * 28 - 2008) % 28);
}

/** V8's `EquivalentTime` where `LocalTimezone` uses it: `t` itself inside the range. */
function equivalentTime(t) {
  if (t >= FROM && t <= TO) return t;
  const d = new Date(t);
  const days = Math.floor(t / 86400000);
  const inDay = t - days * 86400000;
  const day = Date.UTC(equivalentYear(d.getUTCFullYear()), d.getUTCMonth(), d.getUTCDate());
  return day + inDay;
}

/** The GMT form of an offset in seconds, as zone_name.velt derives it: `GMT+05:30`, `GMT-00:36:45`. */
function offsetForm(secs) {
  const a = Math.abs(secs);
  const two = (n) => String(n).padStart(2, "0");
  const s = a % 60;
  return (
    `GMT${secs < 0 ? "-" : "+"}${two(Math.floor(a / 3600))}:${two(Math.floor(a / 60) % 60)}` +
    (s ? `:${two(s)}` : "")
  );
}

const ZONE_TZ = process.env.TZ;
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const SHAPE = /^\w{3} (\w{3}) (\d{2}) (\d{4}) (\d{2}):(\d{2}):(\d{2}) GMT[+-]\d{4} \((.*)\)$/;

/**
 * `[offset in seconds, name]` of `toString()` at `t` (a whole second), in this process' zone, as
 * a program printing one date gets it. Setting `TZ` resets V8's date cache first: the cache
 * extends the daylight-saving segments it has seen and can miss a short one (Asuncion's nine
 * days of "Paraguay Summer Time" at -03:00 before October 15, 2024, when -03:00 became its
 * standard time), so a scan would see what no single `toString()` prints.
 */
function sample(t) {
  process.env.TZ = ZONE_TZ;
  const s = new Date(t).toString();
  const m = SHAPE.exec(s);
  if (!m) throw new Error(`unexpected toString(): ${s}`);
  const local = Date.UTC(+m[3], MONTHS.indexOf(m[1]), +m[2], +m[4], +m[5], +m[6]);
  return [(local - t) / 1000, m[7]];
}

const same = (a, b) => a[0] === b[0] && a[1] === b[1];

/**
 * The offset in seconds `Intl.DateTimeFormat` gives zone `TZ` at `t`: ICU's zone of that id,
 * the same on every OS, where `toString()` runs the zone ICU detected from `TZ`, which isn't
 * (Node on Linux runs `TZ=Eire` at +01:00 all year).
 */
let intlFormat = null;
function intlOffset(t) {
  intlFormat ??= new Intl.DateTimeFormat("en-US", { timeZone: ZONE_TZ, timeZoneName: "longOffset" });
  const s = intlFormat.formatToParts(t).find((p) => p.type === "timeZoneName").value;
  const m = /^GMT(?:([+-])(\d\d):(\d\d)(?::(\d\d))?)?$/.exec(s);
  if (!m) throw new Error(`unexpected offset ${s}`);
  const secs = m[1] ? +m[2] * 3600 + +m[3] * 60 + +(m[4] ?? 0) : 0;
  return m[1] === "-" ? -secs : secs;
}

/** Whether every sample's `toString()` offset was `intlOffset`'s. */
let agrees = true;

/** Worker: the segments of this process' zone, `[start ms, offset, name]`, the first from FROM. */
function segmentsHere() {
  let prevT = FROM;
  let prev = sample(FROM);
  const segs = [[FROM, prev[0], prev[1]]];
  for (let t = FROM + STEP; prevT < TO; t += STEP) {
    if (t > TO) t = TO;
    const cur = sample(t);
    agrees &&= cur[0] === intlOffset(t);
    if (same(cur, prev)) {
      prevT = t;
      continue;
    }
    // The change is in (prevT, t]: bisect to the second.
    let lo = prevT;
    let hi = t;
    while (hi - lo > 1000) {
      const mid = lo + Math.floor((hi - lo) / 2000) * 1000;
      if (same(sample(mid), prev)) lo = mid;
      else hi = mid;
    }
    const at = sample(hi);
    segs.push([hi, at[0], at[1]]);
    prev = at;
    prevT = hi;
    if (!same(at, cur)) {
      // Two changes between samples: go on from the first.
      t = hi;
    }
  }
  return segs;
}

/**
 * The runs of a zone's segments: `[start ms, [[offset, name | null], …]]`, a name null where it
 * is the offset's GMT form. A run lasts while no offset changes its name.
 */
function runsOf(segs) {
  const runs = [];
  let map = null;
  for (const [start, offset, name] of segs) {
    const n = name === offsetForm(offset) ? null : name;
    if (map && map.has(offset) && map.get(offset) !== n) map = null;
    if (!map) {
      map = new Map();
      runs.push([start, map]);
    }
    map.set(offset, n);
  }
  return runs.map(([start, m]) => [
    start,
    [...m.entries()].filter(([, n]) => n !== null).sort((a, b) => a[0] - b[0]),
  ]);
}

/**
 * Checks `runs` as zone_name.velt reads them against `toString()`: at the start, middle and last
 * second of each segment, and at 20,000 instants from the year 1000 to 9999 through the
 * equivalent time (with the offset there, as zone_name.velt asks the OS for it).
 */
function check(runs, segs) {
  const nameAt = (t) => {
    const e = equivalentTime(t);
    const offset = sample(e)[0];
    let r = runs[0];
    for (const x of runs) if (x[0] <= e) r = x;
    const hit = r[1].find(([o]) => o === offset);
    return hit ? hit[1] : offsetForm(offset);
  };
  const instants = [];
  for (let i = 0; i < segs.length; i++) {
    const end = i + 1 < segs.length ? segs[i + 1][0] : TO + 1000;
    const start = segs[i][0];
    instants.push(start, Math.floor((start + end) / 2000) * 1000, end - 1000);
  }
  // A fixed pseudo-random spread (MINSTD), whole seconds.
  const lo = Date.UTC(1000, 0, 1);
  const hi = Date.UTC(9999, 11, 31);
  let x = 12345;
  for (let i = 0; i < 20000; i++) {
    x = (x * 48271) % 2147483647;
    instants.push(lo + Math.floor(((x / 2147483647) * (hi - lo)) / 1000) * 1000);
  }
  for (const t of instants) {
    const n = sample(t)[1];
    if (nameAt(t) !== n) throw new Error(`${process.env.TZ} at ${t}: ${n}, table ${nameAt(t)}`);
  }
}

if (process.argv[2] === "--worker") {
  const segs = segmentsHere();
  const runs = runsOf(segs);
  check(runs, segs);
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  process.stdout.write(JSON.stringify({ zone, runs, agrees }));
  process.exit(0);
}

if (process.argv[2] === "--extract") {
  const win = fs.readFileSync(process.argv[3], "utf8");
  const tz = fs.readFileSync(process.argv[4], "utf8");
  const windows = {};
  for (const m of win.matchAll(/<mapZone other="([^"]+)" territory="001" type="([^"]+)"\/>/g)) {
    windows[m[1]] = m[2];
  }
  const aliases = new Set();
  for (const m of tz.matchAll(/ alias="([^"]+)"/g)) {
    for (const id of m[1].split(" ")) aliases.add(id);
  }
  const out = {
    source: `unicode-org/cldr ${CLDR_TAG}: common/supplemental/windowsZones.xml (territory 001), common/bcp47/timezone.xml (aliases)`,
    license: "Unicode License v3 (https://www.unicode.org/license.txt)",
    windows,
    aliases: [...aliases].sort(),
  };
  fs.writeFileSync(CLDR_JSON, JSON.stringify(out, null, 1) + "\n");
  console.log(`${CLDR_JSON}: ${Object.keys(windows).length} Windows keys, ${aliases.size} ids`);
  process.exit(0);
}

const cldr = JSON.parse(fs.readFileSync(CLDR_JSON, "utf8"));
const accepted = (id) => {
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: id });
    return true;
  } catch {
    return false;
  }
};
const ids = [...new Set([...Intl.supportedValuesOf("timeZone"), ...cldr.aliases, "UTC"])]
  .filter(accepted)
  .sort();
for (const id of Object.values(cldr.windows)) {
  if (!ids.includes(id)) throw new Error(`windowsZones names ${id}, which Node doesn't accept`);
}

/** Runs the worker with `TZ=id`. */
function zoneRuns(id) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [__filename, "--worker"], {
      env: { ...process.env, TZ: id },
      stdio: ["ignore", "pipe", "inherit"],
    });
    let out = "";
    child.stdout.on("data", (d) => (out += d));
    child.on("exit", (code) => {
      if (code !== 0) return reject(new Error(`${id}: worker exited with ${code}`));
      const r = JSON.parse(out);
      // `TZ` must be the zone Node took (a rejected one would fall back to the system zone);
      // ICU runs `TZ=GMT` as the offset zone `+00:00`.
      const zone = new Intl.DateTimeFormat("en-US", { timeZone: id }).resolvedOptions().timeZone;
      if (r.zone !== zone && !/^[+-]\d\d:\d\d$/.test(r.zone)) {
        return reject(new Error(`${id}: Node runs it as ${r.zone}, not ${zone}`));
      }
      resolve({ runs: r.runs, agrees: r.agrees, canonical: zone });
    });
  });
}

async function main() {
  const jobs = Math.max(1, Math.min(4, require("os").cpus().length - 1));
  const results = new Array(ids.length);
  let next = 0;
  await Promise.all(
    Array.from({ length: jobs }, async () => {
      while (next < ids.length) {
        const i = next++;
        results[i] = await zoneRuns(ids[i]);
      }
    }),
  );

  // A zone whose `toString()` offsets aren't Intl's for its id ran as another zone ICU detected
  // from `TZ`, which depends on the OS: it takes the record of the zone Intl resolves the id to,
  // which must have agreed itself. So the table is the same on every OS.
  const runsOfId = new Map(ids.map((id, i) => [id, results[i]]));
  for (const [i, r] of results.entries()) {
    if (r.agrees) continue;
    const canonical = runsOfId.get(r.canonical);
    if (!canonical || !canonical.agrees) {
      throw new Error(`${ids[i]}: its offsets aren't Intl's, nor are ${r.canonical}'s`);
    }
    console.log(`${ids[i]}: toString()'s offsets aren't Intl's; it takes ${r.canonical}'s names`);
  }
  for (const [i, r] of results.entries()) {
    results[i] = r.agrees ? r.runs : runsOfId.get(r.canonical).runs;
  }

  // Names, records (a zone's runs; zones with the same runs share one) and their indexes.
  const names = [];
  const nameIndex = new Map();
  const nameOf = (n) => {
    if (n.length === 0) throw new Error("an empty name");
    if (!nameIndex.has(n)) {
      nameIndex.set(n, names.length);
      names.push(n);
    }
    return nameIndex.get(n);
  };
  const records = [];
  const recordIndex = new Map();
  const zoneRecord = results.map((runs) => {
    const key = JSON.stringify(runs);
    if (!recordIndex.has(key)) {
      recordIndex.set(key, records.length);
      records.push(runs);
    }
    return recordIndex.get(key);
  });
  const recordRuns = [];
  const runStart = [];
  const runPairs = [];
  const pairs = [];
  for (const runs of records) {
    recordRuns.push(runStart.length);
    for (const [start, map] of runs) {
      runStart.push(start / 1000);
      runPairs.push(pairs.length);
      for (const [offset, name] of map) {
        if (Math.abs(offset) >= OFFSET_BIAS) throw new Error(`offset ${offset}`);
        pairs.push((offset + OFFSET_BIAS) * NAME_SLOTS + nameOf(name));
      }
    }
  }
  recordRuns.push(runStart.length);
  runPairs.push(pairs.length);
  if (names.length > NAME_SLOTS) throw new Error(`${names.length} names`);
  const nameAt = [];
  let at = 0;
  for (const n of names) {
    nameAt.push(at);
    at += n.length;
  }
  nameAt.push(at);

  writeTable(ids, zoneRecord, recordRuns, runStart, runPairs, pairs, names, nameAt);
  writeWindows(cldr.windows);
  const bytes =
    names.join("").length +
    3 * nameAt.length +
    ids.join("\n").length + 3 * ids.length +
    2 * zoneRecord.length +
    2 * recordRuns.length +
    6 * runStart.length +
    2 * runPairs.length +
    5 * pairs.length;
  console.log(
    `${ids.length} zone ids, ${records.length} records, ${runStart.length} runs, ` +
      `${pairs.length} pairs, ${names.length} names: about ${bytes} bytes of data`,
  );
}

/** An offset is stored biased by this (seconds), times `NAME_SLOTS`, plus the name's index. */
const OFFSET_BIAS = 86400;
const NAME_SLOTS = 1024;


/** A string literal of `text` split over lines (`+`), each at most about 90 characters. */
function stringLines(text) {
  const parts = [];
  for (let i = 0; i < text.length; i += 88) parts.push(JSON.stringify(text.slice(i, i + 88)));
  return parts.map((p, i) => `  ${p}${i + 1 < parts.length ? " +" : ";"}`);
}

function header(role, user) {
  return [
    `// velt/time (internal): ${role} (${user}),`,
    "// generated by scripts/gen-zone-names.js: do not edit.",
    "//",
    `// From Node ${NODE_MAJOR} (ICU ${ICU}, CLDR ${CLDR}, tz ${TZDATA}); see the script for how.`,
    "// Derived from Unicode CLDR data, © Unicode, Inc., Unicode License v3",
    "// (https://www.unicode.org/license.txt).",
    "",
  ];
}

/** Where each id's leading `\n` is in `ZONE_IDS`. */
function zoneAt(ids) {
  const at = [];
  let p = 0;
  for (const id of ids) {
    at.push(p);
    p += id.length + 1;
  }
  if (p >= 64 ** 3) throw new Error("ZONE_IDS outgrew three digits of position");
  return at;
}

/**
 * `values` as one string of `width` digits each, six bits a digit, most significant first: digit
 * `d` is the character `35 + d`, one more from `\` (92) on, so no digit needs an escape (zone_name.velt's
 * `entry` reads them). A string literal is one token for the compiler, where an array literal of
 * as many numbers is that many expressions to parse, check and lower.
 */
function digits(values, width) {
  let s = "";
  for (const v of values) {
    if (!Number.isInteger(v) || v < 0 || v >= 64 ** width) throw new Error(`${v} needs more than ${width} digits`);
    for (let k = width - 1; k >= 0; k--) {
      const d = Math.floor(v / 64 ** k) % 64;
      const c = 35 + d;
      s += String.fromCharCode(c >= 92 ? c + 1 : c);
    }
  }
  return s;
}

/** An exported table of numbers: its digits' width and its string. */
function numbers(name, doc, values, width) {
  return [
    `/** ${doc} */`,
    `export const ${name}_WIDTH: i64 = ${width};`,
    `export const ${name}: string =`,
    ...stringLines(digits(values, width)),
    "",
  ];
}

function writeTable(ids, zoneRecord, recordRuns, runStart, runPairs, pairs, names, nameAt) {
  const lines = header("the long zone names of `Date`'s `toString()`", "zone_name.velt");
  lines.push(
    "// The tables of numbers are strings of digits, each entry `_WIDTH` of them: six bits a digit,",
    "// most significant first, digit `d` the character `35 + d`, one more from `\\` (92) on.",
    "",
    "/** Every zone id `TZ` can name, sorted, each between two `\\n`s. */",
    "export const ZONE_IDS: string =",
    ...stringLines(`\n${ids.join("\n")}\n`),
    "",
    ...numbers("ZONE_AT", "Per zone id, in `ZONE_IDS`' order: where the `\\n` before it is.", zoneAt(ids), 3),
    ...numbers("ZONE_RECORD", "Per zone id, in `ZONE_IDS`' order: its record.", zoneRecord, 2),
    ...numbers(
      "RECORD_RUNS",
      "Per record: its first run in `RUN_START`; one more entry, the end.",
      recordRuns,
      2,
    ),
    ...numbers(
      "RUN_START",
      "Per run: the second it starts at (a record's first run: 0, 1970-01-01).",
      runStart,
      6,
    ),
    ...numbers("RUN_PAIRS", "Per run: its first pair in `PAIRS`; one more entry, the end.", runPairs, 2),
    `/** The pairs: (offset in seconds + OFFSET_BIAS) * NAME_SLOTS + the name's index. */`,
    `export const OFFSET_BIAS: i64 = ${OFFSET_BIAS};`,
    `export const NAME_SLOTS: i64 = ${NAME_SLOTS};`,
    ...numbers("PAIRS", "The pairs, run by run.", pairs, 5),
    "/** The names, one after another; name `i` runs from `NAME_AT`'s entry `i` to entry `i + 1`. */",
    "export const NAMES: string =",
    ...stringLines(names.join("")),
    "",
    ...numbers("NAME_AT", "Where each name starts in `NAMES`; one more entry, the end.", nameAt, 3),
  );
  const out = path.join(ROOT, "std", "time", "zone_names_table.velt");
  fs.writeFileSync(out, lines.join("\n"));
  console.log(`${out}: ${lines.length} lines`);
}

function writeWindows(windows) {
  const keys = Object.keys(windows).sort();
  const lines = header("Windows' time zones as IANA ids", "zone_id_win32.velt");
  lines.push(
    "/** The registry keys (`TimeZoneKeyName`), sorted, each between two `\\n`s. */",
    "export const WINDOWS_KEYS: string =",
    ...stringLines(`\n${keys.join("\n")}\n`),
    "",
    "/** Line by line as `WINDOWS_KEYS`: each key's IANA id (CLDR's territory 001). */",
    "export const WINDOWS_IDS: string =",
    ...stringLines(`\n${keys.map((k) => windows[k]).join("\n")}\n`),
    "",
  );
  const out = path.join(ROOT, "std", "time", "zone_names_windows_table.velt");
  fs.writeFileSync(out, lines.join("\n"));
  console.log(`${out}: ${keys.length} keys`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
