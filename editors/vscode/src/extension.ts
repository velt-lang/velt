// VS Code client for the Velt language server: launches `velt lsp` over stdio and restarts it when
// `velt.serverPath` changes or on the "Velt: Restart Language Server" command.

import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand("velt.restartServer", () => restart()),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("velt.serverPath")) {
        void restart();
      }
    }),
  );
  await start();
}

export async function deactivate(): Promise<void> {
  await stop();
}

async function restart(): Promise<void> {
  await stop();
  await start();
}

async function start(): Promise<void> {
  const command = serverCommand();
  // TransportKind.stdio makes the client append `--stdio`, which `velt lsp` accepts.
  const serverOptions: ServerOptions = {
    command,
    args: ["lsp"],
    transport: TransportKind.stdio,
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: "file", language: "velt" },
      { scheme: "untitled", language: "velt" },
    ],
  };
  const created = new LanguageClient("velt", "Velt Language Server", serverOptions, clientOptions);
  try {
    await created.start();
    client = created;
  } catch (err) {
    void vscode.window.showErrorMessage(
      `Velt: could not start \`${command} lsp\` (${String(err)}). ` +
        'Install velt or set "velt.serverPath" to the velt executable.',
    );
  }
}

async function stop(): Promise<void> {
  const running = client;
  client = undefined;
  if (running) {
    await running.stop();
  }
}

// The configured executable with `~` and `${workspaceFolder}` expanded.
function serverCommand(): string {
  const configured = vscode.workspace.getConfiguration("velt").get<string>("serverPath") || "velt";
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? "";
  let command = configured.replace("${workspaceFolder}", folder);
  if (command === "~" || command.startsWith("~/") || command.startsWith("~\\")) {
    command = path.join(os.homedir(), command.slice(1));
  }
  return command;
}
