// A TypeScript interface only declares its methods.

export interface Greeter {
  name(): string;
  greet(): string {
    return `hi ${this.name()}`;
  }
}
