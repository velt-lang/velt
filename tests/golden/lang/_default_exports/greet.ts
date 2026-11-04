// `export default function f`: a live binding of the declaration, hoisted.
export default function greet(name: string): string {
  return `hello ${name}`;
}
