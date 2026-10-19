// The server functions of ../src/api.server.vlt in JavaScript, with the same results, so
// reference.mjs can render the app with JavaScript sigx on Node (it cannot call Velt code).
export async function getStats() {
  await new Promise((r) => setTimeout(r, 300));
  return { renderer: "Velt (native)", components: 9, uptime: "since this request" };
}

export async function greet(name) {
  return `Hello, ${name}, from a Velt server function`;
}
