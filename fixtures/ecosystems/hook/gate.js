// A hook the way a real gate is built: an ES module that imports its
// neighbour, reads a config file beside it, and relies on the `package.json`
// beside it to be a module at all.
import { readFileSync } from "node:fs";
import { verdict } from "./lib.js";

const config = JSON.parse(
  readFileSync(new URL("./config.json", import.meta.url), "utf8"),
);
process.exit(verdict(config) === "allow" ? 0 : 1);
