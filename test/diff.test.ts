import assert from "node:assert/strict";
import { test } from "vite-plus/test";

import { unifiedDiff } from "../src/util/diff.ts";

const lines = Array.from({ length: 20 }, (_, index) => `line ${index + 1}`);
const source = `${lines.join("\n")}\n`;

test("diff omits unchanged files", () => {
  for (const text of ["", source, "without a final newline", "CRLF\r\n"]) {
    assert.equal(unifiedDiff("file.ts", text, text), "");
  }
});

test("diff shows three context lines around a change", () => {
  assert.equal(
    unifiedDiff("file.ts", source, source.replace("line 8\n", "changed 8\n")),
    [
      "--- a/file.ts",
      "+++ b/file.ts",
      "@@ -5,7 +5,7 @@",
      " line 5",
      " line 6",
      " line 7",
      "-line 8",
      "+changed 8",
      " line 9",
      " line 10",
      " line 11",
      "",
    ].join("\n"),
  );
});

test("diff merges changes with overlapping or touching context", () => {
  const after = source.replace("line 5\n", "changed 5\n").replace("line 12\n", "changed 12\n");
  const diff = unifiedDiff("file.ts", source, after);
  assert.equal((diff.match(/^@@ /gm) ?? []).length, 1);
  assert.match(diff, /@@ -2,14 \+2,14 @@/);
  assert.match(diff, /-line 5\n\+changed 5\n/);
  assert.match(diff, /-line 12\n\+changed 12\n/);
  assert.doesNotMatch(diff, /^ line (1|16|17|18|19|20)$/m);
});

test("diff separates distant changes without repeating context", () => {
  const after = source.replace("line 5\n", "changed 5\n").replace("line 13\n", "changed 13\n");
  const diff = unifiedDiff("file.ts", source, after);
  assert.equal((diff.match(/^@@ /gm) ?? []).length, 2);
  assert.match(diff, /@@ -2,7 \+2,7 @@/);
  assert.match(diff, /@@ -10,7 \+10,7 @@/);
  assert.doesNotMatch(diff, /^ line 9$/m);
});

test("diff tracks line offsets after insertions and deletions", () => {
  const after = source.replace("line 5\n", "inserted\nline 5\n").replace("line 13\n", "");
  const diff = unifiedDiff("file.ts", source, after);
  assert.equal((diff.match(/^@@ /gm) ?? []).length, 2);
  assert.match(diff, /@@ -2,6 \+2,7 @@/);
  assert.match(diff, /\+inserted\n line 5\n/);
  assert.match(diff, /@@ -10,7 \+11,6 @@/);
  assert.match(diff, / line 12\n-line 13\n line 14\n/);
});

test("diff displays additions and deletions at file boundaries", () => {
  assert.equal(
    unifiedDiff("file.ts", "", "new\n"),
    "--- a/file.ts\n+++ b/file.ts\n@@ -0,0 +1,1 @@\n+new\n",
  );
  assert.equal(
    unifiedDiff("file.ts", "old\n", ""),
    "--- a/file.ts\n+++ b/file.ts\n@@ -1,1 +0,0 @@\n-old\n",
  );
});

test("diff distinguishes a missing final newline", () => {
  assert.equal(
    unifiedDiff("file.ts", "same", "same\n"),
    "--- a/file.ts\n+++ b/file.ts\n@@ -1,1 +1,1 @@\n-same\n\\ No newline at end of file\n+same\n",
  );
});

test("diff preserves Unicode content and CRLF lines", () => {
  const diff = unifiedDiff("file.ts", "앞줄\r\n이전 😀\r\n뒷줄\r\n", "앞줄\r\n이후 😀\r\n뒷줄\r\n");
  assert.match(diff, / 앞줄\r\n-이전 😀\r\n\+이후 😀\r\n 뒷줄\r\n/);
});
