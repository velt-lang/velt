// The shape of the small npm packages #1916 is about: one TypeScript file whose only export is
// a default generic function.
export type Handler<T> = (event: T) => void;

export interface Emitter<T> {
  on(handler: Handler<T>): void;
  emit(event: T): void;
}

export default function emitter<T>(): Emitter<T> {
  const handlers: Handler<T>[] = [];
  return {
    on(handler: Handler<T>): void {
      handlers.push(handler);
    },
    emit(event: T): void {
      for (const h of handlers) {
        h(event);
      }
    },
  };
}
