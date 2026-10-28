const assert = require("node:assert");
const test = require("node:test");
const { seconds } = require("./index");

test("reads a duration in seconds", () => {
  assert.strictEqual(seconds("2m"), 120);
});
