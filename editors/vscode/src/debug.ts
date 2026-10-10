// The `velt` debug type: F5 builds the package (or the open file) with `velt build --json`, shows
// build errors in Problems, and starts the installed debugger extension (CodeLLDB, lldb-dap or the
// C/C++ extension) on the executable, with the Velt LLDB formatters loaded. Also the "Run File",
// "Debug File" and "Generate launch.json" commands. Modeled on rust-analyzer's debug support.

import * as cp from "child_process";
import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import {
  BuildResult,
  buildArgs,
  concreteConfig,
  diagnosticsByFile,
  ENGINE_EXTENSIONS,
  EngineSetting,
  failureMessages,
  parseBuildResult,
  parsePs,
  parseTasklist,
  pickEngine,
  sortProcesses,
  RECOMMENDED_EXTENSION,
  VeltConfig,
} from "./debugConfig";
import { veltCommand } from "./velt";

const DEBUG_TYPE = "velt";

let buildDiagnostics: vscode.DiagnosticCollection | undefined;

export function registerDebugging(context: vscode.ExtensionContext): void {
  buildDiagnostics = vscode.languages.createDiagnosticCollection("velt build");
  const provider = new VeltConfigurationProvider();
  context.subscriptions.push(
    buildDiagnostics,
    vscode.debug.registerDebugConfigurationProvider(DEBUG_TYPE, provider),
    vscode.debug.registerDebugConfigurationProvider(
      DEBUG_TYPE,
      provider,
      vscode.DebugConfigurationProviderTriggerKind.Dynamic,
    ),
    vscode.commands.registerCommand("velt.debugFile", (uri?: vscode.Uri | string) => debugFile(fileOf(uri))),
    vscode.commands.registerCommand("velt.runFile", (uri?: vscode.Uri | string) => runFile(fileOf(uri))),
    vscode.commands.registerCommand("velt.generateLaunchJson", () => generateLaunchJson()),
    // `${command:pickProcess}` in a `velt` configuration (package.json maps it here).
    vscode.commands.registerCommand("velt.pickProcess", () => pickProcess()),
  );
}

class VeltConfigurationProvider implements vscode.DebugConfigurationProvider {
  // "Debug package", "Debug current file" and "Attach to process" (the debug view's dropdown and
  // the initial launch.json).
  provideDebugConfigurations(folder: vscode.WorkspaceFolder | undefined): vscode.DebugConfiguration[] {
    const configs: vscode.DebugConfiguration[] = [];
    if (folder && isPackage(folder)) {
      configs.push({ type: DEBUG_TYPE, request: "launch", name: "Debug package" });
    }
    configs.push(
      { type: DEBUG_TYPE, request: "launch", name: "Debug current file", file: "${file}" },
      { type: DEBUG_TYPE, request: "attach", name: "Attach to process", pid: "${command:pickProcess}" },
    );
    return configs;
  }

  // F5 without a launch.json: debug the package, or else the open file.
  resolveDebugConfiguration(
    folder: vscode.WorkspaceFolder | undefined,
    config: vscode.DebugConfiguration,
  ): vscode.DebugConfiguration | undefined {
    if (!config.type && !config.request && !config.name) {
      config.type = DEBUG_TYPE;
      config.request = "launch";
      config.name = "Debug";
    }
    if (config.request === "launch" && !config.file && !config.program) {
      const target = folder ?? activeFolder();
      if (!target || !isPackage(target)) {
        const file = activeVeltFile();
        if (!file) {
          void vscode.window.showErrorMessage(
            "Velt: open a .vlt file or a folder with package.vlt to debug it.",
          );
          return undefined;
        }
        config.file = file;
      }
    }
    return config;
  }

