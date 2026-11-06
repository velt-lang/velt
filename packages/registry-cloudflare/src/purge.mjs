// The host module `registry-cloudflare` (src/purge.velt declares its function): Cloudflare's cache
// purge. The API token stays here, in the Worker's env; it never reaches the program.

const PURGE_TIMEOUT_MS = 10_000;

export default (host) => ({
  /**
   * Purges `paths` (JSON: an array of paths) below `env.PUBLIC_URL` from the cache of the zone
   * `env.CACHE_ZONE_ID`, with the API token `env.CACHE_PURGE_TOKEN`. Does nothing without a zone:
   * a registry no cache is in front of has nothing to purge.
   */
  async purgePaths(paths) {
    const { CACHE_ZONE_ID: zone, CACHE_PURGE_TOKEN: token, PUBLIC_URL: origin } = host.env;
    if (!zone) return;
    if (!token || !origin) throw new Error("CACHE_PURGE_TOKEN and PUBLIC_URL are needed to purge");
    const base = origin.replace(/\/+$/, "");
    const files = JSON.parse(paths).map((p) => `${base}${p}`);
    const res = await fetch(`https://api.cloudflare.com/client/v4/zones/${zone}/purge_cache`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      body: JSON.stringify({ files }),
      // The registry holds the package's lock meanwhile, and the client waits.
      signal: AbortSignal.timeout(PURGE_TIMEOUT_MS),
    });
    if (!res.ok) throw new Error(`the purge answered ${res.status}`);
  },
});
