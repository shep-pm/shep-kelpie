// A hook the way a real gate is built: an ES module that imports its
// neighbour and reads a config file and the `package.json` beside it. Reading
// the `package.json` itself, not only relying on it for `"type": "module"`,
// makes a fence that hides it fail the hook.
import { readFileSync } from "node:fs";
import { verdict } from "./lib.js";

const beside = (name) =>
  JSON.parse(readFileSync(new URL(name, import.meta.url), "utf8"));

const config = beside("./config.json");
const manifest = beside("./package.json");
process.exit(manifest.type === "module" && verdict(config) === "allow" ? 0 : 1);
