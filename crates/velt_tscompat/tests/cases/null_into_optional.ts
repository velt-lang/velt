// TypeScript types an optional parameter or field `T | undefined`, which has no `null`.

export type Options = { port: number; host?: string };

export function connect(port: number, host?: string): string {
  return `${port}${host ?? ""}`;
}

export function start(): string {
  const o: Options = { port: 80, host: null }; //~ null-into-optional
  return connect(o.port, null); //~ null-into-optional
}

export function plain(): string {
  const o: Options = { port: 80 };
  return connect(o.port);
}
