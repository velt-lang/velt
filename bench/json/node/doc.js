// The document of parse_typed, parse_value and navigate: n objects in one array.
exports.doc = function doc(n) {
  const parts = [];
  for (let i = 0; i < n; i++) {
    parts.push(
      `{"id":${i},"name":"user ${i}","score":${i % 1000}.5,"active":${i % 2 == 0},"tags":["t${i % 7}","x"]}`,
    );
  }
  return `[${parts.join(",")}]`;
};
