export default async function h(a: string): Promise<string>;
export default async function h(a: number): Promise<number>;
export default async function h(a: any): Promise<any> {
  return a;
}
