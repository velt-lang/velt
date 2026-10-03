// Prints what the pinned `tsc` reports for a project, one line per diagnostic:
// `<file>\t<line>\t<code>\t<message>` (file relative to the project, `-\t0` for the project as a
// whole). Unlike `tsc --noEmit`, it lists a file's type errors even when the file has syntax
// errors, so the oracle sees every construct, not only the first syntax error.
// Usage: node diagnostics.mjs <tsconfig.json>
import path from "node:path";
import ts from "typescript";

const configPath = path.resolve(process.argv[2]);
const config = ts.getParsedCommandLineOfConfigFile(configPath, {}, {
  ...ts.sys,
  onUnRecoverableConfigFileDiagnostic(d) {
    throw new Error(ts.flattenDiagnosticMessageText(d.messageText, " "));
  },
});
const program = ts.createProgram({ rootNames: config.fileNames, options: config.options });
const root = path.dirname(configPath);
const lines = [];
const print = (file, line, d) => {
  const message = ts.flattenDiagnosticMessageText(d.messageText, " ").replace(/\s+/g, " ");
  lines.push(`${file}\t${line}\t${d.code}\t${message}`);
};
for (const d of [
  ...config.errors,
  ...program.getOptionsDiagnostics(),
  ...program.getGlobalDiagnostics(),
]) {
  print("-", 0, d);
}
for (const sf of program.getSourceFiles()) {
  if (!config.fileNames.includes(sf.fileName)) {
    continue;
  }
  const file = path.relative(root, sf.fileName).split(path.sep).join("/");
  for (const d of [...program.getSyntacticDiagnostics(sf), ...program.getSemanticDiagnostics(sf)]) {
    print(file, sf.getLineAndCharacterOfPosition(d.start ?? 0).line + 1, d);
  }
}
process.stdout.write(lines.map((l) => l + "\n").join(""));
