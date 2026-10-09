// The fortunes page in TSX: static markup is precompiled into constant strings, and the rows are
// folded into the page's string (`jsxList`: one template literal per row, no element), so the
// whole page is one `jsxTemplateString`, rendered by `renderToStringSync`.

import { renderToStringSync } from "velt:jsx";
import { Fortune } from "../velt/app";

function page(fortunes: Fortune[]): JSX.Element {
  return (
    <html>
      <head>
        <title>Fortunes</title>
      </head>
      <body>
        <table>
          <tr>
            <th>id</th>
            <th>message</th>
          </tr>
          {fortunes.map((f) => (
            <tr>
              <td>{f.id}</td>
              <td>{f.message}</td>
            </tr>
          ))}
        </table>
      </body>
    </html>
  );
}

export function fortunesHtml(fortunes: Fortune[]): string {
  return `<!DOCTYPE html>${renderToStringSync(page(fortunes))}`;
}
