// Search box of the Velt docs: filters window.VELT_SEARCH (search-index.js) as you type.
// Entries: [name, kind, module, href (relative to the site root), summary].
(function () {
  const script = document.currentScript;
  const root = (script && script.dataset.root) || "";
  const input = document.getElementById("search");
  const results = document.getElementById("results");
  if (!input || !results) return;

  function score(entry, q) {
    const name = entry[0].toLowerCase();
    if (name === q) return 0;
    const last = name.split(".").pop();
    if (last === q) return 1;
    if (last.startsWith(q)) return 2;
    if (name.includes(q)) return 3;
    if (entry[2].toLowerCase().includes(q)) return 4;
    return entry[4].toLowerCase().includes(q) ? 5 : -1;
  }

  function render() {
    const q = input.value.trim().toLowerCase();
    results.textContent = "";
    if (!q || !window.VELT_SEARCH) return;
    const hits = window.VELT_SEARCH
      .map((e) => [score(e, q), e])
      .filter(([s]) => s >= 0)
      .sort((a, b) => a[0] - b[0] || a[1][0].length - b[1][0].length)
      .slice(0, 25);
    for (const [, e] of hits) {
      const li = document.createElement("li");
      const a = document.createElement("a");
      a.href = root + e[3];
      a.textContent = e[0];
      const meta = document.createElement("span");
      meta.className = "meta";
      meta.textContent = ` ${e[1]} · ${e[2]}`;
      li.append(a, meta);
      if (e[4]) {
        const p = document.createElement("div");
        p.className = "summary";
        p.textContent = e[4];
        li.append(p);
      }
      results.append(li);
    }
  }

  input.addEventListener("input", render);
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "/" && document.activeElement !== input) {
      ev.preventDefault();
      input.focus();
    }
  });
})();
