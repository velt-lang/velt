// Node twin of print_interface_literal_setter.velt: the same program with its types removed.
const gs = {
  get v() {
    return 1;
  },
  set v(x) {},
  describe() {
    return "y";
  },
};
console.log(gs);
