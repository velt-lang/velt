// @sigx/velt/data in the browser: sigx's own keyed `useData`, behind the signature the Velt
// side needs (`ctx` first; see velt/src/data.vlt). Data the server rendered is restored from
// `window.__SIGX_ASYNC__`, so it is not fetched again.
import { useData as sigxUseData } from "sigx";

export function useData(ctx, key, fetcher) {
  return sigxUseData(key, fetcher);
}
