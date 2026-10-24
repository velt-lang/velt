// A page in TSX for `http_server_jsx.vlt`: a layout, a list, escaped text and an async component
// that loads when told to (`gate`), so the test can see the markup above it arrive first.

import { Element } from "velt:jsx";
import { Channel } from "velt:channel";

export class Post {
  id: i64;
  title: string;

  constructor(id: i64, title: string) {
    this.id = id;
    this.title = title;
  }
}

/// The page around `content`: a plain function, as content holding an async element cannot be
/// copied into a component's props yet (docs/reference/tsx.md).
function layout(title: string, content: Element): Element {
  return (
    <html>
      <head>
        <title>{title}</title>
      </head>
      <body>{content}</body>
    </html>
  );
}

/// Loads its posts once `gate` gives a word (or at once without a gate).
async function Posts(props: { gate: Channel<string> | null; posts: Post[] }): Promise<Element> {
  let note = "ready";
  if (props.gate != null) {
    note = (await props.gate.receive()) ?? "closed";
  }
  return (
    <section>
      <p class="note">{note}</p>
      <ul>
        {props.posts.map((p) => (
          <li id={`post-${p.id}`}>{p.title}</li>
        ))}
      </ul>
    </section>
  );
}

export function page(gate: Channel<string> | null, posts: Post[]): Element {
  return layout(
    "News & posts",
    <>
      <h1>Today</h1>
      <Posts gate={gate} posts={posts} />
      <footer>end</footer>
    </>,
  );
}
