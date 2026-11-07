// Generates std/lang/dom/types.velt, std/lang/dom/index.velt, std/prelude/global/dom.velt and
// crates/velt_sema/src/body/expr/dom_values.rs: the part of TypeScript's `lib.dom.d.ts` that the
// names in scripts/dom-subset/names.txt reach, from the TypeScript pinned in tests/tscompat-oracle.
//
//   (cd tests/tscompat-oracle && npm ci) && node scripts/dom-subset/generate.mjs
//
// A name in names.txt is a type (`HTMLAnchorElement`: the interface and what it extends, with no
// members of its own beyond those listed), a member (`Element.remove`: the member, found on the
// type or on what it extends, declared where `lib.dom` declares it) or all of a type's own members
// (`Event.*`). What a member's signature names is reached the same way, as a type with no members
// of its own, so the output grows only as far as the list asks. Each step is described where it
// is made; the shapes Velt doesn't take yet become what Velt takes:
//
// - an overloaded method keeps its last signature, the most general one in `lib.dom`;
// - an accessor pair is a field of the getter's type (`readonly` without a setter);
// - a method a base already declares is left to the base (`lib.dom` narrows `addEventListener`'s
//   event map per interface; Velt takes the base's string form);
// - an interface of index signatures only (`DOMStringMap`) is a `Record`;
// - an index signature next to members, a `this` parameter, conditional, mapped and template
//   literal types are left out, and a type outside the subset that Velt has no global for is
//   `any`.
import { createRequire } from "node:module";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const oracle = path.join(repo, "tests/tscompat-oracle");
const ts = createRequire(path.join(oracle, "package.json"))("typescript");
const libFile = path.join(oracle, "node_modules/typescript/lib/lib.dom.d.ts");
const here = path.join(repo, "scripts/dom-subset");
const check = process.argv.includes("--check");

// Types Velt has in every program, or as std globals of their own: kept as written.
const KEEP = new Set([
  "Array", "ReadonlyArray", "Promise", "PromiseLike", "Record", "Partial", "Readonly", "Map", "Set",
  "Error", "Date", "RegExp", "Iterable", "IterableIterator", "Function", "Object",
]);
// `lib.dom` names std declares itself (fetch, URL, streams, `console`): never generated here.
const STD = new Set(
  fs.readFileSync(path.join(here, "std-names.txt"), "utf8").split(/\s+/).filter(Boolean),
);

const sf = ts.createSourceFile(libFile, fs.readFileSync(libFile, "utf8"), ts.ScriptTarget.Latest, true);
const ifaces = new Map(); // name -> [InterfaceDeclaration]
const aliases = new Map(); // name -> TypeAliasDeclaration
const vars = new Set(); // names `lib.dom` declares as values
for (const n of sf.statements) {
  if (ts.isInterfaceDeclaration(n)) (ifaces.get(n.name.text) ?? ifaces.set(n.name.text, []).get(n.name.text)).push(n);
  else if (ts.isTypeAliasDeclaration(n)) aliases.set(n.name.text, n);
  else if (ts.isVariableStatement(n)) for (const d of n.declarationList.declarations) vars.add(d.name.getText(sf));
  else if (ts.isFunctionDeclaration(n) && n.name) vars.add(n.name.text);
}

const memberName = (m) => (m.name ? m.name.getText(sf) : m.kind === ts.SyntaxKind.CallSignature ? "()" : null);
const ownMembers = (name) => (ifaces.get(name) ?? []).flatMap((d) => [...d.members]);
const heritage = (name) =>
  (ifaces.get(name) ?? []).flatMap((d) => (d.heritageClauses ?? []).flatMap((h) => [...h.types]));
const bases = (name) => heritage(name).map((t) => t.expression.getText(sf));
// An options bag (only optional fields) or a function type (a call signature) is all of its
// members: without them a literal or a closure wouldn't convert to it.
const whole = (name) => {
  const ms = ownMembers(name);
  return ms.length > 0 && (ms.some((m) => m.kind === ts.SyntaxKind.CallSignature) ||
    ms.every((m) => m.kind === ts.SyntaxKind.PropertySignature && m.questionToken));
};

