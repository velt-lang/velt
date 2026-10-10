// Unit tests of the debugger-independent part of the `velt` debug type (`npm test`).

import * as assert from "assert/strict";
import * as path from "path";
import { test } from "node:test";
import {
  buildArgs,
  concreteConfig,
  diagnosticsByFile,
  failureMessages,
  importScript,
  parseBuildResult,
  pickEngine,
  VeltConfig,
} from "../debugConfig";

const all = () => true;
const only = (...ids: string[]) => (id: string) => ids.includes(id);

test("auto picks CodeLLDB, then lldb-dap, then the C/C++ extension", () => {
  assert.equal(pickEngine("auto", all), "codelldb");
  assert.equal(pickEngine(undefined, only("llvm-vs-code-extensions.lldb-dap", "ms-vscode.cpptools")), "lldb-dap");
  assert.equal(pickEngine("auto", only("ms-vscode.cpptools")), "cpptools");
  assert.equal(pickEngine("auto", only()), undefined);
});

test("a configured engine is used only when installed", () => {
  assert.equal(pickEngine("cpptools", all), "cpptools");
  assert.equal(pickEngine("lldb-dap", only("vadimcn.vscode-lldb")), undefined);
});

test("build arguments: the package, or one file", () => {
  const base: VeltConfig = { type: "velt", request: "launch", name: "Debug" };
  assert.deepEqual(buildArgs(base), ["build", "--json"]);
  assert.deepEqual(buildArgs({ ...base, file: "/w/app.vlt" }), ["build", "/w/app.vlt", "--json"]);
});

const failed = JSON.stringify({
  executable: null,
  debugInfo: true,
  lldbScript: null,
  errors: 2,
  warnings: 0,
  diagnostics: [
    {
      severity: "error",
      message: "mismatched types",
      location: { file: "src/main.vlt", line: 2, column: 5, endLine: 2, endColumn: 9 },
      labels: [],
      notes: ["expected string, found f64"],
    },
    { severity: "error", message: "could not find the runtime", location: null, labels: [], notes: [] },
  ],
});

test("build results: diagnostics by absolute file, failures without a location", () => {
  const result = parseBuildResult(failed);
  assert.equal(result.errors, 2);
  const byFile = diagnosticsByFile(result, "/w");
  assert.deepEqual([...byFile.keys()], [path.resolve("/w", "src/main.vlt")]);
  assert.deepEqual(failureMessages(result), ["could not find the runtime"]);
  assert.throws(() => parseBuildResult("error: no such command"));
  assert.throws(() => parseBuildResult("{}"));
});

const launch: VeltConfig = {
  type: "velt",
  request: "launch",
  name: "Debug",
  args: ["--port", "8080"],
  cwd: "/w",
  env: { RUST_LOG: "debug" },
};

test("CodeLLDB loads the formatters and evaluates simple expressions", () => {
  const c = concreteConfig("codelldb", launch, "/w/target/velt/app", "/opt/velt/share/velt/lldb/velt_lldb.py", "linux");
  assert.equal(c.type, "lldb");
  assert.equal(c.program, "/w/target/velt/app");
  assert.deepEqual(c.args, ["--port", "8080"]);
  assert.deepEqual(c.initCommands, ['command script import "/opt/velt/share/velt/lldb/velt_lldb.py"']);
  assert.equal(c.expressions, "simple");
  assert.deepEqual(c.env, { RUST_LOG: "debug" });
});

test("lldb-dap takes the environment as KEY=VALUE strings", () => {
  const c = concreteConfig("lldb-dap", launch, "/w/app", null, "darwin");
  assert.equal(c.type, "lldb-dap");
  assert.deepEqual(c.env, ["RUST_LOG=debug"]);
  assert.deepEqual(c.initCommands, []);
});

test("cpptools: lldb on macOS, gdb on Linux, the Visual Studio debugger on Windows", () => {
  assert.equal(concreteConfig("cpptools", launch, "/w/app", null, "darwin").MIMode, "lldb");
  const linux = concreteConfig("cpptools", launch, "/w/app", null, "linux");
  assert.equal(linux.type, "cppdbg");
  assert.equal(linux.MIMode, "gdb");
  assert.deepEqual(linux.environment, [{ name: "RUST_LOG", value: "debug" }]);
  assert.equal(concreteConfig("cpptools", launch, "C:\\w\\app.exe", null, "win32").type, "cppvsdbg");
});

test("attach passes the process id", () => {
  const attach: VeltConfig = { type: "velt", request: "attach", name: "Attach", pid: 42 };
  assert.equal(concreteConfig("codelldb", attach, undefined, null, "linux").pid, 42);
  assert.equal(concreteConfig("lldb-dap", attach, undefined, null, "linux").pid, 42);
  assert.equal(concreteConfig("cpptools", attach, undefined, null, "linux").processId, "42");
});

test("script paths are quoted for LLDB", () => {
  assert.equal(importScript('C:\\Velt "x"\\velt_lldb.py'), 'command script import "C:\\\\Velt \\"x\\"\\\\velt_lldb.py"');
});
