// Node twin of std/regex for the differential harness: JS's own `RegExp` behind the Velt API
// (explicit `from` offsets instead of `lastIndex`, `RegExpMatch` objects, `matchAll` whatever the
// flags). Subjects must be ASCII: Velt offsets are bytes, JS offsets UTF-16 units.

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
  }
  private js(extra: string): RegExp {
    const flags = this.flags.replace(/[gy]/g, "") + extra;
    return new JsRegExp(this.source, flags) as any;
  }
  test(s: string, from: number = 0): boolean {
    return this.exec(s, from) != null;
  }
  exec(s: string, from: number = 0): RegExpMatch | null {
    const re: any = this.js(this.sticky ? "y" : "g");
    re.lastIndex = from;
    const m = re.exec(s);
    return m ? new RegExpMatch(m, this.names) : null;
  }
  matchAll(s: string): RegExpMatch[] {
    return [...s.matchAll(this.js("g") as any)].map((m) => new RegExpMatch(m as any, this.names));
  }
  matches(s: string): string[] {
    return this.matchAll(s).map((m) => m.value);
  }
  replace(s: string, replacement: string): string {
    return s.replace(this.js(this.global ? "g" : "") as any, replacement);
  }
  replaceAll(s: string, replacement: string): string {
    return s.replace(this.js("g") as any, replacement);
  }
  replaceWith(s: string, f: (m: RegExpMatch) => string): string {
    const found = this.global ? this.matchAll(s) : [this.exec(s, 0)].filter((m) => m != null);
    let out = "";
    let last = 0;
    for (const m of found as RegExpMatch[]) {
      out += s.slice(last, m.index) + f(m);
      last = m.end;
    }
    return out + s.slice(last);
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
