// An array buffer larger than memory (2^51 + 64 bytes, within the length limit) whose size is known
// only when the program runs (`zero` comes from the clock): the allocator's error. In a release
// build that is the allocation wrapper's (rt_abi.md "Memory"), not the inlined hook a constant size
// takes (array_alloc_past_memory.velt). Node stops at JS's 2^32 - 1 elements
// (`RangeError: Invalid array length`).
function main() {
  console.log("before");
  const zero = Math.min(Date.now(), 0);
  const xs = new Array(zero + 2 ** 48 + 8).fill(0);
  console.log(xs.length);
}
main();
