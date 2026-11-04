export const x = 1;

export default 9;

export interface Shape {
  area(): number;
}

export class Square implements Shape {
  side: number;

  constructor(side: number) {
    this.side = side;
  }

  area(): number {
    return this.side * this.side;
  }
}

export enum E {
  P,
  Q,
}
