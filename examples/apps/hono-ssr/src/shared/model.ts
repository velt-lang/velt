// The shop's data: plain types and pure helpers, shared by the Velt server and the Node reference
// (`tsCompat` in package.velt).

export type Product = {
  slug: string;
  name: string;
  /** Price in cents. */
  price: number;
  stock: number;
  tags: string[];
  blurb: string;
};

/** `1999` → `"$19.99"`. */
export function formatPrice(cents: number): string {
  const whole = Math.floor(cents / 100);
  const rest = cents % 100;
  return `$${whole}.${rest < 10 ? "0" : ""}${rest}`;
}

/** The catalog, in display order. */
export function catalog(): Product[] {
  return [
    {
      slug: "kettle",
      name: "Copper kettle",
      price: 4900,
      stock: 3,
      tags: ["kitchen", "gift"],
      blurb: "Boils water. Looks good doing it.",
    },
    {
      slug: "mug",
      name: "Tom's \"best\" mug",
      price: 1205,
      stock: 0,
      tags: ["kitchen"],
      blurb: "Holds 350 ml & keeps it <hot>.",
    },
    {
      slug: "lamp",
      name: "Desk lamp",
      price: 2999,
      stock: 12,
      tags: ["office", "gift"],
      blurb: "Warm light for late builds.",
    },
  ];
}

/** The products tagged `tag`, or all of them for `null`. */
export function withTag(products: Product[], tag: string | null): Product[] {
  if (tag == null) {
    return products;
  }
  return products.filter((p) => p.tags.includes(tag));
}

/** The product called `slug`, if there is one. */
export function findProduct(products: Product[], slug: string): Product | null {
  for (const p of products) {
    if (p.slug == slug) {
      return p;
    }
  }
  return null;
}
