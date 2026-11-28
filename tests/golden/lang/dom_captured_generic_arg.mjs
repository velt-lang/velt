// Node twin of dom_captured_generic_arg.velt: the same program with its types removed.
// A variable of the enclosing function, captured by a closure and narrowed there, is narrowed
// for a generic argument as for a receiver (`removeChild<T extends Node>(child: T)`, sigx's
// Portal, #2739), as under `tsc`: on a server the DOM is never reached, as on Node.
function on(cb) {
  cb();
}

function portal(target) {
  let pc = null;
  const mount = () => {
    pc = document.createElement("div");
    document.body.appendChild(pc);
  };
  on(() => {
    if (pc && pc.parentNode) {
      pc.parentNode.removeChild(pc);
    }
  });
  on(function () {
    if (pc !== null && pc.parentNode !== null) {
      pc.parentNode.insertBefore(pc, null);
    }
    if (pc && target) {
      target.appendChild(pc);
    }
    pc = null;
  });
  try {
    mount();
  } catch (e) {
    return e.name + ": " + e.message;
  }
  return "mounted";
}

console.log(portal(null));
