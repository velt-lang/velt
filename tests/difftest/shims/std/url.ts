// Node twin of std/url for the differential harness: JS's own WHATWG `URL`/`URLSearchParams`,
// adapted to the Velt API (errors are `UrlError` with Velt's messages, `searchParams` is a copy
// that can be assigned back, `keys`/`values`/`entries` return arrays).

export class UrlError extends Error {}

export class URLSearchParams extends globalThis.URLSearchParams {
  keys(): any {
    return [...super.keys()];
  }
  values(): any {
    return [...super.values()];
  }
  entries(): any {
    return [...super.entries()];
  }
}

function parseOrThrow(input: string, base: string | null): globalThis.URL {
  if (base != null && !globalThis.URL.canParse(base)) {
    throw new UrlError(`Invalid base URL: ${base}`);
  }
  const u = globalThis.URL.parse(input, base ?? undefined);
  if (u == null) {
    throw new UrlError(`Invalid URL: ${input}`);
  }
  return u;
}

export class URL extends globalThis.URL {
  constructor(input: string, base: string | null = null) {
    super(parseOrThrow(input, base).href);
  }
  static parse(input: string, base: string | null = null): URL | null {
    try {
      return new URL(input, base);
    } catch {
      return null;
    }
  }
  static canParse(input: string, base: string | null = null): boolean {
    return URL.parse(input, base) != null;
  }
  get href(): string {
    return super.href;
  }
  set href(v: string) {
    super.href = parseOrThrow(v, null).href;
  }
  get searchParams(): any {
    return new URLSearchParams(super.search);
  }
  set searchParams(p: URLSearchParams) {
    super.search = p.toString();
  }
}

function decodeOrThrow(f: (s: string) => string, s: string): string {
  try {
    return f(s);
  } catch {
    throw new UrlError("URI malformed");
  }
}

export const encodeURIComponent = globalThis.encodeURIComponent;
export const encodeURI = globalThis.encodeURI;
export function decodeURIComponent(s: string): string {
  return decodeOrThrow(globalThis.decodeURIComponent, s);
}
export function decodeURI(s: string): string {
  return decodeOrThrow(globalThis.decodeURI, s);
}
