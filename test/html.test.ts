import assert from "node:assert/strict";

import { test } from "vite-plus/test";

import { parseHtmlSource, withHtmlParseCache } from "../src/parser/html.ts";

test("shares HTML parses within one run, including parse failures", async () => {
  const page = '<link rel="stylesheet" href="./a.css"><div class="a"></div>';
  const broken = '<div class="a"></div><';
  const [first, second, edited, firstError, secondError] = await withHtmlParseCache(async () => {
    const parsed = parseHtmlSource("/p/index.html", page);
    await Promise.resolve();
    const failure = () => {
      try {
        parseHtmlSource("/p/broken.html", broken);
      } catch (error) {
        return error;
      }
      throw new Error("expected a parse failure");
    };
    return [
      parsed,
      parseHtmlSource("/p/index.html", page),
      parseHtmlSource("/p/index.html", `${page}\n`),
      failure(),
      failure(),
    ];
  });
  assert.equal(first, second);
  assert.notEqual(first, edited);
  assert.ok(firstError instanceof Error);
  assert.equal(firstError, secondError);
  assert.notEqual(parseHtmlSource("/p/index.html", page), first);
});
