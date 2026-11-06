// The types of api.server.velt's server functions, for TypeScript (Velt reads the .velt file).
export type Stats = { renderer: string; components: number; uptime: string };
export declare function getStats(): Promise<Stats>;
export declare function greet(name: string): Promise<string>;
