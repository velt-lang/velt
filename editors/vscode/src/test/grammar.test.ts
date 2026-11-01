// The TextMate grammar's keyword lists (`npm test`).

import * as assert from "assert/strict";
import * as fs from "fs";
import * as path from "path";
import { test } from "node:test";

const grammar = JSON.parse(
  fs.readFileSync(path.join(__dirname, "..", "..", "syntaxes", "velt.tmLanguage.json"), "utf8"),
);

/** The regexes of every pattern named `scope`, anywhere in the grammar. */
function matches(node: unknown, scope: string, out: RegExp[] = []): RegExp[] {
  if (Array.isArray(node)) {
    node.forEach((n) => matches(n, scope, out));
  } else if (node && typeof node === "object") {
    const o = node as Record<string, unknown>;
    if (o.name === scope && typeof o.match === "string") {
      out.push(new RegExp(o.match.replace(/^\(\?<!\[[^\]]*\]\)/, ""), "g"));
    }
    Object.values(o).forEach((v) => matches(v, scope, out));
  }
  return out;
}

test("member modifiers, `abstract` included, are storage modifiers", () => {
  const modifiers = matches(grammar, "storage.modifier.velt");
  for (const word of ["abstract", "override", "readonly", "static", "async"]) {
    assert.ok(
      modifiers.some((r) => new RegExp(`^(?:${r.source})$`).test(word)),
      `\`${word}\` is not a storage modifier`,
    );
  }
});
