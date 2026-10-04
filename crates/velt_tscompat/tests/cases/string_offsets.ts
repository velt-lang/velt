// Lengths and positions in strings are UTF-8 bytes in Velt, UTF-16 code units in JavaScript.
// ASCII string literals agree.

export function initials(name: string): string {
  if (name.length > 3) { //~ string-offsets
    return name.slice(0, 2); //~ string-offsets
  }
  return name[0]; //~ string-offsets
}

export function width(): number {
  return "abc".length + "Zoë".length; //~ string-offsets
}

export function words(s: string): string[] {
  return s.split(" ");
}