// What the list asks for: type -> set of member names ("*": all of its own).
const want = new Map();
const reach = (t) => {
  if (!want.has(t)) {
    want.set(t, new Set());
    if (whole(t)) for (const m of ownMembers(t)) want.get(t).add(memberName(m));
    for (const b of bases(t)) if (!STD.has(b)) reach(b);
  }
};
const unknown = [];
// The interface that declares `member` for type `t`: `t` or the first base that does.
const owner = (t, member, seen = new Set()) => {
  if (seen.has(t)) return null;
  seen.add(t);
  if (ownMembers(t).some((m) => memberName(m) === member)) return t;
  for (const b of bases(t)) {
    const o = owner(b, member, seen);
    if (o) return o;
  }
  return null;
};
const list = fs.readFileSync(path.join(here, "names.txt"), "utf8").split("\n")
  .map((l) => l.replace(/#.*/, "").trim()).filter(Boolean);
for (const entry of list) {
  const [t, member] = entry.split(".");
  if (!ifaces.has(t) && !aliases.has(t)) { unknown.push(entry); continue; }
  if (aliases.has(t)) { reach(t); continue; }
  reach(t);
  if (member === "*") {
    for (const m of ownMembers(t)) if (memberName(m)) want.get(t).add(memberName(m));
  } else if (member) {
    const o = owner(t, member);
    if (!o) { unknown.push(entry); continue; }
    reach(o);
    want.get(o).add(member);
  }
}
if (unknown.length) {
  console.error(`not in lib.dom.d.ts: ${unknown.join(", ")}`);
  process.exit(1);
}

// Printing types. `scope` holds the type parameters in scope; a name of `lib.dom` outside std is
// reached (and so emitted), anything else Velt doesn't have is `any`.
const notes = new Map();
const note = (what) => notes.set(what, (notes.get(what) ?? 0) + 1);
function ty(n, scope) {
  if (!n) return "any";
  switch (n.kind) {
    case ts.SyntaxKind.StringKeyword: return "string";
    case ts.SyntaxKind.NumberKeyword: return "number";
    case ts.SyntaxKind.BooleanKeyword: return "boolean";
    case ts.SyntaxKind.AnyKeyword: return "any";
    case ts.SyntaxKind.UnknownKeyword: return "unknown";
    case ts.SyntaxKind.VoidKeyword: return "void";
    case ts.SyntaxKind.UndefinedKeyword: return "undefined";
    case ts.SyntaxKind.NeverKeyword: return "never";
    case ts.SyntaxKind.ObjectKeyword: return "object";
    case ts.SyntaxKind.BigIntKeyword: return "bigint";
    case ts.SyntaxKind.SymbolKeyword: return "symbol";
    case ts.SyntaxKind.LiteralType: return n.literal.getText(sf);
    case ts.SyntaxKind.ParenthesizedType: return `(${ty(n.type, scope)})`;
    case ts.SyntaxKind.ArrayType: return `${ty(n.elementType, scope)}[]`;
    case ts.SyntaxKind.TupleType: return `[${n.elements.map((e) => ty(e, scope)).join(", ")}]`;
    case ts.SyntaxKind.UnionType: return unionOf(n.types, scope);
    case ts.SyntaxKind.IntersectionType: return n.types.map((t) => ty(t, scope)).join(" & ");
    case ts.SyntaxKind.TypeOperator:
      return n.operator === ts.SyntaxKind.ReadonlyKeyword ? `readonly ${ty(n.type, scope)}` : (note("keyof"), "any");
    case ts.SyntaxKind.FunctionType:
      return `(${params(n.parameters, scope)}) => ${ty(n.type, scope)}`;
    case ts.SyntaxKind.TypeLiteral:
      return `{ ${n.members.map((m) => member(m, scope, "")).filter(Boolean).join(" ")} }`;
    case ts.SyntaxKind.TypeReference: {
      const name = n.typeName.getText(sf);
      const args = n.typeArguments ? `<${n.typeArguments.map((a) => ty(a, scope)).join(", ")}>` : "";
      if (scope.defaults?.has(name)) return ty(scope.defaults.get(name), scope);
      if (scope.has(name) || KEEP.has(name)) return name + args;
      if ((ifaces.has(name) || aliases.has(name)) && !STD.has(name)) {
        reach(name);
        return name + args;
      }
      note(`any for ${name}`);
      return "any";
    }
    default:
      note(`any for ${ts.SyntaxKind[n.kind]}`);
      return "any";
  }
}
// An interface that is only a call signature (`EventListener`): a function type.
function callable(name) {
  const ms = ownMembers(name);
  return ifaces.has(name) && !bases(name).length && ms.length > 0 &&
    ms.every((m) => m.kind === ts.SyntaxKind.CallSignature);
}
// Does type node `n` name a function type (through aliases)?
function isFn(n) {
  if (ts.isParenthesizedTypeNode(n)) return isFn(n.type);
  if (ts.isFunctionTypeNode(n)) return true;
  if (!ts.isTypeReferenceNode(n)) return false;
  const name = n.typeName.getText(sf);
  if (callable(name)) return true;
  const a = aliases.get(name);
  return !!a && ts.isUnionTypeNode(a.type) && a.type.types.some(isFn);
}
const NOTHING = new Set([ts.SyntaxKind.NullKeyword, ts.SyntaxKind.UndefinedKeyword]);
const isNothing = (n) => ts.isLiteralTypeNode(n) ? NOTHING.has(n.literal.kind) : NOTHING.has(n.kind);
// A union's members. Next to a function type, only the function types and `null`/`undefined`
// are kept (`EventListener | EventListenerObject`): Velt types a closure's parameters from a
// function type, not from a union with objects.
function unionOf(types, scope) {
  let ts_ = [...types];
  if (ts_.some(isFn)) {
    const kept = ts_.filter((t) => isFn(t) || isNothing(t));
    if (kept.length < ts_.length) note("union: the function type kept, the objects left out");
    ts_ = kept;
  }
  // A member that is itself such a union (an alias) is flattened by printing it.
  const parts = ts_.map((t) => ty(t, scope));
  if (parts.includes("any")) return "any";
  return [...new Set(parts)].join(" | ");
}
function params(ps, scope) {
  return ps
    .filter((p) => p.name.getText(sf) !== "this")
    .map((p) => {
      let name = p.name.getText(sf);
      // Names Velt refuses for a parameter (`arguments` in strict mode, `async`).
      if (name === "arguments" || name === "async") name = `${name}_`;
      return `${p.dotDotDotToken ? "..." : ""}${name}${p.questionToken ? "?" : ""}: ${ty(p.type, scope)}`;
    })
    .join(", ");
}
// The type parameters of method `m` that a parameter mentions; one no parameter mentions has
// nothing to be inferred from (`querySelector<E extends Element = Element>(selectors: string)`),
// so it is its default, as `tsc` reads a call without type arguments.
function unmentioned(m, scope) {
  const tps = m.typeParameters;
  if (!tps) return tps;
  const mentioned = new Set();
  const walk = (n) => { if (ts.isTypeReferenceNode(n)) mentioned.add(n.typeName.getText(sf)); ts.forEachChild(n, walk); };
  for (const p of m.parameters) if (p.type) walk(p.type);
  const keep = [];
  for (const t of tps) {
    if (mentioned.has(t.name.text) || !(t.default ?? t.constraint)) keep.push(t);
    else { scope.defaults.set(t.name.text, t.default ?? t.constraint); note("type parameter no parameter mentions: its default"); }
  }
  return keep.length ? keep : undefined;
}
function typeParams(tps, scope) {
  if (!tps) return "";
  for (const t of tps) scope.add(t.name.text);
  return `<${tps.map((t) => `${t.name.text}${t.constraint ? ` extends ${ty(t.constraint, scope)}` : ""}${t.default ? ` = ${ty(t.default, scope)}` : ""}`).join(", ")}>`;
}
// One member, as Velt declares it ("" when left out).
function member(m, outer, indent) {
  const scope = new Set(outer);
  scope.defaults = new Map();
  const name = memberName(m);
  const ro = m.modifiers?.some((x) => x.kind === ts.SyntaxKind.ReadonlyKeyword) ? "readonly " : "";
  switch (m.kind) {
    case ts.SyntaxKind.PropertySignature:
      return `${indent}${ro}${name}${m.questionToken ? "?" : ""}: ${ty(m.type, scope)};`;
    case ts.SyntaxKind.MethodSignature: {
      const tp = typeParams(unmentioned(m, scope), scope);
      return `${indent}${name}${m.questionToken ? "?" : ""}${tp}(${params(m.parameters, scope)}): ${ty(m.type, scope)};`;
    }
    case ts.SyntaxKind.CallSignature: {
      const tp = typeParams(m.typeParameters, scope);
      return `${indent}${tp}(${params(m.parameters, scope)}): ${ty(m.type, scope)};`;
    }
    case ts.SyntaxKind.IndexSignature:
      note("index signature left out");
      return "";
    default:
      note(`member left out: ${ts.SyntaxKind[m.kind]}`);
      return "";
  }
}
// The declaration of member `name` of interface `t`: the last overload of a method, a field for
// an accessor pair.
function memberOf(t, name, scope) {
  const ms = ownMembers(t).filter((m) => memberName(m) === name);
  const get = ms.find((m) => m.kind === ts.SyntaxKind.GetAccessor);
  if (get) {
    const set = ms.some((m) => m.kind === ts.SyntaxKind.SetAccessor);
    return `  ${set ? "" : "readonly "}${name}: ${ty(get.type, new Set(scope))};`;
  }
  const sigs = ms.filter((m) => m.kind === ts.SyntaxKind.MethodSignature || m.kind === ts.SyntaxKind.CallSignature);
  if (sigs.length > 1) note("overloads: last kept");
  return member(sigs.at(-1) ?? ms.at(-1), scope, "  ");
}
// The members of `t`'s bases (all of `lib.dom`'s, transitively) that are emitted.
function inheritedWanted(t, seen = new Set()) {
  const out = new Set();
  for (const b of bases(t)) {
    if (seen.has(b)) continue;
    seen.add(b);
    for (const m of want.get(b) ?? []) out.add(m);
    for (const m of inheritedWanted(b, seen)) out.add(m);
  }
  return out;
}

// Emit until nothing new is reached (printing a member reaches the types it names).
const emitted = new Map();
for (let changed = true; changed; ) {
  changed = false;
  for (const t of [...want.keys()]) {
    if (emitted.has(t)) continue;
    changed = true;
    emitted.set(t, null);
  }
  for (const t of [...emitted.keys()]) {
    if (emitted.get(t) !== null) continue;
    emitted.set(t, emit(t));
  }
}
// The bases of `t` that are emitted, with their type arguments: a base that isn't is replaced by
// its own.
function emittedBases(t, seen = new Set()) {
  const out = [];
  const scope = new Set((ifaces.get(t)[0].typeParameters ?? []).map((p) => p.name.text));
  for (const h of heritage(t)) {
    const b = h.expression.getText(sf);
    if (seen.has(b)) continue;
    seen.add(b);
    const args = h.typeArguments ? `<${h.typeArguments.map((a) => ty(a, scope)).join(", ")}>` : "";
    if (emitted.has(b)) out.push(b + args);
    else if (!STD.has(b)) out.push(...emittedBases(b, seen));
  }
  return [...new Set(out)];
}
function emit(t) {
  if (aliases.has(t)) {
    const a = aliases.get(t);
    const scope = new Set();
    const tp = typeParams(a.typeParameters, scope);
    return `export type ${t}${tp} = ${ty(a.type, scope)};`;
  }
  const decl = ifaces.get(t)[0];
  const all = ownMembers(t);
  if (callable(t)) {
    const c = all.filter((m) => m.kind === ts.SyntaxKind.CallSignature).at(-1);
    const scope = new Set();
    note("function interface as a function type");
    return `export type ${t} = (${params(c.parameters, scope)}) => ${ty(c.type, scope)};`;
  }
  if (all.length && all.every((m) => m.kind === ts.SyntaxKind.IndexSignature)) {
    const s = all[0];
    note("index-only interface as Record");
    return `export type ${t} = Record<${ty(s.parameters[0].type, new Set())}, ${ty(s.type, new Set())}>;`;
  }
  const scope = new Set();
  const tp = typeParams(decl.typeParameters, scope);
  const inherited = inheritedWanted(t);
  const lines = [];
  for (const m of want.get(t)) {
    const isMethod = ownMembers(t).some((x) => memberName(x) === m && x.kind === ts.SyntaxKind.MethodSignature);
    if (isMethod && inherited.has(m)) { note("method a base declares: left to the base"); continue; }
    const line = memberOf(t, m, scope);
    if (line) lines.push(line);
  }
  return { tp, lines };
}
const order = [...emitted.keys()].sort();
const text = order.map((t) => {
  const e = emitted.get(t);
  if (typeof e === "string") return e;
  const ext = emittedBases(t);
  const head = `export interface ${t}${e.tp}${ext.length ? ` extends ${ext.join(", ")}` : ""}`;
  return e.lines.length ? `${head} {\n${e.lines.join("\n")}\n}` : `${head} {}`;
}).join("\n");

const header = (mod) =>
  `${mod}\n// Generated by scripts/dom-subset/generate.mjs from TypeScript ${ts.version}'s lib.dom.d.ts and\n// scripts/dom-subset/names.txt: do not edit, change the list and run the script.\n`;
const files = new Map();
files.set("std/lang/dom/types.velt", `${header("// velt/lang/dom (internal): the types of TypeScript's `lib.dom` that code shared between server and\n// browser names. Velt has no DOM, so no value of these types is made here: they describe what such\n// code reads on a value that comes from elsewhere, and cost nothing in a program that never names\n// one.")}\n${text}\n`);
files.set("std/lang/dom/index.velt", `${header("// velt/lang/dom: the types of TypeScript's `lib.dom` that code shared with the browser names.")}\nexport {\n${order.map((t) => `  ${t},`).join("\n")}\n} from "velt/lang/dom/types";\n`);
files.set("std/prelude/global/dom.velt", `${header("// Global (loaded when a module names one of these): the types of TypeScript's `lib.dom` that code\n// shared between server and browser names. Types only: Velt has no DOM.")}\nexport {\n${order.map((t) => `  ${t},`).join("\n")}\n} from "velt/lang/dom";\n`);
const values = order.filter((t) => vars.has(t));
files.set("crates/velt_sema/src/body/expr/dom_values.rs", `//! Generated by scripts/dom-subset/generate.mjs from TypeScript ${ts.version}'s lib.dom.d.ts: do not edit.\n//! The types of std/lang/dom/types.velt that \`lib.dom\` also declares as values (\`declare var\`).\n\n/// The names, sorted.\npub(crate) const DOM_VALUES: [&str; ${values.length}] = [\n${values.map((v) => `    "${v}",`).join("\n")}\n];\n`);

let stale = false;
for (const [rel, body] of files) {
  const p = path.join(repo, rel);
  if (check) {
    if (!fs.existsSync(p) || fs.readFileSync(p, "utf8") !== body) { console.error(`stale: ${rel}`); stale = true; }
  } else fs.writeFileSync(p, body);
}
console.log(`${order.length} types (${values.length} also values), ${Buffer.byteLength(files.get("std/lang/dom/types.velt"))} bytes of declarations`);
for (const [k, v] of [...notes].sort()) console.log(`  ${v} x ${k}`);
if (stale) process.exit(1);
