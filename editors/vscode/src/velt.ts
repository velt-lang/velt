// The `velt` executable the extension runs (language server, builds, debugging).

import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";

// The configured `velt.serverPath` with `~` and `${workspaceFolder}` expanded.
export function veltCommand(): string {
  const configured = vscode.workspace.getConfiguration("velt").get<string>("serverPath") || "velt";
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? "";
  let command = configured.replace("${workspaceFolder}", folder);
  if (command === "~" || command.startsWith("~/") || command.startsWith("~\\")) {
    command = path.join(os.homedir(), command.slice(1));
  }
  return command;
}
