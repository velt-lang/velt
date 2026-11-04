// Routing, shared like the components: a request path and query to the page's status and element.

import type { Child } from "hono/jsx";
import { Layout, NotFound, ProductList, ProductPage } from "./components";
import { catalog, findProduct, withTag } from "./model";

/** A page: its status code and what to render. */
export type Page = { status: number; title: string; body: Child };

/** The page for `path` (`/`, `/products/<slug>`) with query `tag` (`null` without one). */
export function route(path: string, tag: string | null): Page {
  const products = catalog();
  if (path == "/") {
    return {
      status: 200,
      title: "Products",
      body: <ProductList products={withTag(products, tag)} tag={tag} />,
    };
  }
  if (path.startsWith("/products/")) {
    const p = findProduct(products, path.slice(10));
    if (p != null) {
      // The element first: `{ title: p.name, body: <ProductPage product={p} /> }` renders an
      // empty name in Velt today (#2258: a field of a narrowed value moved out by the literal).
      const body = <ProductPage product={p} />;
      return { status: 200, title: p.name, body: body };
    }
  }
  return { status: 404, title: "Not found", body: <NotFound path={path} /> };
}

/** The whole document of a page. */
export function document(page: Page) {
  return <Layout title={page.title}>{page.body}</Layout>;
}
