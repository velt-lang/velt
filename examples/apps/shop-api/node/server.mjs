// The same shop in idiomatic Node (node:http, no dependencies), for comparing with the Velt
// build: same seed, same routes, byte-identical responses. Only the messages of `bad_json`
// errors differ (Velt's typed JSON.parse writes its own).
//
//   PORT=8081 node node/server.mjs
import http from "node:http";

const CURRENCY = "EUR";
const CATEGORIES = ["audio", "books", "camping", "coffee", "garden", "kitchen", "running", "toys"];
const BRANDS = ["Acme", "Borealis", "Cobalt", "Dune", "Ember", "Fjord", "Granite", "Halo", "Ivy", "Juniper"];
const ADJECTIVES = ["Classic", "Compact", "Deluxe", "Everyday", "Light", "Pro", "Rugged", "Smart", "Ultra", "Vintage"];
const NOUNS = ["Backpack", "Bottle", "Grinder", "Headphones", "Kettle", "Lamp", "Notebook", "Planter", "Shoes", "Tent", "Speaker", "Puzzle"];
const COLORS = ["black", "white", "red", "green", "blue", "sand", "grey"];
const SIZES = ["XS", "S", "M", "L", "XL"];
const WORDS = ["durable", "recycled", "lightweight", "waterproof", "handmade", "organic", "quiet", "fast", "warm", "modular", "foldable", "repairable"];
const MATERIALS = ["steel", "cotton", "wool", "bamboo", "aluminium", "ceramic", "oak"];
const STATUSES = ["pending", "paid", "paid", "shipped", "shipped", "shipped", "cancelled"];
const SORTS = ["id", "price", "-price", "rating", "name", "newest"];
const BASE_MS = 1767225600000;

class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}
const invalid = (message) => new ApiError(400, "invalid", message);

class Random {
  constructor(seed) {
    this.state = seed >>> 0;
  }
  next(n) {
    this.state = (Math.imul(this.state, 1664525) + 1013904223) >>> 0;
    return (this.state >>> 8) % n;
  }
  pick(xs) {
    return xs[this.next(xs.length)];
  }
}

const isoAt = (seconds) => new Date(BASE_MS + seconds * 1000).toISOString();
const slugify = (name) => name.toLowerCase().replaceAll(" ", "-");

function description(r) {
  let text = "";
  const sentences = 2 + r.next(3);
  for (let s = 0; s < sentences; s++) {
    text += `${s === 0 ? "A" : " Made to be"} ${r.pick(WORDS)}, ${r.pick(WORDS)} and ${r.pick(WORDS)} companion for every day.`;
  }
  return text;
}

function seedProduct(r, id) {
  const name = `${r.pick(BRANDS)} ${r.pick(ADJECTIVES)} ${r.pick(NOUNS)} ${id}`;
  const tags = [];
  const tagCount = 1 + r.next(4);
  for (let t = 0; t < tagCount; t++) {
    const tag = r.pick(WORDS);
    if (!tags.includes(tag)) tags.push(tag);
  }
  const attributes = {};
  attributes.material = r.pick(MATERIALS);
  attributes.warranty = `${1 + r.next(5)} years`;
  if (r.next(2) === 0) attributes.origin = r.pick(["SE", "NO", "DE", "PT", "JP"]);
  const variants = [];
  const variantCount = 1 + r.next(5);
  const base = 990 + r.next(400) * 100;
  for (let v = 0; v < variantCount; v++) {
    variants.push({
      sku: `P${id}-${v + 1}`,
      color: r.pick(COLORS),
      size: r.pick(SIZES),
      stock: r.next(5) === 0 ? 0 : r.next(500),
      price: { amount: base + v * 500, currency: CURRENCY },
    });
  }
  return {
    id,
    slug: slugify(name),
    name,
    description: description(r),
    category: r.pick(CATEGORIES),
    brand: BRANDS[r.next(10)],
    tags,
    attributes,
    variants,
    rating: { average: (10 + r.next(41)) / 10, count: r.next(2000) },
    active: r.next(10) !== 0,
    createdAt: isoAt(id * 3600),
  };
}

