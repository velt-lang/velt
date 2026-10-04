// TypeScript types an optional parameter `string | undefined`, which doesn't accept `null`.

export function connect(port: number, host?: string): string {
  return `${port}${host ?? ""}`;
}

export function start(): string {
  return connect(80, null);
}
