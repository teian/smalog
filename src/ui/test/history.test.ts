import assert from "node:assert/strict";
import { test } from "node:test";

import { numericValue } from "../src/lib/history.ts";

test("a metric the inverter does not report reads as absent, not as zero", () => {
  // The SB 2500 has no temperature sensor: the API sends null, and
  // Number(null) would be 0.
  assert.equal(numericValue(null), null);
  assert.equal(numericValue(undefined), null);
});

test("real values survive, including a genuine zero", () => {
  assert.equal(numericValue(0), 0);
  assert.equal(numericValue("21.5"), 21.5);
  assert.equal(numericValue(1_234), 1_234);
});

test("anything unparseable counts as absent", () => {
  assert.equal(numericValue("nope"), null);
  assert.equal(numericValue(""), null);
});
