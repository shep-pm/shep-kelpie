import { readdirSync, readFileSync } from "node:fs";

const banned = /\b(debugger|console\.log)\b/;
let failed = false;
for (const name of readdirSync("src")) {
  readFileSync(`src/${name}`, "utf8")
    .split("\n")
    .forEach((line, at) => {
      if (banned.test(line)) {
        console.error(`src/${name}:${at + 1}: ${line.trim()}`);
        failed = true;
      }
    });
}
process.exit(failed ? 1 : 0);