function priced(id, status, customer, items, shipping, createdAt) {
  let subtotal = 0;
  for (const it of items) subtotal += it.lineTotal;
  const discount = subtotal >= 50000 ? Math.floor(subtotal / 10) : 0;
  const net = subtotal - discount;
  const tax = Math.floor((net * 25 + 50) / 100);
  const shippingCost = net >= 30000 ? 0 : 4900;
  return { id, status, customer, items, shipping, currency: CURRENCY, subtotal, discount, tax, shippingCost, total: net + tax + shippingCost, createdAt };
}

function seedOrders(r, products, count) {
  const out = [];
  for (let id = 1; id <= count; id++) {
    const items = [];
    const lines = 1 + r.next(4);
    for (let l = 0; l < lines; l++) {
      const p = products[r.next(products.length)];
      const v = p.variants[r.next(p.variants.length)];
      const quantity = 1 + r.next(3);
      items.push({ productId: p.id, sku: v.sku, name: p.name, quantity, unitPrice: v.price.amount, lineTotal: v.price.amount * quantity });
    }
    const customer = r.next(800) + 1;
    out.push(
      priced(id, STATUSES[r.next(7)], { id: customer, name: `Customer ${customer}`, email: `c${customer}@example.com` }, items, {
        line1: `${1 + r.next(200)} Main Street`,
        city: r.pick(["Stockholm", "Oslo", "Berlin", "Lisbon", "Kyoto"]),
        postalCode: `${10000 + r.next(89999)}`,
        country: r.pick(["SE", "NO", "DE", "PT", "JP"]),
      }, isoAt(id * 600)),
    );
  }
  return out;
}

// Typed decoding, as strict as Velt's JSON.parse<T>: required fields, exact kinds, integers.
const bad = (what, path) => new ApiError(400, "bad_json", `expected ${what} at ${path}`);
const obj = (v, p) => { if (v === null || typeof v !== "object" || Array.isArray(v)) throw bad("object", p); return v; };
const str = (v, p) => { if (typeof v !== "string") throw bad("string", p); return v; };
const int = (v, p) => { if (!Number.isInteger(v)) throw bad("i64", p); return v; };
const bool = (v, p) => { if (typeof v !== "boolean") throw bad("boolean", p); return v; };
const arr = (v, p, each) => { if (!Array.isArray(v)) throw bad("array", p); return v.map((x, i) => each(x, `${p}[${i}]`)); };
const opt = (v, p, f) => (v === undefined || v === null ? null : f(v, p));
const record = (v, p) => { obj(v, p); const out = {}; for (const [k, x] of Object.entries(v)) out[k] = str(x, `${p}.${k}`); return out; };
const oneOf = (values) => (v, p) => { if (!values.includes(v)) throw new ApiError(400, "bad_json", `expected one of ${values.map((x) => `"${x}"`).join(", ")} at ${p}`); return v; };
const price = (v, p) => { obj(v, p); return { amount: int(v.amount, `${p}.amount`), currency: str(v.currency, `${p}.currency`) }; };
const newVariant = (v, p) => { obj(v, p); return { sku: str(v.sku, `${p}.sku`), color: str(v.color, `${p}.color`), size: str(v.size, `${p}.size`), stock: int(v.stock, `${p}.stock`), price: price(v.price, `${p}.price`) }; };
const newProduct = (v, p) => {
  obj(v, p);
  return {
    name: str(v.name, `${p}.name`),
    description: opt(v.description, `${p}.description`, str),
    category: str(v.category, `${p}.category`),
    brand: str(v.brand, `${p}.brand`),
    tags: opt(v.tags, `${p}.tags`, (x, q) => arr(x, q, str)),
    attributes: opt(v.attributes, `${p}.attributes`, record),
    variants: arr(v.variants, `${p}.variants`, newVariant),
  };
};
const productPatch = (v, p) => {
  obj(v, p);
  return {
    name: opt(v.name, `${p}.name`, str),
    description: opt(v.description, `${p}.description`, str),
    tags: opt(v.tags, `${p}.tags`, (x, q) => arr(x, q, str)),
    attributes: opt(v.attributes, `${p}.attributes`, record),
    active: opt(v.active, `${p}.active`, bool),
  };
};
const customer = (v, p) => { obj(v, p); return { id: int(v.id, `${p}.id`), name: str(v.name, `${p}.name`), email: str(v.email, `${p}.email`) }; };
const address = (v, p) => { obj(v, p); return { line1: str(v.line1, `${p}.line1`), city: str(v.city, `${p}.city`), postalCode: str(v.postalCode, `${p}.postalCode`), country: str(v.country, `${p}.country`) }; };
const newOrder = (v, p) => {
  obj(v, p);
  return {
    customer: customer(v.customer, `${p}.customer`),
    items: arr(v.items, `${p}.items`, (x, q) => { obj(x, q); return { sku: str(x.sku, `${q}.sku`), quantity: int(x.quantity, `${q}.quantity`) }; }),
    shipping: address(v.shipping, `${p}.shipping`),
  };
};
const statusChange = (v, p) => { obj(v, p); return { status: oneOf(["pending", "paid", "shipped", "cancelled"])(v.status, `${p}.status`) }; };

