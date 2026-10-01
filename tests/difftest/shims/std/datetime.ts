// Node twin of std/datetime for the differential harness, on JS `Date` (UTC getters,
// `Date.UTC`, `Date.parse`, `toISOString`, `toUTCString`). The harness runs every program with
// `TZ=UTC`, so the local-time API is UTC on both sides. Pattern formatting, month arithmetic and
// `Duration.toString` have no JS equivalent: they are written from the std/datetime docs.

export class DateTimeError extends Error {}

const DAY = 86400000;
const MONTHS = "January February March April May June July August September October November December".split(" ");
const DAYS = "Sunday Monday Tuesday Wednesday Thursday Friday Saturday".split(" ");
const pad = (n: number, w: number): string => (n < 0 ? "-" : "") + `${Math.abs(n)}`.padStart(w, "0");

export class DateTime {
  private ms: number;
  private constructor(ms: number) {
    this.ms = ms;
  }
  static now(): DateTime {
    return new DateTime(Date.now());
  }
  static fromEpochMs(ms: number): DateTime {
    return new DateTime(ms);
  }
  static utc(y: number, mo: number, d = 1, h = 0, mi = 0, s = 0, ms = 0): DateTime {
    const t = new Date(0);
    t.setUTCFullYear(y, mo - 1, d);
    t.setUTCHours(h, mi, s, ms);
    return new DateTime(t.getTime());
  }
  static parse(s: string): DateTime {
    const t = Date.parse(s);
    if (Number.isNaN(t)) throw new DateTimeError(`Invalid date: ${s}`);
    return new DateTime(t);
  }
  static parseHttpDate(s: string): DateTime {
    const t = Date.parse(s);
    if (Number.isNaN(t)) throw new DateTimeError(`Invalid HTTP date: ${s}`);
    return new DateTime(t);
  }
  private get date(): Date {
    return new Date(this.ms);
  }
  toEpochMs(): number {
    return this.ms;
  }
  toEpochSeconds(): number {
    return Math.floor(this.ms / 1000);
  }
  parts() {
    const d = this.date;
    const jan1 = Date.UTC(d.getUTCFullYear(), 0, 1);
    const start = new Date(jan1);
    start.setUTCFullYear(d.getUTCFullYear());
    return {
      year: d.getUTCFullYear(),
      month: d.getUTCMonth() + 1,
      day: d.getUTCDate(),
      hour: d.getUTCHours(),
      minute: d.getUTCMinutes(),
      second: d.getUTCSeconds(),
      millisecond: d.getUTCMilliseconds(),
      dayOfWeek: d.getUTCDay(),
      dayOfYear: Math.floor((this.startOfDay().ms - start.getTime()) / DAY) + 1,
      offsetMinutes: 0,
    };
  }
  localParts() {
    return this.parts();
  }
  get localOffsetMinutes(): number {
    return 0;
  }
  get year(): number {
    return this.date.getUTCFullYear();
  }
  get month(): number {
    return this.date.getUTCMonth() + 1;
  }
  get day(): number {
    return this.date.getUTCDate();
  }
  get hour(): number {
    return this.date.getUTCHours();
  }
  get minute(): number {
    return this.date.getUTCMinutes();
  }
  get second(): number {
    return this.date.getUTCSeconds();
  }
  get millisecond(): number {
    return this.date.getUTCMilliseconds();
  }
  get dayOfWeek(): number {
    return this.date.getUTCDay();
  }
  get dayOfYear(): number {
    return this.parts().dayOfYear;
  }
  toISOString(): string {
    return this.date.toISOString();
  }
  toLocalISOString(): string {
    return this.date.toISOString().replace("Z", "+00:00");
  }
  toUTCString(): string {
    return this.date.toUTCString();
  }
  toString(): string {
    return this.toISOString();
  }
  toJSON(): string {
    return this.toISOString();
  }
  format(pattern: string): string {
    const p = this.parts();
    const h12 = p.hour % 12 === 0 ? 12 : p.hour % 12;
    const tokens: Record<string, string> = {
      YYYY: pad(p.year, 4), YY: pad(Math.abs(p.year % 100), 2),
      M: `${p.month}`, MM: pad(p.month, 2), MMM: MONTHS[p.month - 1].slice(0, 3), MMMM: MONTHS[p.month - 1],
      D: `${p.day}`, DD: pad(p.day, 2), DDDD: pad(p.dayOfYear, 3),
      ddd: DAYS[p.dayOfWeek].slice(0, 3), dddd: DAYS[p.dayOfWeek],
      H: `${p.hour}`, HH: pad(p.hour, 2), h: `${h12}`, hh: pad(h12, 2),
      A: p.hour < 12 ? "AM" : "PM", a: p.hour < 12 ? "am" : "pm",
      m: `${p.minute}`, mm: pad(p.minute, 2), s: `${p.second}`, ss: pad(p.second, 2),
      SSS: pad(p.millisecond, 3), Z: "+00:00", ZZ: "+0000",
    };
    return pattern.replace(/\[([^\]]*)\]?|(.)\2*/g, (run, lit) => lit ?? (Object.hasOwn(tokens, run) ? tokens[run] : run));
  }
  formatLocal(pattern: string): string {
    return this.format(pattern);
  }
  addMs(n: number): DateTime {
    return new DateTime(this.ms + n);
  }
  addSeconds(n: number): DateTime {
    return new DateTime(this.ms + n * 1000);
  }
  addMinutes(n: number): DateTime {
    return new DateTime(this.ms + n * 60000);
  }
  addHours(n: number): DateTime {
    return new DateTime(this.ms + n * 3600000);
  }
  addDays(n: number): DateTime {
    return new DateTime(this.ms + n * DAY);
  }
  addMonths(n: number): DateTime {
    const d = this.date;
    const target = new Date(this.ms);
    target.setUTCDate(1);
    target.setUTCMonth(d.getUTCMonth() + n);
    const last = new Date(target.getTime());
    last.setUTCMonth(last.getUTCMonth() + 1, 0);
    target.setUTCDate(Math.min(d.getUTCDate(), last.getUTCDate()));
    return new DateTime(target.getTime());
  }
  addYears(n: number): DateTime {
    return this.addMonths(n * 12);
  }
  add(d: Duration): DateTime {
    return new DateTime(this.ms + d.toMs());
  }
  diffMs(other: DateTime): number {
    return this.ms - other.ms;
  }
  since(other: DateTime): Duration {
    return Duration.ofMs(this.ms - other.ms);
  }
  startOfDay(): DateTime {
    return new DateTime(Math.floor(this.ms / DAY) * DAY);
  }
  compareTo(other: DateTime): number {
    return Math.sign(this.ms - other.ms);
  }
  isBefore(other: DateTime): boolean {
    return this.ms < other.ms;
  }
  isAfter(other: DateTime): boolean {
    return this.ms > other.ms;
  }
  equals(other: DateTime): boolean {
    return this.ms === other.ms;
  }
}

