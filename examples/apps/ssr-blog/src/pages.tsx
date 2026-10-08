// The pages: the shared components plus async components that load their data on the server.
// Each async component starts when its element is created, so the loads of one page overlap;
// renderToStream sends the markup above one while it still loads.

import { CommentList, NotFound, PostBody, PostList, page } from "./shared/components";
import type { Post } from "./shared/model";
import { findPost, loadComments, loadPosts } from "./store";

async function Posts(props: { tag: string | null }): Promise<JSX.Element> {
  const posts = await loadPosts(props.tag);
  return <PostList posts={posts} tag={props.tag} />;
}

async function Comments(props: { slug: string }): Promise<JSX.Element> {
  const comments = await loadComments(props.slug);
  return <CommentList comments={comments} />;
}

const POSTS = "/posts/";

/** What a request asks for: decided before rendering starts, as the status goes out first. */
export type Route = { status: i64; path: string; tag: string | null; post: Post | null };

/** The route of `path` (`query` without the `?`): the post is looked up once, here. */
export function route(path: string, query: string): Route {
  if (path == "/") {
    const tag = new URLSearchParams(query).get("tag");
    return { status: 200, path, tag: tag == "" ? null : tag, post: null };
  }
  const post = path.startsWith(POSTS) ? findPost(path.slice(POSTS.length)) : null;
  return { status: post == null ? 404 : 200, path, tag: null, post };
}

/** The page of route `r`. Its async components start loading now. */
export function render(r: Route): JSX.Element {
  const post = r.post;
  if (post != null) {
    const title = post.title;
    const slug = post.slug;
    return page(
      title,
      <>
        <PostBody post={post} />
        <Comments slug={slug} />
      </>,
    );
  }
  if (r.status == 200) {
    return page("Posts", <Posts tag={r.tag} />);
  }
  return page("Not found", <NotFound path={r.path} />);
}