  // Build, then start the real debugger; this session itself is cancelled (`undefined`).
  async resolveDebugConfigurationWithSubstitutedVariables(
    folder: vscode.WorkspaceFolder | undefined,
    config: vscode.DebugConfiguration,
    token?: vscode.CancellationToken,
  ): Promise<vscode.DebugConfiguration | undefined> {
    const velt = config as VeltConfig;
    const engine = pickEngine(
      vscode.workspace.getConfiguration("velt").get<EngineSetting>("debug.engine"),
      (id) => vscode.extensions.getExtension(id) !== undefined,
    );
    if (!engine) {
      await offerDebugger();
      return undefined;
    }
    const target = folder ?? activeFolder();
    const cwd = velt.cwd ?? target?.uri.fsPath ?? (velt.file ? path.dirname(velt.file) : process.cwd());
    let program = velt.program;
    let lldbScript: string | null = null;
    if (velt.request === "launch" && velt.build !== false) {
      const built = await build(velt, cwd, token);
      if (!built) {
        return undefined;
      }
      program = program ?? built.executable ?? undefined;
      lldbScript = built.lldbScript;
      if (built.debugInfo === false) {
        void vscode.window.showWarningMessage(
          "Velt: this build has no line information, so breakpoints will not bind. On Windows the " +
            'default build has none yet: add "buildArgs": ["--backend", "llvm"] (needs clang) to the ' +
            "launch configuration.",
        );
      }
    }
    if (velt.request === "launch" && !program) {
      void vscode.window.showErrorMessage("Velt: nothing to debug (set `program`, or `build: true`).");
      return undefined;
    }
    const concrete = concreteConfig(engine, { ...velt, cwd }, program, lldbScript, process.platform);
    await vscode.debug.startDebugging(target, concrete as vscode.DebugConfiguration);
    return undefined;
  }
}

// `velt build --json` with progress; build errors go to Problems. `undefined` if it failed.
async function build(
  config: VeltConfig,
  cwd: string,
  token?: vscode.CancellationToken,
): Promise<BuildResult | undefined> {
  buildDiagnostics?.clear();
  const command = veltCommand();
  const args = buildArgs(config);
  const output = await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Window, title: "Velt: building for the debugger" },
    () => run(command, args, cwd, token),
  );
  if (output.error) {
    void vscode.window.showErrorMessage(
      `Velt: could not run \`${command} build\` (${output.error}). ` +
        'Install velt or set "velt.serverPath" to the velt executable.',
    );
    return undefined;
  }
  let result: BuildResult;
  try {
    result = parseBuildResult(output.stdout);
  } catch {
    void vscode.window.showErrorMessage(
      `Velt: \`velt build --json\` failed; is this velt too old? ${output.stderr.trim()}`,
    );
    return undefined;
  }
  showDiagnostics(result, cwd);
  if (result.errors > 0 || !result.executable) {
    const failures = failureMessages(result);
    const message = failures.length > 0 ? failures.join("; ") : `${result.errors} error(s)`;
    const choice = await vscode.window.showErrorMessage(`Velt: build failed: ${message}`, "Show Problems");
    if (choice === "Show Problems") {
      void vscode.commands.executeCommand("workbench.actions.view.problems");
    }
    return undefined;
  }
  return result;
}

interface Output {
  stdout: string;
  stderr: string;
  error?: string;
}

function run(command: string, args: string[], cwd: string, token?: vscode.CancellationToken): Promise<Output> {
  return new Promise((resolve) => {
    const child = cp.execFile(command, args, { cwd, maxBuffer: 64 * 1024 * 1024 }, (err, stdout, stderr) => {
      // A failed build exits with 1 but still prints the JSON document.
      const spawnFailed = err && typeof (err as NodeJS.ErrnoException).code === "string";
      resolve({ stdout, stderr, error: spawnFailed ? err.message : undefined });
    });
    token?.onCancellationRequested(() => child.kill());
  });
}

function showDiagnostics(result: BuildResult, cwd: string): void {
  for (const [file, diags] of diagnosticsByFile(result, cwd)) {
    buildDiagnostics?.set(
      vscode.Uri.file(file),
      diags.map((d) => {
        const loc = d.location!;
        const range = new vscode.Range(loc.line - 1, loc.column - 1, loc.endLine - 1, loc.endColumn - 1);
        const severity =
          d.severity === "error"
            ? vscode.DiagnosticSeverity.Error
            : d.severity === "warning"
              ? vscode.DiagnosticSeverity.Warning
              : vscode.DiagnosticSeverity.Information;
        const message = [d.message, ...d.notes].join("\n");
        const diagnostic = new vscode.Diagnostic(range, message, severity);
        diagnostic.source = "velt build";
        return diagnostic;
      }),
    );
  }
}

async function offerDebugger(): Promise<void> {
  const install = "Install CodeLLDB";
  const choice = await vscode.window.showInformationMessage(
    "Velt: debugging needs a debugger extension. CodeLLDB is recommended (it includes LLDB); " +
      `lldb-dap and the C/C++ extension also work (${Object.values(ENGINE_EXTENSIONS).flat().join(", ")}).`,
    install,
  );
  if (choice === install) {
    await vscode.commands.executeCommand("workbench.extensions.installExtension", RECOMMENDED_EXTENSION);
  }
}

