const fs = require("node:fs");

fs.mkdirSync("dist", { recursive: true });
fs.copyFileSync("src/index.js", "dist/index.js");