function decode(body, f) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    throw new ApiError(400, "bad_json", e.message);
  }
  return f(v, "$");
}

const minPrice = (p) => Math.min(...p.variants.map((v) => v.price.amount));
const totalStock = (p) => p.variants.reduce((n, v) => n + v.stock, 0);

function cleanTags(tags) {
  const out = [];
  if (tags === null) return out;
  if (tags.length > 10) throw invalid("at most 10 tags");
  for (const t of tags) {
    const tag = t.trim().toLowerCase();
    if (tag.length === 0 || tag.length > 30) throw invalid("tags are 1 to 30 characters");
    if (!out.includes(tag)) out.push(tag);
  }
  return out;
}

function cleanName(name) {
  const n = name.trim();
  if (n.length === 0 || n.length > 120) throw invalid("name must be 1 to 120 characters");
  return n;
}

const isSku = (s) => /^[A-Z0-9-]{3,32}$/.test(s);

function pageOf(all, page, limit) {
  const start = (page - 1) * limit;
  return { items: all.slice(start, start + limit), page, limit, total: all.length, pages: Math.floor((all.length + limit - 1) / limit) };
}

export class Store {
  constructor() {
    this.products = [];
    this.orders = [];
    this.byId = new Map();
    this.bySku = new Map();
    this.orderById = new Map();
    this.nextProductId = 1;
    this.nextOrderId = 1;
    this.clock = 0;
  }

  static seeded(seed, productCount, orderCount) {
    const s = new Store();
    const r = new Random(seed);
    const products = [];
    for (let id = 1; id <= productCount; id++) products.push(seedProduct(r, id));
    for (const p of products) s.addProduct(p);
    for (const o of seedOrders(r, s.products, orderCount)) s.addOrder(o);
    s.clock = orderCount * 600;
    return s;
  }

  now() {
    this.clock += 60;
    return isoAt(this.clock);
  }

  addProduct(p) {
    const index = this.products.length;
    p.variants.forEach((v, i) => this.bySku.set(v.sku, { product: index, variant: i }));
    this.byId.set(p.id, index);
    this.products.push(p);
    if (p.id >= this.nextProductId) this.nextProductId = p.id + 1;
  }

  addOrder(o) {
    this.orderById.set(o.id, this.orders.length);
    this.orders.push(o);
    if (o.id >= this.nextOrderId) this.nextOrderId = o.id + 1;
  }

  product(id) {
    const index = this.byId.get(id);
    if (index === undefined) throw new ApiError(404, "not_found", `no product ${id}`);
    return this.products[index];
  }

  order(id) {
    const index = this.orderById.get(id);
    if (index === undefined) throw new ApiError(404, "not_found", `no order ${id}`);
    return this.orders[index];
  }

