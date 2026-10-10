// The debugger-independent part of the `velt` debug type: which debugger extension to use, the
// `velt build --json` command and its result, and the concrete launch configuration handed to
// the chosen debugger. Nothing here imports `vscode`, so `npm test` runs it under plain Node.

import * as path from "path";

/** A debugger extension the `velt` debug type can drive. */
export type Engine = "codelldb" | "lldb-dap" | "cpptools";

/** The `velt.debug.engine` setting. */
export type EngineSetting = Engine | "auto";

/** Extension ids of each engine; the first installed one is used. */
export const ENGINE_EXTENSIONS: Record<Engine, string[]> = {
  codelldb: ["vadimcn.vscode-lldb"],
  "lldb-dap": ["llvm-vs-code-extensions.lldb-dap"],
  cpptools: ["ms-vscode.cpptools"],
};

/** `auto` tries CodeLLDB (it bundles LLDB), then lldb-dap, then the C/C++ extension. */
export const ENGINE_ORDER: Engine[] = ["codelldb", "lldb-dap", "cpptools"];

/** The extension recommended when no engine is installed. */
export const RECOMMENDED_EXTENSION = "vadimcn.vscode-lldb";

/**
 * The engine to use: the configured one if its extension is installed, or with `auto` the first
 * installed one in [`ENGINE_ORDER`]; `undefined` if there is none.
 */
export function pickEngine(
  setting: EngineSetting | undefined,
  installed: (extensionId: string) => boolean,
): Engine | undefined {
  const candidates = !setting || setting === "auto" ? ENGINE_ORDER : [setting];
  return candidates.find((e) => ENGINE_EXTENSIONS[e]?.some(installed));
}

/** A `velt` launch or attach configuration as written in `launch.json`. */
export interface VeltConfig {
  type: string;
  request: string;
  name: string;
  /** The program to debug; default: what `velt build` produces. */
  program?: string;
  /** A single file to build and debug instead of the package. */
  file?: string;
  args?: string[];
  cwd?: string;
  env?: Record<string, string>;
  /** Build before debugging (default `true`). */
  build?: boolean;
  stopOnEntry?: boolean;
  /** Attach: the process id. */
  pid?: number | string;
  [key: string]: unknown;
}

/** The arguments of `velt build` for `config`: the file (if any) and `--json`. */
export function buildArgs(config: VeltConfig): string[] {
  return config.file ? ["build", config.file, "--json"] : ["build", "--json"];
}

/** A diagnostic of `velt build --json` (the format of `velt check --json`). */
export interface BuildDiagnostic {
  severity: "error" | "warning" | "note";
  message: string;
  location: {
    file: string;
    line: number;
    column: number;
    endLine: number;
    endColumn: number;
  } | null;
  notes: string[];
}

/** What `velt build --json` prints. */
export interface BuildResult {
  executable: string | null;
  debugInfo: boolean | null;
  lldbScript: string | null;
  diagnostics: BuildDiagnostic[];
  errors: number;
  warnings: number;
}

/** Parse the stdout of `velt build --json`; throws if it is not that document. */
export function parseBuildResult(stdout: string): BuildResult {
  const value = JSON.parse(stdout) as Partial<BuildResult>;
  if (typeof value !== "object" || value === null || !Array.isArray(value.diagnostics)) {
    throw new Error("`velt build --json` printed something else");
  }
  return {
    executable: value.executable ?? null,
    debugInfo: value.debugInfo ?? null,
    lldbScript: value.lldbScript ?? null,
    diagnostics: value.diagnostics,
    errors: value.errors ?? 0,
    warnings: value.warnings ?? 0,
  };
}

/** The diagnostics with a location, by absolute file path (relative ones resolve against `cwd`). */
export function diagnosticsByFile(
  result: BuildResult,
  cwd: string,
): Map<string, BuildDiagnostic[]> {
  const byFile = new Map<string, BuildDiagnostic[]>();
  for (const d of result.diagnostics) {
    if (!d.location) {
      continue;
    }
    const file = path.resolve(cwd, d.location.file);
    const list = byFile.get(file) ?? [];
    list.push(d);
    byFile.set(file, list);
  }
  return byFile;
}

/** The messages of the diagnostics without a location (a missing runtime, a linker error). */
export function failureMessages(result: BuildResult): string[] {
  return result.diagnostics
    .filter((d) => !d.location && d.severity === "error")
    .map((d) => d.message);
}

/** The LLDB command that loads the Velt formatters. */
export function importScript(script: string): string {
  return `command script import "${script.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
}

/**
 * The configuration for the chosen engine's debug type: launch `program` (or attach to `pid`)
 * with the LLDB formatters loaded where the engine runs LLDB.
 */
export function concreteConfig(
  engine: Engine,
  config: VeltConfig,
  program: string | undefined,
  lldbScript: string | null,
  platform: NodeJS.Platform,
): Record<string, unknown> {
  const initCommands = lldbScript ? [importScript(lldbScript)] : [];
  const name = config.name;
  const env = config.env ?? {};
  if (config.request === "attach") {
    switch (engine) {
      case "codelldb":
        return { type: "lldb", request: "attach", name, pid: config.pid, initCommands, program };
      case "lldb-dap":
        return { type: "lldb-dap", request: "attach", name, pid: Number(config.pid), initCommands, program };
      case "cpptools":
        return platform === "win32"
          ? { type: "cppvsdbg", request: "attach", name, processId: String(config.pid) }
          : {
              type: "cppdbg",
              request: "attach",
              name,
              processId: String(config.pid),
              program,
              MIMode: platform === "darwin" ? "lldb" : "gdb",
            };
    }
  }
  const common = { name, request: "launch", program, args: config.args ?? [], cwd: config.cwd };
  switch (engine) {
    case "codelldb":
      return {
        ...common,
        type: "lldb",
        env,
        stopOnEntry: config.stopOnEntry ?? false,
        initCommands,
        expressions: "simple",
      };
    case "lldb-dap":
      return {
        ...common,
        type: "lldb-dap",
        env: Object.entries(env).map(([k, v]) => `${k}=${v}`),
        stopOnEntry: config.stopOnEntry ?? false,
        initCommands,
      };
    case "cpptools":
      return platform === "win32"
        ? {
            ...common,
            type: "cppvsdbg",
            environment: Object.entries(env).map(([n, value]) => ({ name: n, value })),
            stopAtEntry: config.stopOnEntry ?? false,
            console: "integratedTerminal",
          }
        : {
            ...common,
            type: "cppdbg",
            environment: Object.entries(env).map(([n, value]) => ({ name: n, value })),
            stopAtEntry: config.stopOnEntry ?? false,
            MIMode: platform === "darwin" ? "lldb" : "gdb",
          };
  }
}
