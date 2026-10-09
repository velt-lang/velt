// The browser entry. In dev it first installs @sigx/vite's HMR hook: @sigx/vite registers it
// asynchronously, after the first module graph has run, so components defined in that graph
// would never hot-update (a sigx-side fix belongs in @sigx/vite). The app is imported only once
// the hook is in place. Plain JavaScript because Velt does not parse `import.meta` or `import()`,
// and Velt's tools skip .js files.
if (import.meta.hot) {
  await (await import("@sigx/vite/hmr")).installHMRPlugin();
}
await import("./client.tsx");
