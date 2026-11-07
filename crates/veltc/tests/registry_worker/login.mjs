// Signing in with GitHub on the registry's Worker (crates/veltc/tests/registry_worker.rs), against
// serve.mjs's GitHub stub. `node login.mjs <url>`: runs the cases, then prints `tokens <a> <b>`, two
// tokens of the user `carol` from two sign-ins, for the test to publish with. Exits 1 on a failure.

import assert from "node:assert/strict";

const base = process.argv[2].replace(/\/+$/, "");
const COOKIE = "__Host-velt_login_state";

/** `GET <path>` without following a redirect, with `cookie` when given. */
async function get(path, cookie = null) {
  const headers = cookie === null ? {} : { cookie };
  return await fetch(`${base}${path}`, { headers, redirect: "manual" });
}

/** Starts a sign-in: the `state` GitHub is sent, and the cookie holding it. */
async function start() {
  const res = await get("/login");
  assert.equal(res.status, 302);
  const to = new URL(res.headers.get("location"));
  assert.equal(to.origin + to.pathname, "https://github.com/login/oauth/authorize");
  assert.equal(to.searchParams.get("client_id"), "the-client");
  assert.equal(to.searchParams.get("redirect_uri"), "https://registry.test/login/callback");
  const state = to.searchParams.get("state");
  assert.match(state, /^[0-9a-f]{32}$/);
  const set = res.headers.get("set-cookie");
  assert.ok(set.startsWith(`${COOKIE}=${state};`), set);
  for (const attribute of ["Path=/", "Max-Age=600", "HttpOnly", "Secure", "SameSite=Lax"]) {
    assert.ok(set.includes(attribute), `${attribute} in ${set}`);
  }
  return { state, cookie: `other=1; ${COOKIE}=${state}` };
}

/** The callback with `code`, after a sign-in started here: status and page. */
async function signIn(code) {
  const { state, cookie } = await start();
  const res = await get(`/login/callback?code=${code}&state=${state}`, cookie);
  assert.equal(res.headers.get("cache-control"), "no-store");
  // Every answer ends the sign-in.
  assert.ok(res.headers.get("set-cookie").startsWith(`${COOKIE}=; Path=/; Max-Age=0;`));
  return { status: res.status, page: await res.text() };
}

/** The token on a page that signed `user` in. */
function tokenOf(answer, user) {
  assert.equal(answer.status, 200, answer.page);
  assert.ok(answer.page.includes(`signed in as <strong>${user}</strong>`), answer.page);
  assert.ok(answer.page.includes(`velt login ${base}`), answer.page);
  return /<pre id="token">(velt_[0-9a-f]{64})<\/pre>/.exec(answer.page)[1];
}

const first = tokenOf(await signIn("code-carol"), "carol");
// A renamed account keeps its user, and gets a new token; the first one still works.
const second = tokenOf(await signIn("code-carol-renamed"), "carol");
assert.notEqual(first, second);

// A login that can't be a user name, and one that is another user's name.
const digit = await signIn("code-digit");
assert.equal(digit.status, 403);
assert.ok(digit.page.includes("9lives"), digit.page);
const taken = await signIn("code-alice");
assert.equal(taken.status, 403);
assert.ok(taken.page.includes("belongs to someone else"), taken.page);
// A code GitHub doesn't know.
assert.equal((await signIn("code-nobody")).status, 502);

// A callback that didn't start here, or with another state, or cancelled on GitHub.
const { state, cookie } = await start();
for (const [path, sent] of [
  [`/login/callback?code=code-carol&state=${state}`, null],
  [`/login/callback?code=code-carol&state=${state}`, `${COOKIE}=`],
  [`/login/callback?code=code-carol&state=${"0".repeat(32)}`, cookie],
  ["/login/callback?code=code-carol", cookie],
  [`/login/callback?error=access_denied&state=${state}`, cookie],
  [`/login/callback?state=${state}`, cookie],
]) {
  const res = await get(path, sent);
  assert.equal(res.status, 400, `${path} with ${sent}`);
  assert.ok(!(await res.text()).includes("velt_"));
}
// Only GET signs in.
assert.notEqual((await fetch(`${base}/login`, { method: "POST", redirect: "manual" })).status, 302);

console.log(`tokens ${first} ${second}`);
