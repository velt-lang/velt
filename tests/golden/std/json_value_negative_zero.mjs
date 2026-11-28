// The Node twin of tests/golden/std/json_value_negative_zero.velt.
console.log(JSON.parse("-0"));
console.log(JSON.parse("[-0, 0, -0.5]"));
console.log(JSON.parse("{\"a\": -0}"));
