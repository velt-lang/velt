// Node twin of dom_instanceof_join.velt: the same program with its types removed.
// Two `instanceof` tests of `lib.dom` interfaces joined by `||` narrow to the union of the two,
// as under `tsc` (sigx's nodeOps, #2739): `focus` is on both through `HTMLOrSVGElement`. On a
// server the test throws `ReferenceError` as on Node, so nothing after it runs.
function focusable(el) {
  if (el instanceof HTMLElement || el instanceof SVGElement) {
    el.focus();
    return el.tagName + " " + el.dataset;
  }
  return "not an element";
}

function three(el) {
  if (el instanceof HTMLDivElement || el instanceof HTMLInputElement || el instanceof SVGElement) {
    el.focus({ preventScroll: true });
    return el.tagName;
  }
  return "other";
}

function onUnknown(el) {
  if (el instanceof HTMLElement || el instanceof SVGElement) {
    el.blur();
    return el.nodeName;
  }
  return "unknown";
}

function report(f) {
  try {
    console.log(f());
  } catch (e) {
    console.log(e.name + ": " + e.message);
  }
}

report(() => focusable(null));
report(() => focusable(document.createElement("div")));
report(() => three(document.createElement("input")));
report(() => onUnknown(1));
