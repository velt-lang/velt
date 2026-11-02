// The host module `probe` (src/lib.vlt), made for each instance from the Worker's `env` and
// `ctx` (drive.mjs's fakes).
export default (host) => ({
  greeting: (name) => `${host.env.GREETING}, ${name}`,
  gate: (name) => host.env.gates.enter(name),
});
