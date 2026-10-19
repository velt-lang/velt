// Seeds doc comments in std/prelude/*.vlt from the JSDoc of TypeScript's lib.*.d.ts (issue #515).
// Run by hand; the edited prelude files are committed (and reviewed: see below).
//
//   (cd tests/tscompat-oracle && npm ci)   # the pinned `typescript`
//   node scripts/gen-prelude-docs.js        # edits std/prelude/*.vlt in place
//   node scripts/gen-prelude-docs.js --dry  # only prints what it would insert
//
// For every prelude declaration that TypeScript has with the same meaning (`TS_GLOBALS` and
// `TS_MEMBERS` in crates/velt_tscompat/src/typed/prelude.rs, read from that file) and that has no
// doc comment yet, it inserts a `/** … */` with TypeScript's text above the declaration. `@param`
// names are renamed to the Velt parameter at the same position (a TS parameter Velt does not
// have, such as `thisArg`, loses its tag), and the text is wrapped to 100 columns. Existing doc
// comments are never touched. It prints the names it found no TypeScript doc for.
//
// The text is TypeScript's, written for JS: review every inserted comment for Velt's
// differences (docs/std/prelude.md: byte offsets in strings, `i64`/`f64`, `null` for
// `undefined`, no `thisArg`, async timer tasks, thrown error types) before committing it.
// TypeScript's lib is Apache-2.0, © Microsoft Corporation: see NOTICE. Velt never reads .d.ts
// files at build time; only this script does.

"use strict";
const fs = require("fs");
const path = require("path");

const repo = path.join(__dirname, "..");
const ts = require(path.join(repo, "tests/tscompat-oracle/node_modules/typescript"));
const libDir = path.dirname(require.resolve(path.join(repo, "tests/tscompat-oracle/node_modules/typescript")));
const dry = process.argv.includes("--dry");
const WIDTH = 100;

// ---- What TypeScript has: the classification tables of velt_tscompat. ----

const tablesSrc = fs.readFileSync(path.join(repo, "crates/velt_tscompat/src/typed/prelude.rs"), "utf8");

function rustTable(name) {
  const start = tablesSrc.indexOf(`const ${name}:`);
  if (start < 0) throw new Error(`no ${name} in prelude.rs`);
  const open = tablesSrc.indexOf("= &[", start) + 3;
  let depth = 0;
  let i = open;
  for (; i < tablesSrc.length; i++) {
    if (tablesSrc[i] === "[") depth++;
    if (tablesSrc[i] === "]" && --depth === 0) break;
  }
  // Drop `//` comments, which may contain quoted names.
  return tablesSrc.slice(open, i + 1).replace(/\/\/.*$/gm, "");
}

