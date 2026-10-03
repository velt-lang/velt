// Code in the common subset: no findings.

export type User = { id: number; name: string; email?: string };

export enum Role {
  Admin = "admin",
  Guest = "guest",
}

export interface Named {
  name(): string;
}

export class Account implements Named {
  constructor(
    readonly owner: User,
    public balance: number,
  ) {}

  name(): string {
    return this.owner.name;
  }

  deposit(amount: number): boolean {
    if (amount <= 0) {
      return false;
    }
    this.balance += amount;
    return true;
  }
}

export async function fetchName(u: User): Promise<string> {
  return u.email ?? u.name;
}

export function total(xs: number[]): number {
  return xs.reduce((a, b) => a + b, 0);
}