  listProducts(q) {
    const hits = this.products.filter((p) => {
      if (q.category !== null && p.category !== q.category) return false;
      if (q.brand !== null && p.brand !== q.brand) return false;
      if (q.tag !== null && !p.tags.includes(q.tag)) return false;
      if (q.q !== null && !p.name.toLowerCase().includes(q.q) && !p.description.toLowerCase().includes(q.q)) return false;
      if (q.minPrice !== null || q.maxPrice !== null) {
        const price = minPrice(p);
        if ((q.minPrice !== null && price < q.minPrice) || (q.maxPrice !== null && price > q.maxPrice)) return false;
      }
      if (q.inStock && totalStock(p) === 0) return false;
      return true;
    });
    switch (q.sort) {
      case "price":
      case "-price": {
        // Each product's lowest price once, not on every comparison.
        const dir = q.sort === "price" ? 1 : -1;
        const keyed = hits.map((p) => ({ price: minPrice(p), p }));
        keyed.sort((a, b) => (a.price - b.price) * dir || a.p.id - b.p.id);
        return pageOf(keyed.map((k) => k.p), q.page, q.limit);
      }
      case "rating": hits.sort((a, b) => Math.sign(b.rating.average - a.rating.average) || b.rating.count - a.rating.count || a.id - b.id); break;
      case "name": hits.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : a.id - b.id)); break;
      case "newest": hits.sort((a, b) => b.id - a.id); break;
    }
    return pageOf(hits, q.page, q.limit);
  }

  variantsOf(input, taken) {
    if (input.length === 0 || input.length > 20) throw invalid("a product has 1 to 20 variants");
    return input.map((v) => {
      if (!isSku(v.sku)) throw invalid(`sku '${v.sku}' must be 3 to 32 of A-Z, 0-9 and -`);
      if (this.bySku.has(v.sku) || taken.includes(v.sku)) throw new ApiError(409, "conflict", `sku '${v.sku}' already exists`);
      if (v.stock < 0 || v.stock > 1000000) throw invalid("stock must be 0 to 1000000");
      if (v.price.amount <= 0 || v.price.currency !== CURRENCY) throw invalid(`price must be a positive amount in ${CURRENCY}`);
      taken.push(v.sku);
      return { sku: v.sku, color: v.color, size: v.size, stock: v.stock, price: { amount: v.price.amount, currency: CURRENCY } };
    });
  }

  create(input, taken) {
    const name = cleanName(input.name);
    if (!CATEGORIES.includes(input.category)) throw invalid(`category must be one of ${CATEGORIES.join(", ")}`);
    const brand = input.brand.trim();
    if (brand.length === 0 || brand.length > 60) throw invalid("brand must be 1 to 60 characters");
    const description = input.description ?? "";
    if (description.length > 2000) throw invalid("description is at most 2000 characters");
    const tags = cleanTags(input.tags);
    const variants = this.variantsOf(input.variants, taken);
    const p = {
      id: this.nextProductId,
      slug: slugify(name),
      name,
      description,
      category: input.category,
      brand,
      tags,
      attributes: input.attributes ?? {},
      variants,
      rating: { average: 0, count: 0 },
      active: true,
      createdAt: this.now(),
    };
    this.addProduct(p);
    return p;
  }

  createProduct(input) {
    return this.create(input, []);
  }

  bulkCreate(inputs) {
    if (inputs.length > 1000) throw invalid("at most 1000 products per request");
    const ids = [];
    const errors = [];
    const taken = [];
    inputs.forEach((input, index) => {
      try {
        ids.push(this.create(input, taken).id);
      } catch (e) {
        errors.push({ index, code: e.code, message: e.message });
      }
    });
    return { created: ids.length, ids, errors };
  }

  patchProduct(id, patch) {
    const p = this.product(id);
    const name = patch.name === null ? null : cleanName(patch.name);
    if (patch.description !== null && patch.description.length > 2000) throw invalid("description is at most 2000 characters");
    const tags = patch.tags === null ? null : cleanTags(patch.tags);
    if (name !== null) {
      p.name = name;
      p.slug = slugify(name);
    }
    if (patch.description !== null) p.description = patch.description;
    if (tags !== null) p.tags = tags;
    if (patch.attributes !== null) Object.assign(p.attributes, patch.attributes);
    if (patch.active !== null) p.active = patch.active;
    return p;
  }

  createOrder(input) {
    if (input.items.length === 0 || input.items.length > 50) throw invalid("an order has 1 to 50 items");
    if (!input.customer.email.includes("@")) throw invalid("customer.email is not an email address");
    const items = [];
    const refs = [];
    for (const line of input.items) {
      const ref = this.bySku.get(line.sku);
      if (ref === undefined) throw invalid(`unknown sku '${line.sku}'`);
      if (line.quantity < 1 || line.quantity > 100) throw invalid("quantity must be 1 to 100");
      const p = this.products[ref.product];
      const v = p.variants[ref.variant];
      if (!p.active) throw new ApiError(409, "conflict", `product ${p.id} is not for sale`);
      const wanted = line.quantity + items.filter((it) => it.sku === line.sku).reduce((n, it) => n + it.quantity, 0);
      if (v.stock < wanted) throw new ApiError(409, "conflict", `only ${v.stock} of '${line.sku}' in stock`);
      items.push({ productId: p.id, sku: v.sku, name: p.name, quantity: line.quantity, unitPrice: v.price.amount, lineTotal: v.price.amount * line.quantity });
      refs.push(ref);
    }
    items.forEach((it, i) => {
      this.products[refs[i].product].variants[refs[i].variant].stock -= it.quantity;
    });
    const order = priced(this.nextOrderId, "pending", input.customer, items, input.shipping, this.now());
    this.addOrder(order);
    return order;
  }

  setStatus(id, status) {
    const o = this.order(id);
    const allowed =
      (o.status === "pending" && (status === "paid" || status === "cancelled")) ||
      (o.status === "paid" && (status === "shipped" || status === "cancelled"));
    if (!allowed) throw new ApiError(409, "conflict", `an order can't go from ${o.status} to ${status}`);
    if (status === "cancelled") {
      for (const it of o.items) {
        const ref = this.bySku.get(it.sku);
        if (ref !== undefined) this.products[ref.product].variants[ref.variant].stock += it.quantity;
      }
    }
    o.status = status;
    return o;
  }

  listOrders(q) {
    const hits = [];
    for (let i = this.orders.length - 1; i >= 0; i--) {
      const o = this.orders[i];
      if ((q.status === null || o.status === q.status) && (q.customer === null || o.customer.id === q.customer)) hits.push(o);
    }
    return pageOf(hits, q.page, q.limit);
  }

  stats() {
    let active = 0;
    let variants = 0;
    let units = 0;
    for (const p of this.products) {
      if (p.active) active++;
      variants += p.variants.length;
      units += totalStock(p);
    }
    const byStatus = { pending: 0, paid: 0, shipped: 0, cancelled: 0 };
    const byCategory = new Map(CATEGORIES.map((c) => [c, { category: c, orders: 0, units: 0, revenue: 0 }]));
    const byProduct = new Map();
    let revenue = 0;
    let counted = 0;
    for (const o of this.orders) {
      byStatus[o.status]++;
      if (o.status === "cancelled") continue;
      revenue += o.total;
      counted++;
      const seen = new Set();
      for (const it of o.items) {
        const c = byCategory.get(this.product(it.productId).category);
        c.units += it.quantity;
        c.revenue += it.lineTotal;
        if (!seen.has(c.category)) {
          seen.add(c.category);
          c.orders++;
        }
        const ps = byProduct.get(it.productId);
        if (ps === undefined) byProduct.set(it.productId, { productId: it.productId, name: it.name, units: it.quantity, revenue: it.lineTotal });
        else {
          ps.units += it.quantity;
          ps.revenue += it.lineTotal;
        }
      }
    }
    const cats = [...byCategory.values()].sort((a, b) => b.revenue - a.revenue || (a.category < b.category ? -1 : 1));
    const top = [...byProduct.values()].sort((a, b) => b.units - a.units || a.productId - b.productId);
    return {
      products: this.products.length,
      activeProducts: active,
      variants,
      unitsInStock: units,
      orders: this.orders.length,
      ordersByStatus: byStatus,
      revenue,
      averageOrderValue: counted === 0 ? 0 : Math.floor(revenue / counted),
      categories: cats,
      topProducts: top.slice(0, 10),
    };
  }
}

