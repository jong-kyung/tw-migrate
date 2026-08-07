import assert from "node:assert/strict";
import { test } from "vite-plus/test";

import { collectMediaConditions } from "../src/native.ts";
import { allocateMediaNames } from "../src/plan/media.ts";
import type { AuthoredMediaVariant, MediaCollection } from "../src/plan/media.ts";

function collection(overrides: Partial<MediaCollection> = {}): MediaCollection {
  return { conditions: [], authoredVariants: [], breakpointUnit: "rem", ...overrides };
}

function collect(request: object): MediaCollection {
  const parsed: MediaCollection = JSON.parse(collectMediaConditions(JSON.stringify(request)));
  return parsed;
}

const never = (): boolean => false;

test("allocates preferred names and stays deterministic", () => {
  const collected = collect({
    stylesheets: [
      {
        cssPath: "card.css",
        cssSource:
          "@media (min-width: 52rem) { .card { padding: 2rem; } }\n" +
          "@media screen and (width <= 768px) { .card { margin: 0; } }",
      },
    ],
    themeTokens: { "breakpoint-md": "48rem" },
  });
  const first = allocateMediaNames([collected], { "breakpoint-md": "48rem" }, never);
  const second = allocateMediaNames([collected], { "breakpoint-md": "48rem" }, never);
  assert.deepEqual([...first.names.keys()], [...second.names.keys()]);
  assert.equal(first.names.get("(width >= 52rem)")?.name, "min-52rem");
  assert.equal(first.names.get("(width >= 52rem)")?.kind, "breakpoint");
  assert.equal(first.names.get("screen and (width <= 768px)")?.name, "screen-width-lte-768px");
  assert.equal(first.fallbacks.size, 0);
});

test("reserved theme and authored names force the digest suffix", () => {
  const authored: AuthoredMediaVariant = {
    name: "screen-width-lte-768px",
    definition: "@custom-variant screen-width-lte-768px { &:hover { @slot; } }",
    mediaQueryKey: null,
    path: "app.css",
  };
  const conditions = collect({
    stylesheets: [
      {
        cssPath: "card.css",
        cssSource:
          "@media (min-width: 52rem) { .card { padding: 2rem; } }\n" +
          "@media screen and (width <= 768px) { .card { margin: 0; } }",
      },
    ],
    themeTokens: { "breakpoint-md": "48rem", "breakpoint-min-52rem": "999rem" },
  }).conditions;
  const allocation = allocateMediaNames(
    [collection({ conditions, authoredVariants: [authored] })],
    { "breakpoint-md": "48rem", "breakpoint-min-52rem": "999rem" },
    never,
  );
  const breakpoint = allocation.names.get("(width >= 52rem)");
  assert.ok(breakpoint);
  assert.notEqual(breakpoint.name, "min-52rem");
  assert.match(breakpoint.name, /^min-52rem-[0-9a-f]{8}$/);
  const variant = allocation.names.get("screen and (width <= 768px)");
  assert.ok(variant);
  assert.match(variant.name, /^screen-width-lte-768px-[0-9a-f]{8}$/);
});

test("bounded probing escapes functional namespaces or falls back", () => {
  const collected = collect({
    stylesheets: [
      {
        cssPath: "card.css",
        cssSource: "@media screen and (width <= 768px) { .card { margin: 0; } }",
      },
    ],
    themeTokens: { "breakpoint-md": "48rem" },
  });
  const themeTokens = { "breakpoint-md": "48rem" };

  const namespaceOwned = (name: string): boolean => name.startsWith("screen-");
  const escaped = allocateMediaNames([collected], themeTokens, namespaceOwned);
  const escapedName = escaped.names.get("screen and (width <= 768px)");
  assert.ok(escapedName);
  assert.match(escapedName.name, /^twm-media-[0-9a-f]{8}$/);

  const everything = (): boolean => true;
  const exhausted = allocateMediaNames([collected], themeTokens, everything);
  assert.equal(exhausted.names.size, 0);
  assert.ok(exhausted.fallbacks.has("screen and (width <= 768px)"));
});

test("reuses a single authored definition only with a proven meaning match", () => {
  const conditions = collect({
    stylesheets: [
      { cssPath: "card.css", cssSource: "@media (width <= 768px) { .card { margin: 0; } }" },
    ],
    themeTokens: { "breakpoint-md": "48rem" },
  }).conditions;
  const matching: AuthoredMediaVariant = {
    name: "width-lte-768px",
    definition: "@custom-variant width-lte-768px { @media (width <= 768px) { @slot; } }",
    mediaQueryKey: "(width <= 768px)",
    path: "app.css",
  };
  const probe = (name: string): boolean => name === "width-lte-768px";

  const reused = allocateMediaNames(
    [collection({ conditions, authoredVariants: [matching] })],
    { "breakpoint-md": "48rem" },
    probe,
  );
  const entry = reused.names.get("(width <= 768px)");
  assert.ok(entry);
  assert.equal(entry.reused, true);
  assert.equal(entry.name, "width-lte-768px");

  const competing: AuthoredMediaVariant = { ...matching, path: "other.css", mediaQueryKey: null };
  const collided = allocateMediaNames(
    [collection({ conditions, authoredVariants: [matching, competing] })],
    { "breakpoint-md": "48rem" },
    probe,
  );
  const suffixed = collided.names.get("(width <= 768px)");
  assert.ok(suffixed);
  assert.equal(suffixed.reused, false);
  assert.match(suffixed.name, /^width-lte-768px-[0-9a-f]{8}$/);
});

test("cross-package conditions share one deduplicated allocation", () => {
  const first = collect({
    stylesheets: [
      { cssPath: "a/card.css", cssSource: "@media (width <= 768px) { .card { margin: 0; } }" },
    ],
    themeTokens: { "breakpoint-md": "48rem" },
  });
  const second = collect({
    stylesheets: [
      { cssPath: "b/hero.css", cssSource: "@media (max-width: 768px) { .hero { margin: 1px; } }" },
    ],
    themeTokens: { "breakpoint-md": "48rem" },
  });
  const allocation = allocateMediaNames([first, second], { "breakpoint-md": "48rem" }, never);
  assert.equal(allocation.names.size, 1);
  const entry = allocation.names.get("(width <= 768px)");
  assert.ok(entry);
  assert.equal(entry.cssPath, "a/card.css");
});

test("distinct keys with one preferred name receive stable digest suffixes", () => {
  const shared = {
    kind: "customVariant" as const,
    preferredName: "width-lte-768px",
    cssPath: "card.css",
  };
  const conditions = [
    { ...shared, key: "(width <= 768px)", digest: "11111111", order: 1 },
    { ...shared, key: "(width <= 48em)", digest: "22222222", order: 2 },
  ];
  const allocation = allocateMediaNames([collection({ conditions })], {}, never);
  assert.equal(allocation.names.get("(width <= 48em)")?.name, "width-lte-768px");
  assert.equal(allocation.names.get("(width <= 768px)")?.name, "width-lte-768px-11111111");
});
