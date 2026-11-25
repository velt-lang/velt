// The Node twin of tests/golden/std/url_inspect.velt; its .out is what Node 22 prints.
const custom = Symbol.for("nodejs.util.inspect.custom");

class Wide {
  [custom]() {
    return "Wide<" + "w".repeat(40) + ", " + "v".repeat(40) + ">";
  }
}

console.log(new URL("https://example.com/"));
console.log(new URL("https://user:pa%20ss@example.com:8080/a/b?x=1&y=two#frag"));
console.log(new URL("https://example.com/it's?q=a+b%20c&q=%27"));
console.log(new URL("file:///tmp/x"));
console.log(new URL("mailto:a@b.c"));
console.log({ link: new URL("https://a.b/?q=1") });
console.log([new URL("https://a.b/")]);
console.log({ a: { b: new URL("https://a.b/?q=1") } });
console.log("u", new URL("https://a.b/c"), 1);
console.log({ a: { b: { c: new URL("https://a.b/") } } });
console.log([[[new URL("https://a.b/")]]]);

console.log(new URLSearchParams("a=1&b=2"));
console.log(new URLSearchParams(""));
console.log(new URLSearchParams("a=1&a=2&it's=\"x\""));
console.log({ p: new URLSearchParams("a=1") });
console.log([new URLSearchParams("a=1&a=2")]);
console.log({ a: { b: { c: new URLSearchParams("a=1") } } });
console.log(new URLSearchParams(`a=${"x".repeat(80)}&b=2&c=3`));
console.log(new URLSearchParams(`a=${"x".repeat(59)}&b=1`));
console.log(new URLSearchParams(`a=${"x".repeat(60)}&b=1`));
console.log({ deep: { p: new URLSearchParams(`a=${"y".repeat(80)}&b=2`) } });
console.log(new URL(`https://a.b/?a=${"z".repeat(80)}&b=2`));
// A string past 76 units with a line break is split after it, as Node splits strings.
console.log(new URLSearchParams(`a=${"x".repeat(37)}%0A${"y".repeat(37)}`));
console.log(new URLSearchParams(`a=${"x".repeat(38)}%0A${"y".repeat(38)}`));
console.log(new URLSearchParams(`k%0A${"x".repeat(80)}=1&b=${"q".repeat(40)}%0A%0Az`));
console.log(new URL(`https://a.b/?a=${"x".repeat(50)}%0A${"y".repeat(50)}`));
// A custom inspect's text printed before a URL stays in one piece.
console.log([new Wide(), new URLSearchParams("a=1")]);
console.log({ a: new Wide(), u: new URL("https://a.b/") });

const u = new URL("https://a.b/x?y#z");
console.log(`${u}`);
console.log(String(u));
console.log(u.toString(), JSON.stringify({ u }));
