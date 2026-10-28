import { expect, test } from "bun:test";
import { seconds } from "./index";

test("reads a duration in seconds", () => {
  expect(seconds("2m")).toBe(120);
});
