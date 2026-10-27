// The requests for serve.vlt.
export const requests = () => [
  new Request("https://example.com/json?q=velt"),
  new Request("https://example.com/greet", {
    method: "POST",
    headers: { "user-agent": "drive" },
    body: JSON.stringify({ name: "Workers" }),
  }),
  new Request("https://example.com/bytes"),
  new Request("https://example.com/fail"),
  new Request("https://example.com/missing"),
];