export class Duration {
  private ms: number;
  private constructor(ms: number) {
    this.ms = ms;
  }
  static ofMs(n: number): Duration {
    return new Duration(n);
  }
  static ofSeconds(n: number): Duration {
    return new Duration(n * 1000);
  }
  static ofMinutes(n: number): Duration {
    return new Duration(n * 60000);
  }
  static ofHours(n: number): Duration {
    return new Duration(n * 3600000);
  }
  static ofDays(n: number): Duration {
    return new Duration(n * DAY);
  }
  toMs(): number {
    return this.ms;
  }
  toSeconds(): number {
    return this.ms / 1000;
  }
  plus(o: Duration): Duration {
    return new Duration(this.ms + o.ms);
  }
  minus(o: Duration): Duration {
    return new Duration(this.ms - o.ms);
  }
  negated(): Duration {
    return new Duration(0 - this.ms);
  }
  compareTo(o: Duration): number {
    return Math.sign(this.ms - o.ms);
  }
  toString(): string {
    const abs = Math.abs(this.ms);
    const days = Math.floor(abs / DAY);
    const h = Math.floor((abs % DAY) / 3600000);
    const m = Math.floor((abs % 3600000) / 60000);
    const ms = abs % 60000;
    let time = (h ? `${h}H` : "") + (m ? `${m}M` : "") + (ms ? `${ms / 1000}S` : "");
    if (!time && !days) time = "0S";
    return (this.ms < 0 ? "-P" : "P") + (days ? `${days}D` : "") + (time ? `T${time}` : "");
  }
}