const tsGlobals = new Set([...rustTable("TS_GLOBALS").matchAll(/"([^"]+)"/g)].map((m) => m[1]));
const tsMembers = new Set();
for (const m of rustTable("TS_MEMBERS").matchAll(/\(\s*"([^"]+)",\s*&\[([^\]]*)\]/g)) {
  for (const n of m[2].matchAll(/"([^"]+)"/g)) tsMembers.add(`${m[1]}.${n[1]}`);
}

// The TS interfaces that hold the members of each Velt owner, in the order to look in.
const OWNER_INTERFACES = {
  Array: ["Array", "ReadonlyArray"],
  string: ["String"],
  number: ["Number"],
  Number: ["NumberConstructor"],
  String: ["StringConstructor"],
  Object: ["ObjectConstructor"],
  Date: ["Date", "DateConstructor"],
  Map: ["Map", "ReadonlyMap", "MapConstructor"],
  Math: ["Math"],
  JSON: ["JSON"],
  Error: ["Error", "ErrorConstructor"],
  Iterator: ["Iterator"],
  AsyncIterator: ["AsyncIterator"],
  Generator: ["Generator"],
  AsyncGenerator: ["AsyncGenerator"],
  ArrayIterator: ["ArrayIterator", "IteratorObject", "Iterator"],
  StringIterator: ["StringIterator", "IteratorObject", "Iterator"],
};

// ---- What TypeScript says: JSDoc of interface members and globals in the lib files. ----

const libFiles = fs
  .readdirSync(libDir)
  .filter((f) => /^lib\.(es5|es20(1[5-9]|2[0-3])(\.[a-z.]+)?|dom)\.d\.ts$/.test(f) && !f.includes(".full."))
  .sort();

// `Owner.member` / global name → [{ node, docs }] in lib order.
const members = new Map();
const globals = new Map();
const add = (map, key, node) => {
  const docs = ts.getJSDocCommentsAndTags(node).filter(ts.isJSDoc);
  if (!docs.length) return;
  if (!map.has(key)) map.set(key, []);
  map.get(key).push({ node, doc: docs[docs.length - 1] });
};
for (const f of libFiles) {
  const file = path.join(libDir, f);
  const sf = ts.createSourceFile(file, fs.readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true);
  for (const st of sf.statements) {
    if (ts.isInterfaceDeclaration(st)) {
      add(globals, `interface ${st.name.text}`, st);
      for (const m of st.members) {
        if (m.name && (ts.isIdentifier(m.name) || ts.isStringLiteral(m.name))) {
          add(members, `${st.name.text}.${m.name.text}`, m);
        }
      }
    } else if (ts.isFunctionDeclaration(st) && st.name) {
      add(globals, st.name.text, st);
    } else if (ts.isVariableStatement(st)) {
      for (const d of st.declarationList.declarations) {
        if (ts.isIdentifier(d.name)) {
          // The JSDoc is on the statement.
          const docs = ts.getJSDocCommentsAndTags(d).filter(ts.isJSDoc);
          if (docs.length) {
            if (!globals.has(d.name.text)) globals.set(d.name.text, []);
            globals.get(d.name.text).push({ node: d, doc: docs[docs.length - 1] });
          }
        }
      }
    } else if (ts.isTypeAliasDeclaration(st)) {
      add(globals, `interface ${st.name.text}`, st);
    }
  }
}

function lookupMember(owner, name, isStatic) {
  let ifaces = OWNER_INTERFACES[owner] || [owner];
  // Date's statics (`now`, `parse`, `UTC`) are on DateConstructor.
  if (owner === "Date") ifaces = isStatic ? ["DateConstructor"] : ["Date"];
  for (const i of ifaces) {
    const found = members.get(`${i}.${name}`);
    if (found) return found;
  }
  return null;
}

function lookupGlobal(name) {
  return globals.get(name) || globals.get(`interface ${name}`) || null;
}

// The parameter names of a TS declaration.
function tsParams(node) {
  const decl = ts.isVariableDeclaration(node) ? null : node;
  return decl && decl.parameters ? decl.parameters.map((p) => p.name.getText()) : [];
}

const text = (c) => (c === undefined ? "" : (typeof c === "string" ? c : ts.getTextOfJSDocComment(c)) || "");

// One JSDoc as doc comment lines (unwrapped), with TS parameter names replaced by Velt's.
function convert(entry, veltParams) {
  const rename = new Map();
  tsParams(entry.node).forEach((n, i) => {
    if (i < veltParams.length) rename.set(n, veltParams[i]);
  });
  // Parameter names in the text: only ones that can't be ordinary words (`callbackfn`,
  // `searchString`), so "a string" stays as it is.
  const isCode = (w) => /[A-Z]/.test(w) || /fn$/.test(w);
  const fixNames = (s) =>
    s.replace(/\b[A-Za-z_]\w*\b/g, (w) =>
      rename.has(w) && rename.get(w) !== w && isCode(w) ? `\`${rename.get(w)}\`` : w,
    );
  const out = [];
  const body = text(entry.doc.comment).trim();
  if (body) out.push(...fixNames(body).split("\n"));
  for (const tag of entry.doc.tags || []) {
    const t = tag.tagName.text;
    const c = text(tag.comment).trim();
    if (t === "param") {
      const n = tag.name.getText();
      if (!rename.has(n)) continue;
      out.push(...`@param ${rename.get(n)} - ${fixNames(c).replace(/^-\s*/, "")}`.split("\n"));
    } else if (t === "example") {
      out.push("@example", ...c.split("\n"));
    } else {
      out.push(...`@${t}${c ? " " + fixNames(c) : ""}`.split("\n"));
    }
  }
  return out;
}

// Wraps doc lines into ` * ` lines at `indent` within WIDTH columns; `@example` code verbatim.
function render(lines, indent) {
  const room = WIDTH - indent.length - 3;
  const out = [];
  let inExample = false;
  let inFence = false;
  for (const line of lines) {
    if (line.startsWith("@")) inExample = line === "@example";
    if (line.trim().startsWith("```")) inFence = !inFence;
    if (inExample || inFence || line.trim().startsWith("```") || line.length <= room || line.trim() === "") {
      out.push(line);
      continue;
    }
    let cur = "";
    for (const word of line.split(/\s+/)) {
      if (cur && cur.length + 1 + word.length > room) {
        out.push(cur);
        cur = word;
      } else cur = cur ? `${cur} ${word}` : word;
    }
    if (cur) out.push(cur);
  }
  if (out.length === 1 && indent.length + 7 + out[0].length <= WIDTH) return [`${indent}/** ${out[0]} */`];
  return [`${indent}/**`, ...out.map((l) => (l ? `${indent} * ${l}` : `${indent} *`)), `${indent} */`];
}

// ---- The prelude's declarations (formatted source, so a line scanner suffices). ----

// The owner velt_tscompat names an `extend` target's members by.
function extendOwner(target) {
  const head = target.trim().replace(/^\(/, "");
  if (/\|\s*null\)?$/.test(target.trim())) return "nullable";
  const name = head.match(/^\w+/)[0];
  if (/^(i8|i16|i32|i64|isize|u8|u16|u32|u64|usize|f32|f64|number)$/.test(name)) return "number";
  if (name === "bool" || name === "boolean") return "boolean";
  return name;
}

// The parameter names of the signature starting at `lines[i]`.
function veltParams(lines, i) {
  if (!/^\s*(?:export\s+)?(?:(?:static|async|get|set)\s+)*(?:function\*?\s+)?\w+\s*(?:<[^(]*>)?\(/.test(lines[i])) {
    return [];
  }
  let sig = "";
  for (let j = i; j < lines.length && j < i + 20; j++) {
    sig += lines[j] + "\n";
    if (/\)\s*(:[^{]*)?\{\s*$|\);\s*$/.test(lines[j])) break;
  }
  const open = sig.indexOf("(");
  let depth = 0;
  let cur = "";
  const parts = [];
  for (let k = open + 1; k < sig.length; k++) {
    const ch = sig[k];
    if ("([{<".includes(ch)) depth++;
    if (")]}>".includes(ch)) {
      if (depth === 0) break;
      depth--;
    }
    if (ch === "=" && sig[k + 1] === ">") {
      cur += "=>";
      k++;
      continue;
    }
    if (ch === "," && depth === 0) {
      parts.push(cur);
      cur = "";
    } else cur += ch;
  }
  parts.push(cur);
  return parts
    .map((p) => p.trim().replace(/^(private|public|readonly)\s+/, "").replace(/^\.\.\./, ""))
    .filter(Boolean)
    .map((p) => (p.match(/^\w+/) || [""])[0])
    .filter(Boolean);
}

const hasDoc = (lines, i) => {
  const prev = (lines[i - 1] || "").trim();
  return prev.endsWith("*/") || prev.startsWith("///");
};

const noDoc = [];
let inserted = 0;
const preludeDir = path.join(repo, "std/prelude");
for (const f of fs.readdirSync(preludeDir).filter((f) => f.endsWith(".vlt")).sort()) {
  const file = path.join(preludeDir, f);
  const lines = fs.readFileSync(file, "utf8").split("\n");
  const inserts = []; // [line index, comment lines]
  let owner = null; // the current class or extend block's owner
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    let m;
    if ((m = line.match(/^extend(?:<[^>]*>)?\s+(.+?)\s*\{\s*$/))) {
      owner = extendOwner(m[1]);
      continue;
    }
    if ((m = line.match(/^export\s+(?:class|struct|interface)\s+(\w+)/))) {
      owner = m[1] === "NumberConstructor" ? "Number" : m[1];
      const key = m[1];
      if (tsGlobals.has(key) && !hasDoc(lines, i)) want(i, key, null, lookupGlobal(key), "");
      if (/\{\s*\}\s*$/.test(line)) owner = null;
      continue;
    }
    if (/^\}/.test(line)) {
      owner = null;
      continue;
    }
    if ((m = line.match(/^export\s+(?:async\s+)?(?:function\*?|const|let|type)\s+(\w+)/))) {
      if (tsGlobals.has(m[1]) && !hasDoc(lines, i)) want(i, m[1], null, lookupGlobal(m[1]), "");
      continue;
    }
    if (owner && (m = line.match(/^  ((?:(?:static|readonly|async|get|set)\s+)*)(\w+)\s*[<(:]/))) {
      if (m[2] === "constructor" || /\bprivate\b/.test(line)) continue;
      const key = `${owner}.${m[2]}`;
      if (tsMembers.has(key) && !hasDoc(lines, i)) {
        want(i, key, m[2], lookupMember(owner, m[2], /\bstatic\b/.test(m[1])), "  ");
      }
    }
  }
  function want(i, key, _member, found, indent) {
    if (!found) {
      noDoc.push(`${f}: ${key}`);
      return;
    }
    // The first declaration with a doc (TypeScript's oldest overload).
    const entry = found[0];
    const comment = render(convert(entry, veltParams(lines, i)), indent);
    inserts.push([i, comment, key]);
  }
  for (const [i, comment, key] of inserts.reverse()) {
    lines.splice(i, 0, ...comment);
    inserted++;
    if (dry) console.log(`${f}: ${key}\n${comment.join("\n")}\n`);
  }
  if (!dry && inserts.length) fs.writeFileSync(file, lines.join("\n"));
}

console.error(`inserted ${inserted} doc comments`);
if (noDoc.length) console.error(`no TypeScript doc for:\n  ${noDoc.join("\n  ")}`);
