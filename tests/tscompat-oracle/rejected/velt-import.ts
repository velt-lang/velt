// The Velt standard library has no TypeScript declarations (or JavaScript runtime).

import { join } from "velt:path";

export function under(dir: string, file: string): string {
  return join(dir, file);
}
