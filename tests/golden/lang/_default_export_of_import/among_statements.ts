// Evaluated where it stands among the statements.
import { v as w, set } from "./among_source";

console.log("before", w);
set(2);

export default w;

set(3);
console.log("after", w);
