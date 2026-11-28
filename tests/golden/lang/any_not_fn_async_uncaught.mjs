// Node twin of any_not_fn_async_uncaught.velt: the same program with its types removed.
const o = { ob: 5 };
console.log("before");
(async () => {
  o.ob();
})();
console.log("after");
