// Node twin of dom_assign_module.velt: the same program with its types removed.
// A module variable assigned a DOM call's result holds what the call's type says, as a
// function's variable does (dom_assign_unreached.velt), also through a cast of the result
// (sigx's Portal, #2739): reading `document` throws `ReferenceError` as on Node, so the line
// after it never runs, and it type-checks as under `tsc`.
let container = null;
let input = null;
let label;

function mount() {
  container = document.createElement("div");
  container.setAttribute("data-portal", "");
}

function mountCast() {
  input = document.createElement("input");
  input.value = "x";
}

function mountLocal() {
  const el = document.createElement("span");
  el.textContent = "y";
  label = document.body;
  return label.tagName;
}

function report(f) {
  try {
    f();
    console.log("ran");
  } catch (e) {
    console.log(e.name + ": " + e.message);
  }
}

report(mount);
report(mountCast);
report(() => console.log(mountLocal()));
console.log(container === null, input === null, label === undefined);
