// `tsc` rejects a decorator on a plain function (TS1206): decorators are for classes and members.
function logged(value: unknown, context: unknown): void {}

@logged
export function f(): void {}
