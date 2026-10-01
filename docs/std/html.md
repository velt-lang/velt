# velt:html

`import { escapeHtml } from "velt:html"`. Escapes text for HTML element content and quoted
attribute values in one pass: `&`, `<`, `>`, `"` and `'` become `&amp;`, `&lt;`, `&gt;`,
`&quot;` and `&#39;`; everything else (including non-ASCII) is kept.

```ts
import { escapeHtml } from "velt:html";

function main() {
  const name = `<b>"Tom" & 'Jerry'</b>`;
  console.log(`<td title="${escapeHtml(name)}">${escapeHtml(name)}</td>`);
  // <td title="&lt;b&gt;&quot;Tom&quot; &amp; &#39;Jerry&#39;&lt;/b&gt;">&lt;b&gt;&quot;Tom…</td>
}
```
