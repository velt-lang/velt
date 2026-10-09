import type { AsyncState } from "sigx";

/** sigx's keyed `useData`; `ctx` is the component's setup context. */
export declare function useData<T>(ctx: unknown, key: string, fetcher: (key: string) => Promise<T>): AsyncState<T>;
