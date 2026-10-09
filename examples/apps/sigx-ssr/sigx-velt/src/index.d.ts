import type { Plugin } from "vite";

export interface VeltOptions {
  /** The Velt program to run and build; default: the package entry in package.vlt. */
  entry?: string;
  /** The `velt` executable; default `$VELT`, else `velt` on PATH. */
  bin?: string;
  /** Folders of components shared with the browser; default `["src/shared"]`. */
  shared?: string[];
  /** Build the server after the client (`vite build`); default true. */
  build?: boolean;
  /** `velt build --release`; default true. */
  release?: boolean;
  /** Where the server binary goes; default `dist/server/app`. */
  serverOutFile?: string;
  /** How long a document request waits for the server to pick up a save, in ms; default 2000. */
  holdMs?: number;
}

export default function velt(opts?: VeltOptions): Plugin;
