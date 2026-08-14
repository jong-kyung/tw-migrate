import assert from "node:assert/strict";

import { test } from "vite-plus/test";

import { decodeSourceMap, planBatchMigration } from "../src/native.ts";

const recoverablePrefix = "TW_MIGRATE_RECOVERABLE_INPUT:";

test("batch planning prefixes malformed Tailwind entry CSS", () => {
  const request = {
    tailwindPath: "tailwind.css",
    tailwindSource: "@media \u000bscreen {}",
    files: [],
    stylesheets: [
      {
        cssPath: "tokens.module.css",
        cssSource: '@property --x { syntax: "*"; inherits: false; initial-value: 0; }',
        isModule: true,
      },
    ],
  };

  assert.throws(
    () => planBatchMigration(JSON.stringify(request)),
    (error: unknown) => {
      assert(error instanceof Error);
      assert.equal(
        error.message,
        `${recoverablePrefix}Failed to parse Tailwind CSS: Error { kind: Unexpected("<ident>", "<unknown>"), span: Span { start: 7, end: 8 } }`,
      );
      return true;
    },
  );
});

test("source-map decoding failures stay fatal and unprefixed", () => {
  assert.throws(
    () => decodeSourceMap("{"),
    (error: unknown) => {
      assert(error instanceof Error);
      assert.equal(
        error.message,
        "Failed to decode source map: JSON parsing error: EOF while parsing an object at line 1 column 1",
      );
      return true;
    },
  );
});
