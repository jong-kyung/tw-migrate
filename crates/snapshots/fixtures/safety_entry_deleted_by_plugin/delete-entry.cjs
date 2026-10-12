const fs = require("node:fs");
const path = require("node:path");

module.exports = () => fs.rmSync(path.join(__dirname, "globals.css"), { force: true });
