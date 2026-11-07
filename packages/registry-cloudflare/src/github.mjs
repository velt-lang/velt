// The host module `registry-cloudflare-github` (src/github.velt declares its functions): signing in
// with GitHub, and secure random bytes. The OAuth app's secret stays here, in the Worker's env; the
// program gets only the account's id and login.

const GITHUB_TIMEOUT_MS = 10_000;

/** `env.PUBLIC_URL` without its trailing `/`s; throws without one. */
function origin(env) {
  if (!env.PUBLIC_URL) throw new Error("PUBLIC_URL is needed to sign in with GitHub");
  return env.PUBLIC_URL.replace(/\/+$/, "");
}

/** `env.GITHUB_CLIENT_ID` and `env.GITHUB_CLIENT_SECRET`; throws without them. */
function client(env) {
  const { GITHUB_CLIENT_ID: id, GITHUB_CLIENT_SECRET: secret } = env;
  if (!id || !secret) {
    throw new Error("GITHUB_CLIENT_ID and GITHUB_CLIENT_SECRET are needed to sign in with GitHub");
  }
  return { id, secret };
}

/** A JSON answer from GitHub, or an error naming what answered `status`. */
async function json(res, what) {
  if (!res.ok) throw new Error(`${what} answered ${res.status}`);
  return await res.json();
}

export default (host) => ({
  /**
   * GitHub's authorization URL for this registry's OAuth app, coming back to
   * `<PUBLIC_URL>/login/callback` with `state`. No scope: the public profile is enough.
   */
  async authorizeUrl(state) {
    const url = new URL("https://github.com/login/oauth/authorize");
    url.searchParams.set("client_id", client(host.env).id);
    url.searchParams.set("redirect_uri", `${origin(host.env)}/login/callback`);
    url.searchParams.set("state", state);
    url.searchParams.set("allow_signup", "true");
    return url.href;
  },

  /**
   * The GitHub account that `code` (from the callback) signs in, as JSON `{ "id": <number>,
   * "login": "<login>" }`. Rejects when GitHub refuses the code or doesn't answer.
   */
  async githubAccount(code) {
    const { id, secret } = client(host.env);
    const exchange = await fetch("https://github.com/login/oauth/access_token", {
      method: "POST",
      headers: { accept: "application/json", "content-type": "application/json" },
      body: JSON.stringify({
        client_id: id,
        client_secret: secret,
        code,
        redirect_uri: `${origin(host.env)}/login/callback`,
      }),
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
    const grant = await json(exchange, "GitHub's token exchange");
    if (typeof grant.access_token !== "string") {
      throw new Error(`GitHub refused the sign-in: ${grant.error ?? "no access token"}`);
    }
    const res = await fetch("https://api.github.com/user", {
      headers: {
        accept: "application/vnd.github+json",
        authorization: `Bearer ${grant.access_token}`,
        "user-agent": "velt-registry",
      },
      signal: AbortSignal.timeout(GITHUB_TIMEOUT_MS),
    });
    const user = await json(res, "GitHub's user API");
    if (!Number.isSafeInteger(user.id) || typeof user.login !== "string") {
      throw new Error("GitHub's user API answered no account");
    }
    return JSON.stringify({ id: user.id, login: user.login });
  },

  /** `n` secure random bytes, in hex (the wasm runtime has no secure generator). */
  randomHex(n) {
    const bytes = crypto.getRandomValues(new Uint8Array(Number(n)));
    return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  },
});
