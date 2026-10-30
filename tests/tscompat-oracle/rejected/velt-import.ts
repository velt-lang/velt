// The Velt standard library has no JavaScript runtime (and, without the `velt` type package,
// no declarations).

import { join } from "velt/path";

export function under(dir: string, file: string): string {
  return join(dir, file);
}
