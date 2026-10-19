// The blog's components, shared with a TypeScript client (`tsCompat`): functions of typed props
// that return `JSX.Element`, in the common subset of TypeScript and Velt. They know nothing about
// the server: data comes in through props, and async loading happens in src/pages.tsx.

import type { Comment, Post } from "./model";
import { excerpt, readingMinutes } from "./model";

/**
 * The page around `content`. A function the pages call rather than a component with children:
 * the content holds async components, and std/jsx can't pass a pending async element in props yet
 * (docs/internals/contracts/jsx.md, "Known compatibility gaps"). This works the same in
 * TypeScript.
 */
export function page(title: string, content: JSX.Element): JSX.Element {
  return (
    <html lang="en">
      <head>
        <meta charset="utf-8" />
        <title>{title} · Velt blog</title>
      </head>
      <body>
        <header>
          <a href="/">Velt blog</a>
        </header>
        <main>{content}</main>
        <footer>Rendered on the server with TSX</footer>
      </body>
    </html>
  );
}

// The post list filtered by `tag`, the tag encoded.
function tagHref(tag: string): string {
  const query = new URLSearchParams();
  query.set("tag", tag);
  return `/?${query.toString()}`;
}

export function Tags(props: { tags: string[] }): JSX.Element {
  return (
    <ul class="tags">
      {props.tags.map((t) => (
        <li>
          <a href={tagHref(t)}>#{t}</a>
        </li>
      ))}
    </ul>
  );
}

export function PostCard(props: { post: Post }): JSX.Element {
  const post = props.post;
  return (
    <article class="card">
      <h2>
        <a href={`/posts/${post.slug}`}>{post.title}</a>
      </h2>
      <p class="meta">
        {post.author} · {post.date} · {readingMinutes(post)} min
      </p>
      <p>{excerpt(post, 80)}</p>
      <Tags tags={post.tags} />
    </article>
  );
}

export function PostList(props: { posts: Post[]; tag: string | null }): JSX.Element {
  const tag = props.tag;
  return (
    <section>
      <h1>{tag == null ? "All posts" : `Posts tagged #${tag}`}</h1>
      {props.posts.length == 0 ? <p>Nothing here yet.</p> : null}
      {props.posts.map((p) => (
        <PostCard post={p} />
      ))}
    </section>
  );
}

export function PostBody(props: { post: Post }): JSX.Element {
  const post = props.post;
  return (
    <article>
      <h1>{post.title}</h1>
      <p class="meta">
        {post.author} · {post.date}
      </p>
      {post.paragraphs.map((p) => (
        <p>{p}</p>
      ))}
      <Tags tags={post.tags} />
    </article>
  );
}

export function CommentList(props: { comments: Comment[] }): JSX.Element {
  const n = props.comments.length;
  return (
    <section class="comments">
      <h2>{n == 1 ? "1 comment" : `${n} comments`}</h2>
      <ul>
        {props.comments.map((c) => (
          <li>
            <b>{c.author}</b>: {c.text}
          </li>
        ))}
      </ul>
    </section>
  );
}

export function NotFound(props: { path: string }): JSX.Element {
  return (
    <section>
      <h1>Not found</h1>
      <p>
        No page at <code>{props.path}</code>. <a href="/">Back to all posts</a>
      </p>
    </section>
  );
}
