import assert from "node:assert/strict";
import { test } from "vite-plus/test";

import { formatDiagnostic } from "../src/diagnostics.js";

test("colors terminal diagnostics unless NO_COLOR is set", () => {
  const ansiEscape = String.fromCharCode(27);
  const isTTY = Object.getOwnPropertyDescriptor(process.stderr, "isTTY");
  const noColor = process.env.NO_COLOR;
  try {
    Object.defineProperty(process.stderr, "isTTY", { configurable: true, value: true });
    delete process.env.NO_COLOR;
    assert.equal(
      formatDiagnostic("warning", "38;5;208"),
      `${ansiEscape}[38;5;208mwarning${ansiEscape}[0m`,
    );
    assert.equal(formatDiagnostic("error", "31"), `${ansiEscape}[31merror${ansiEscape}[0m`);

    process.env.NO_COLOR = "1";
    assert.equal(formatDiagnostic("warning", "38;5;208"), "warning");

    delete process.env.NO_COLOR;
    Object.defineProperty(process.stderr, "isTTY", { configurable: true, value: false });
    assert.equal(formatDiagnostic("error", "31"), "error");
  } finally {
    if (isTTY) Object.defineProperty(process.stderr, "isTTY", isTTY);
    else delete (process.stderr as { isTTY?: boolean }).isTTY;
    if (noColor === undefined) delete process.env.NO_COLOR;
    else process.env.NO_COLOR = noColor;
  }
});
