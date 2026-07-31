import assert from "node:assert/strict";
import { test } from "vite-plus/test";

import { formatDiagnostic } from "../bin/tw-migrate.js";

test("colors terminal diagnostics unless NO_COLOR is set", () => {
  const isTTY = Object.getOwnPropertyDescriptor(process.stderr, "isTTY");
  const noColor = process.env.NO_COLOR;
  try {
    Object.defineProperty(process.stderr, "isTTY", { configurable: true, value: true });
    delete process.env.NO_COLOR;
    assert.equal(formatDiagnostic("warning", "38;5;208"), "\x1b[38;5;208mwarning\x1b[0m");
    assert.equal(formatDiagnostic("error", "31"), "\x1b[31merror\x1b[0m");

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
