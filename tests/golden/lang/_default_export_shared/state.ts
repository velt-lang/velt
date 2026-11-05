let c = 0;

export default {
  inc(): number {
    c++;
    return c;
  },
  get v(): number {
    return c;
  },
};
