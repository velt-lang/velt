// The fortunes page in TSX for the streaming tests (bench/web/README.md, `velt-native`): the
// TechEmpower fortunes rules on in-memory rows (twelve made per request, one added, sorted by
// message, every message escaped), as a sync tree (`renderToStringSync`) and with the rows in an
// async component (`renderToString`, `renderToStream`).

import { Element } from "velt:jsx";

export class Fortune {
  id: i64;
  message: string;

  constructor(id: i64, message: string) {
    this.id = id;
    this.message = message;
  }
}

/// The rows for one request: the twelve, one added at request time, sorted by message.
export function fortunes(): Fortune[] {
  // Made per request, as rows from a database would be (no module state on the request path,
  // docs/std/http_server.md).
  const messages: string[] = [
    "fortune: No such file or directory",
    "A computer scientist is someone who fixes things that aren't broken.",
    "After enough decimal places, nobody gives a damn.",
    "A bad random number generator: 1, 1, 1, 1, 1, 4.33e+67, 1, 1, 1",
    "A computer program does what you tell it to do, not what you want it to do.",
    "Emacs is a nice operating system, but I prefer UNIX. — Tom Christaensen",
    "Any program that runs right is obsolete.",
    "A list is only as strong as its weakest link. — Donald Knuth",
    "Feature: A bug with seniority.",
    "Computers make very fast, very accurate mistakes.",
    "<script>alert(\"This should not be displayed in a browser alert box.\");</script>",
    "フレームワークのベンチマーク",
  ];
  const rows: Fortune[] = [];
  for (let i = 0; i < messages.length; i++) {
    rows.push(new Fortune((i + 1) as i64, messages[i]));
  }
  rows.push(new Fortune(0, "Additional fortune added at request time."));
  rows.sort((a, b) => (a.message < b.message ? -1 : a.message > b.message ? 1 : 0));
  return rows;
}

function table(rows: Fortune[]): Element {
  return (
    <table>
      <tr>
        <th>id</th>
        <th>message</th>
      </tr>
      {rows.map((f) => (
        <tr>
          <td>{f.id}</td>
          <td>{f.message}</td>
        </tr>
      ))}
    </table>
  );
}

function shell(content: Element): Element {
  return (
    <html>
      <head>
        <title>Fortunes</title>
      </head>
      <body>{content}</body>
    </html>
  );
}

/// The whole page as one tree (no async component).
export function page(): Element {
  return shell(table(fortunes()));
}

async function Rows(props: { rows: Fortune[] }): Promise<Element> {
  return table(props.rows);
}

/// The page with its rows in an async component (the shell is sent first when streamed).
export function asyncPage(): Element {
  return shell(<Rows rows={fortunes()} />);
}
