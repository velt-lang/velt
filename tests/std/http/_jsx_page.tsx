// A page in TSX for `jsx.test.vlt`: a layout component, a list, escaped text and an async
// component that loads when told to (`gate`), so the test can see the markup above it arrive
// first.

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

/// The page around its children, which hold an async component (#74).
function Layout(props: { title: string; children: Element[] }): Element {
  return (
    <html>
      <head>
        <title>{props.title}</title>
      </head>
      <body>{props.children}</body>
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
  return (
    <Layout title="News & posts">
      <h1>Today</h1>
      <Posts gate={gate} posts={posts} />
      <footer>end</footer>
    </Layout>
  );
}