// The file a command was given: a `Uri` from the editor title, a URI string from the language
// server's code lens, or else the active editor's file.
function fileOf(uri: vscode.Uri | string | undefined): string | undefined {
  if (typeof uri === "string") {
    return vscode.Uri.parse(uri).fsPath;
  }
  return uri?.fsPath ?? activeVeltFile();
}

// "Debug File": the file of the editor (or of the CodeLens) as a single-file program.
async function debugFile(file: string | undefined): Promise<void> {
  if (!file) {
    void vscode.window.showErrorMessage("Velt: open a .vlt file to debug it.");
    return;
  }
  await vscode.debug.startDebugging(vscode.workspace.getWorkspaceFolder(vscode.Uri.file(file)), {
    type: DEBUG_TYPE,
    request: "launch",
    name: `Debug ${path.basename(file)}`,
    file,
  });
}

// "Run File": `velt run <file>` in a terminal. The terminal runs velt itself, not a shell, so
// nothing in the file name is interpreted.
async function runFile(file: string | undefined): Promise<void> {
  if (!file) {
    void vscode.window.showErrorMessage("Velt: open a .vlt file to run it.");
    return;
  }
  const doc = vscode.workspace.textDocuments.find((d) => d.uri.scheme === "file" && d.uri.fsPath === file);
  if (doc?.isDirty) {
    await doc.save();
  }
  const folder = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(file));
  const terminal = vscode.window.createTerminal({
    name: `Velt: ${path.basename(file)}`,
    cwd: folder?.uri.fsPath ?? path.dirname(file),
    shellPath: veltCommand(),
    shellArgs: ["run", file],
  });
  terminal.show();
}

// The attach picker: running processes, Velt programs (`target/velt/...`) first; the pid.
async function pickProcess(): Promise<string | undefined> {
  const windows = process.platform === "win32";
  const output = windows
    ? await run("tasklist", ["/fo", "csv", "/nh"], process.cwd())
    : await run("ps", ["-A", "-o", "pid=,args="], process.cwd());
  if (output.error) {
    void vscode.window.showErrorMessage(`Velt: cannot list processes (${output.error}).`);
    return undefined;
  }
  const entries = sortProcesses(windows ? parseTasklist(output.stdout) : parsePs(output.stdout));
  const picked = await vscode.window.showQuickPick(
    entries.map((e) => ({ label: e.command, description: String(e.pid), pid: e.pid })),
    { placeHolder: "Attach to which process?", matchOnDescription: true },
  );
  return picked ? String(picked.pid) : undefined;
}

// "Generate launch.json": `velt init --editor vscode` in the workspace folder (it keeps files that
// exist), then open the launch.json.
async function generateLaunchJson(): Promise<void> {
  const folder = activeFolder();
  if (!folder) {
    void vscode.window.showErrorMessage("Velt: open a folder first.");
    return;
  }
  const output = await run(
    veltCommand(),
    ["init", "--editor", "vscode", "--dir", folder.uri.fsPath],
    folder.uri.fsPath,
  );
  if (output.error) {
    void vscode.window.showErrorMessage(`Velt: could not run velt (${output.error}).`);
    return;
  }
  const launch = path.join(folder.uri.fsPath, ".vscode", "launch.json");
  if (!fs.existsSync(launch)) {
    void vscode.window.showErrorMessage(`Velt: \`velt init --editor vscode\` failed: ${output.stderr.trim()}`);
    return;
  }
  await vscode.window.showTextDocument(vscode.Uri.file(launch));
}

function isPackage(folder: vscode.WorkspaceFolder): boolean {
  return fs.existsSync(path.join(folder.uri.fsPath, "package.vlt"));
}

function activeFolder(): vscode.WorkspaceFolder | undefined {
  const doc = vscode.window.activeTextEditor?.document;
  return (doc && vscode.workspace.getWorkspaceFolder(doc.uri)) ?? vscode.workspace.workspaceFolders?.[0];
}

function activeVeltFile(): string | undefined {
  const doc = vscode.window.activeTextEditor?.document;
  if (doc && doc.uri.scheme === "file" && /\.(vlt|ts|tsx)$/.test(doc.fileName)) {
    return doc.fileName;
  }
  return undefined;
}
