// Fixed pages of the shared components, for checking that the server (std/jsx) and a TypeScript
// client (client/jsx-runtime.ts) render the same HTML: crates/veltc/tests/ts_compat_node renders
// them with `velt run` and with Node. The data has what escaping must handle.

import type { Post } from "./model";
import { CommentList, NotFound, PostBody, PostCard, Tags, page } from "./components";

function samplePost(): Post {
  return {
    slug: "quotes",
    title: `Quotes "q" & 'a' <b>`,
    author: "Ada",
    date: "2026-10-08",
    tags: ["x", "c d", "<t>"],
    paragraphs: ["One & two < three > four", "it's a word list that is long enough to cut"],
  };
}

/** The shared components with a sample post and a few other values, one element each. */
export function samples(): JSX.Element[] {
  return [
    PostCard({ post: samplePost() }),
    PostBody({ post: samplePost() }),
    CommentList({ comments: [{ author: "M", text: `<img src=x onerror="alert('1')">` }] }),
    CommentList({ comments: [] }),
    NotFound({ path: "/x?<y>&z" }),
    page("Title & <more>", <Tags tags={["a", "b c"]} />),
  ];
}
