// Node twin of std/regex for the differential harness: JS's own `RegExp` behind the Velt API
// (`RegExpMatch` objects, `matchAll` whatever the flags, an explicit `from` offset that leaves
// `lastIndex` alone). Without `from`, `test`/`exec` of a `g` or `y` regex start at `lastIndex` and
// update it, and `replace`/`replaceWith` read and set it, as JS's own methods do.

const JsRegExp = globalThis.RegExp;

export class RegExpError extends Error {}

export class RegExpMatch {
  index: number;
  end: number;
  value: string;
  captures: (string | null)[];
  names: string[];
  constructor(m: RegExpExecArray, names: string[]) {
    this.index = m.index;
    this.end = m.index + m[0].length;
    this.value = m[0];
    this.captures = m.slice(1).map((c) => c ?? null);
    this.names = names;
  }
  group(n: number): string | null {
    return n === 0 ? this.value : (this.captures[n - 1] ?? null);
  }
  named(name: string): string | null {
    const i = this.names.indexOf(name);
    return i < 0 ? null : this.captures[i];
  }
}

/** Group names in order ("" for unnamed groups), from the pattern's `(` openings. */
function groupNames(source: string): string[] {
  const names: string[] = [];
  for (let i = 0; i < source.length; i++) {
    if (source[i] === "\\") i++;
    else if (source[i] === "[") {
      while (i < source.length && source[i] !== "]") i += source[i] === "\\" ? 2 : 1;
    } else if (source[i] === "(") {
      const named = /^\(\?<([A-Za-z_][A-Za-z0-9_]*)>/.exec(source.slice(i));
      if (named) names.push(named[1]);
      else if (source[i + 1] !== "?") names.push("");
    }
  }
  return names;
}

export class RegExp {
  readonly source: string;
  readonly flags: string;
  readonly global: boolean;
  readonly sticky: boolean;
  private names: string[];
  // The regex with its own flags: it holds `lastIndex`.
  private re: any;
  constructor(pattern: string, flags: string = "") {
    try {
      new JsRegExp(pattern, flags);
    } catch (e) {
      throw new RegExpError((e as Error).message);
    }
    this.source = pattern;
    this.flags = flags;
    this.global = flags.includes("g");
    this.sticky = flags.includes("y");
    this.names = groupNames(pattern);
    this.re = new JsRegExp(pattern, flags);
  }
  private js(extra: string): RegExp {
    const flags = this.flags.replace(/[gy]/g, "") + extra;
    return new JsRegExp(this.source, flags) as any;
  }
  test(s: string, from?: number): boolean {
    return this.exec(s, from) != null;
  }
  exec(s: string, from?: number): RegExpMatch | null {
    if (from === undefined && (this.global || this.sticky)) {
      const m = this.re.exec(s);
      return m ? new RegExpMatch(m, this.names) : null;
    }
    const re: any = this.js(this.sticky ? "y" : "g");
    re.lastIndex = from ?? 0;
    const m = re.exec(s);
    return m ? new RegExpMatch(m, this.names) : null;
  }
  get lastIndex(): number {
    return this.re.lastIndex;
  }
  set lastIndex(value: number) {
    this.re.lastIndex = value;
  }
  matchAll(s: string): RegExpMatch[] {
    return [...s.matchAll(this.js("g") as any)].map((m) => new RegExpMatch(m as any, this.names));
  }
  matches(s: string): string[] {
    return this.matchAll(s).map((m) => m.value);
  }
  replace(s: string, replacement: string): string {
    return s.replace(this.re, replacement);
  }
  replaceAll(s: string, replacement: string): string {
    return s.replace(this.js("g") as any, replacement);
  }
  replaceWith(s: string, f: (m: RegExpMatch) => string): string {
    const n = this.names.length;
    return s.replace(this.re, (...args: any[]) => {
      const m: any = args.slice(0, n + 1);
      m.index = args[n + 1];
      return f(new RegExpMatch(m, this.names));
    });
  }
  split(s: string, limit: number = 0): string[] {
    return s.split(this.js("") as any, limit === 0 ? undefined : limit) as string[];
  }
  get groupCount(): number {
    return this.names.length;
  }
  static escape(s: string): string {
    return s.replace(/[\\^$.*+?()[\]{}|\/-]/g, "\\$&");
  }
  clone(): RegExp {
    return new RegExp(this.source, this.flags);
  }
}
