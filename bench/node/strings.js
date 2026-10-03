// Strings (same workload as strings.vlt).
const lines = [];
for (let i = 0; i < 1000000; i++) {
  const kind = i % 3 === 0 ? "fizz" : "buzz";
  lines.push(`line ${i}: ${kind} ${(i * i) % 1000} ok`);
}
const text = lines.join("\n");
let digits = 0;
for (let i = 0; i < text.length; i++) {
  const c = text.charCodeAt(i);
  if (c >= 48 && c <= 57) {
    digits++;
  }
}
console.log(lines.length, text.length, digits, lines[123456]);
let html = "";
let tpl = "";
for (let i = 0; i < 100000; i++) {
  html += "<div class=\"lvl\">leaf</div>";
  tpl = `${tpl}<p>${i % 100}</p>`;
}
console.log(html.length, tpl.length);
