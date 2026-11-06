// The shop's components, written once: tsc compiles them with the real `hono/jsx`, Velt with
// ./hono (package.velt `paths`), and both render the same HTML. They are functions of typed props
// in the common subset of TypeScript and Velt.

import type { Child } from "hono/jsx";
import type { Product } from "./model";
import { formatPrice } from "./model";

/** The document around a page. */
export function Layout(props: { title: string; children?: Child }) {
  return (
    <html lang="en">
      <head>
        <meta charset="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <title>{props.title} · Velt shop</title>
        <link rel="stylesheet" href="/shop.css" />
      </head>
      <body>
        <header className="top">
          <a href="/">Velt shop</a>
          <nav>
            <a href="/?tag=kitchen">kitchen</a> <a href="/?tag=gift">gifts</a>
          </nav>
        </header>
        <main>{props.children}</main>
        <footer style={{ fontSize: 12, opacity: 0.7, marginTop: 24 }}>Rendered by Velt</footer>
      </body>
    </html>
  );
}

function Tags(props: { tags: string[] }) {
  return (
    <ul class="tags">
      {props.tags.map((t) => (
        <li>
          <a href={`/?tag=${t}`}>{t}</a>
        </li>
      ))}
    </ul>
  );
}

/** One product in the list. */
export function ProductCard(props: { product: Product }) {
  const p = props.product;
  return (
    <article class="card" data-stock={p.stock}>
      <h2>
        <a href={`/products/${p.slug}`}>{p.name}</a>
      </h2>
      <p class="price">{formatPrice(p.price)}</p>
      {p.stock == 0 ? <p class="sold-out">Sold out</p> : null}
      <Tags tags={p.tags} />
    </article>
  );
}

/** The product list, filtered by `tag` when there is one. */
export function ProductList(props: { products: Product[]; tag: string | null }) {
  return (
    <section>
      <h1>{props.tag == null ? "All products" : `Tagged ${props.tag}`}</h1>
      {props.products.length == 0 ? <p>Nothing here.</p> : null}
      {props.products.map((p) => (
        <ProductCard product={p} />
      ))}
    </section>
  );
}

/** One product with an order form. */
export function ProductPage(props: { product: Product }) {
  const p = props.product;
  return (
    <section class="product">
      <h1>{p.name}</h1>
      <p>{p.blurb}</p>
      <p class="price">
        {formatPrice(p.price)}, {p.stock} in stock
      </p>
      <form method="post" action="/cart">
        <input type="hidden" name="slug" value={p.slug} />
        <label htmlFor="qty">Quantity</label>
        <input
          id="qty"
          name="qty"
          type="number"
          min={1}
          max={p.stock}
          value="1"
          disabled={p.stock == 0}
        />
        <br />
        <button type="submit" disabled={p.stock == 0}>
          Add to cart
        </button>
      </form>
    </section>
  );
}

/** The 404 page. */
export function NotFound(props: { path: string }) {
  return (
    <section>
      <h1>Not found</h1>
      <p>
        There is nothing at <code>{props.path}</code>.
      </p>
    </section>
  );
}