function positiveInt(text, what) {
  if (!/^[0-9]{1,15}$/.test(text)) throw invalid(`${what} must be a positive integer`);
  const n = Number.parseInt(text, 10);
  if (n < 1) throw invalid(`${what} must be a positive integer`);
  return n;
}

function intParam(params, name, fallback, max) {
  const raw = params.get(name);
  if (raw === null) return fallback;
  const n = positiveInt(raw, name);
  if (n > max) throw invalid(`${name} must be at most ${max}`);
  return n;
}

const optionalInt = (params, name) => (params.get(name) === null ? null : positiveInt(params.get(name), name));

function productQuery(query) {
  const params = new URLSearchParams(query);
  const sort = params.get("sort") ?? "id";
  if (!SORTS.includes(sort)) throw invalid(`sort must be one of ${SORTS.join(", ")}`);
  const q = params.get("q");
  return {
    category: params.get("category"),
    brand: params.get("brand"),
    tag: params.get("tag"),
    q: q === null ? null : q.toLowerCase(),
    minPrice: optionalInt(params, "minPrice"),
    maxPrice: optionalInt(params, "maxPrice"),
    inStock: params.get("inStock") === "true",
    sort,
    page: intParam(params, "page", 1, 100000),
    limit: intParam(params, "limit", 20, 100),
  };
}

