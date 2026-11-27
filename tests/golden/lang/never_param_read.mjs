// The Node twin of never_param_read.velt.
function store(e) {
  const x = e;
  console.log(x.message);
}
function cast(e) {
  const x = e;
  console.log(x.message);
}
const arrow = (e) => {
  console.log(e.name);
};
const fs = [store, cast, arrow];
console.log("ok", fs.length);
