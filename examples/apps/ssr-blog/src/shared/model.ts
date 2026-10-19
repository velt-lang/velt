// The blog's data, shared with a TypeScript client (`tsCompat`): plain types and pure helpers in
// the common subset of TypeScript and Velt.

export type Post = {
  slug: string;
  title: string;
  author: string;
  date: string;
  tags: string[];
  paragraphs: string[];
};

export type Comment = { author: string; text: string };

/** Reading time at 200 words a minute, at least one minute. */
export function readingMinutes(post: Post): number {
  let words = 0;
  for (const p of post.paragraphs) {
    words += p.split(" ").length;
  }
  return Math.max(1, Math.ceil(words / 200));
}

/** The first paragraph, cut at a word boundary after at most `max` characters. */
export function excerpt(post: Post, max: number): string {
  const first = post.paragraphs.length > 0 ? post.paragraphs[0] : "";
  if (first.length <= max) {
    return first;
  }
  const cut = first.slice(0, max);
  // A word that ends exactly at `max` stays.
  const space = first.slice(max, max + 1) == " " ? max : cut.lastIndexOf(" ");
  return `${space > 0 ? cut.slice(0, space) : cut}…`;
}
