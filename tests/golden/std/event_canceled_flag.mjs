class Loud extends Event {
  get defaultPrevented() {
    console.log("  Loud.defaultPrevented");
    return true;
  }
  preventDefault() {
    console.log("  Loud.preventDefault");
  }
}

for (const cancelable of [false, true]) {
  console.log(`cancelable ${cancelable}`);
  const e = new Loud("x", { cancelable });
  console.log("get returnValue:", e.returnValue);
  e.returnValue = false;
  console.log("after returnValue = false:", e.returnValue);
  e.returnValue = true;
  console.log("after returnValue = true:", e.returnValue);
}

const plain = new Event("y", { cancelable: true });
plain.returnValue = false;
console.log("plain:", plain.returnValue, plain.defaultPrevented);

const t = new EventTarget();
t.addEventListener("z", (e) => {
  e.returnValue = false;
});
console.log("dispatch:", t.dispatchEvent(new Loud("z", { cancelable: true })));
t.addEventListener(
  "p",
  (e) => {
    e.returnValue = false;
    console.log("passive listener:", e.returnValue, e.defaultPrevented);
  },
  { passive: true },
);
console.log("passive dispatch:", t.dispatchEvent(new Event("p", { cancelable: true })));
