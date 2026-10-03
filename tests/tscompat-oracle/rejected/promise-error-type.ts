// TypeScript's `Promise` takes one type argument.

export class NotFound extends Error {}

export async function load(id: string): Promise<string, NotFound> {
  if (id === "") {
    throw new NotFound(id);
  }
  return id;
}

export type Loader = (id: string) => Promise<string, NotFound>;