function orderQuery(query) {
  const params = new URLSearchParams(query);
  const status = params.get("status");
  if (status !== null && !["pending", "paid", "shipped", "cancelled"].includes(status)) {
    throw invalid("status must be one of pending, paid, shipped, cancelled");
  }
  return { status, customer: optionalInt(params, "customer"), page: intParam(params, "page", 1, 100000), limit: intParam(params, "limit", 20, 100) };
}

const notAllowed = (method, path) => new ApiError(405, "method_not_allowed", `${method} is not allowed on ${path}`);
const json = (status, value) => ({ status, body: JSON.stringify(value) });

function products(store, method, rest, query, body) {
  if (rest.length === 0) {
    if (method === "GET") return json(200, store.listProducts(productQuery(query)));
    if (method === "POST") return json(201, store.createProduct(decode(body, newProduct)));
    throw notAllowed(method, "/products");
  }
  if (rest.length === 1 && rest[0] === "bulk") {
    if (method !== "POST") throw notAllowed(method, "/products/bulk");
    return json(200, store.bulkCreate(decode(body, (v, p) => arr(v, p, newProduct))));
  }
  if (rest.length === 1) {
    const id = positiveInt(rest[0], "product id");
    if (method === "GET") return json(200, store.product(id));
    if (method === "PATCH") return json(200, store.patchProduct(id, decode(body, productPatch)));
    throw notAllowed(method, "/products/:id");
  }
  throw new ApiError(404, "not_found", "no such route");
}

function orders(store, method, rest, query, body) {
  if (rest.length === 0) {
    if (method === "GET") return json(200, store.listOrders(orderQuery(query)));
    if (method === "POST") return json(201, store.createOrder(decode(body, newOrder)));
    throw notAllowed(method, "/orders");
  }
  const id = positiveInt(rest[0], "order id");
  if (rest.length === 1) {
    if (method !== "GET") throw notAllowed(method, "/orders/:id");
    return json(200, store.order(id));
  }
  if (rest.length === 2 && rest[1] === "status") {
    if (method !== "POST") throw notAllowed(method, "/orders/:id/status");
    return json(200, store.setStatus(id, decode(body, statusChange).status));
  }
  throw new ApiError(404, "not_found", "no such route");
}

export function route(store, method, path, query, body) {
  try {
    const parts = path.split("/").filter((s) => s !== "");
    if (parts.length === 0 || parts[0] === "health") return { status: 200, body: '{"ok":true}' };
    const rest = parts.slice(1);
    switch (parts[0]) {
      case "products":
        return products(store, method, rest, query, body);
      case "orders":
        return orders(store, method, rest, query, body);
      case "stats":
        if (method !== "GET") throw notAllowed(method, "/stats");
        return json(200, store.stats());
    }
    throw new ApiError(404, "not_found", "no such route");
  } catch (e) {
    if (!(e instanceof ApiError)) throw e;
    return json(e.status, { error: { code: e.code, message: e.message } });
  }
}

const envInt = (name, fallback) => {
  const n = Number.parseInt(process.env[name] ?? "", 10);
  return Number.isNaN(n) ? fallback : n;
};

// Serve only when run directly (`node node/server.mjs`), so the module can be imported.
if (import.meta.url === `file://${process.argv[1]}`) {
  const started = Date.now();
  const store = Store.seeded(42, envInt("SHOP_PRODUCTS", 5000), envInt("SHOP_ORDERS", 20000));
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      const q = req.url.indexOf("?");
      const path = q < 0 ? req.url : req.url.slice(0, q);
      const query = q < 0 ? "" : req.url.slice(q + 1);
      const out = route(store, req.method, path, query, Buffer.concat(chunks).toString("utf8"));
      res.writeHead(out.status, { "content-type": "application/json" });
      res.end(out.body);
    });
  });
  server.listen(envInt("PORT", 8081), "127.0.0.1", () => {
    console.log(`shop-api (node) on http://127.0.0.1:${server.address().port} (seeded in ${Date.now() - started} ms)`);
  });
}
