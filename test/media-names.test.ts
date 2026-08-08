import assert from "node:assert/strict";
import { test } from "vite-plus/test";

import { collectMediaConditions } from "../src/native.ts";
import { allocateMediaNames } from "../src/plan/media.ts";
import type { AuthoredMediaVariant, MediaCollection, MediaProbes } from "../src/plan/media.ts";

function collection(overrides: Partial<MediaCollection> = {}): MediaCollection {
  return { conditions: [], authoredVariants: [], breakpointUnit: "rem", ...overrides };
}

function collect(request: object): MediaCollection {
  const parsed: MediaCollection = JSON.parse(collectMediaConditions(JSON.stringify(request)));
  return parsed;
}

const neverProbes: MediaProbes = { resolves: () => false, expansionMatches: () => false };
const remTokens = { "breakpoint-md": "48rem" };

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
    themeTokens: remTokens,
  });
  const first = allocateMediaNames([collected], remTokens, neverProbes);
  const second = allocateMediaNames([collected], remTokens, neverProbes);
  assert.deepEqual([...first.names.keys()], [...second.names.keys()]);
  assert.equal(first.names.get("(width >= 52rem)")?.name, "min-52rem");
  assert.equal(first.names.get("(width >= 52rem)")?.kind, "breakpoint");
  assert.equal(first.names.get("screen and (width <= 768px)")?.name, "screen-width-lte-768px");
  assert.equal(first.fallbacks.size, 0);
});

test("reserved theme, authored, and explicit names force the digest suffix", () => {
  const authored: AuthoredMediaVariant = {
    name: "screen-width-lte-768px",
    definition: "@custom-variant screen-width-lte-768px { &:hover { @slot; } }",
    mediaQueryKey: null,
    path: "app.css",
  };
  const tokens = { "breakpoint-md": "48rem", "breakpoint-min-52rem": "999rem" };
  const conditions = collect({
    stylesheets: [
      {
        cssPath: "card.css",
        cssSource:
          "@media (min-width: 52rem) { .card { padding: 2rem; } }\n" +
          "@media screen and (width <= 768px) { .card { margin: 0; } }\n" +
          "@media (width <= 900px) { .card { color: red; } }",
      },
    ],
    themeTokens: tokens,
  }).conditions;
  const allocation = allocateMediaNames(
    [collection({ conditions, authoredVariants: [authored] })],
    tokens,
    neverProbes,
    ["width-lte-900px"],
  );
  const breakpoint = allocation.names.get("(width >= 52rem)");
  assert.ok(breakpoint);
  assert.match(breakpoint.name, /^min-52rem-[0-9a-f]{8}$/);
  const variant = allocation.names.get("screen and (width <= 768px)");
  assert.ok(variant);
  assert.match(variant.name, /^screen-width-lte-768px-[0-9a-f]{8}$/);
  const scanned = allocation.names.get("(width <= 900px)");
  assert.ok(scanned);
  assert.match(scanned.name, /^width-lte-900px-[0-9a-f]{8}$/);
});

test("bounded probing escapes functional namespaces or falls back", () => {
  const collected = collect({
    stylesheets: [
      {
        cssPath: "card.css",
        cssSource: "@media screen and (width <= 768px) { .card { margin: 0; } }",
      },
    ],
    themeTokens: remTokens,
  });

  const namespaceOwned: MediaProbes = {
    resolves: (name) => name.startsWith("screen-"),
    expansionMatches: () => false,
  };
  const escaped = allocateMediaNames([collected], remTokens, namespaceOwned);
  const escapedName = escaped.names.get("screen and (width <= 768px)");
  assert.ok(escapedName);
  assert.match(escapedName.name, /^twm-media-[0-9a-f]{8}$/);

  const everything: MediaProbes = { resolves: () => true, expansionMatches: () => false };
  const exhausted = allocateMediaNames([collected], remTokens, everything);
  assert.equal(exhausted.names.size, 0);
  assert.ok(exhausted.fallbacks.has("screen and (width <= 768px)"));
});

test("reuses an authored definition only with an expansion proof", () => {
  const conditions = collect({
    stylesheets: [
      { cssPath: "card.css", cssSource: "@media (width <= 768px) { .card { margin: 0; } }" },
    ],
    themeTokens: remTokens,
  }).conditions;
  const matching: AuthoredMediaVariant = {
    name: "width-lte-768px",
    definition: "@custom-variant width-lte-768px { @media (width <= 768px) { @slot; } }",
    mediaQueryKey: "(width <= 768px)",
    path: "app.css",
  };
  const proving: MediaProbes = {
    resolves: (name) => name === "width-lte-768px",
    expansionMatches: (name, key) => name === "width-lte-768px" && key === "(width <= 768px)",
  };

  const reused = allocateMediaNames(
    [collection({ conditions, authoredVariants: [matching] })],
    remTokens,
    proving,
  );
  const entry = reused.names.get("(width <= 768px)");
  assert.ok(entry);
  assert.equal(entry.reused, true);
  assert.equal(entry.name, "width-lte-768px");

  // A competing opaque registration is invisible to the authored list; the
  // failing expansion proof still blocks reuse and forces the suffix.
  const opaque: MediaProbes = {
    resolves: (name) => name === "width-lte-768px",
    expansionMatches: () => false,
  };
  const blocked = allocateMediaNames(
    [collection({ conditions, authoredVariants: [matching] })],
    remTokens,
    opaque,
  );
  const suffixed = blocked.names.get("(width <= 768px)");
  assert.ok(suffixed);
  assert.equal(suffixed.reused, false);
  assert.match(suffixed.name, /^width-lte-768px-[0-9a-f]{8}$/);

  const competing: AuthoredMediaVariant = { ...matching, path: "other.css", mediaQueryKey: null };
  const collided = allocateMediaNames(
    [collection({ conditions, authoredVariants: [matching, competing] })],
    remTokens,
    proving,
  );
  const twice = collided.names.get("(width <= 768px)");
  assert.ok(twice);
  assert.equal(twice.reused, false);
});

test("reuses a matching authored definition under a non-generated name", () => {
  const conditions = collect({
    stylesheets: [
      { cssPath: "card.css", cssSource: "@media (width <= 768px) { .card { margin: 0; } }" },
    ],
    themeTokens: remTokens,
  }).conditions;
  const tablet: AuthoredMediaVariant = {
    name: "tablet",
    definition: "@custom-variant tablet { @media (width <= 768px) { @slot; } }",
    mediaQueryKey: "(width <= 768px)",
    path: "app.css",
  };
  const proving: MediaProbes = {
    resolves: (name) => name === "tablet",
    expansionMatches: (name, key) => name === "tablet" && key === "(width <= 768px)",
  };
  const allocation = allocateMediaNames(
    [collection({ conditions, authoredVariants: [tablet] })],
    remTokens,
    proving,
  );
  const entry = allocation.names.get("(width <= 768px)");
  assert.ok(entry);
  assert.equal(entry.reused, true);
  assert.equal(entry.name, "tablet");
});

test("duplicate declarations inside one source stay collisions", () => {
  const conditions = collect({
    stylesheets: [
      { cssPath: "card.css", cssSource: "@media (width <= 768px) { .card { margin: 0; } }" },
    ],
    themeTokens: remTokens,
  }).conditions;
  const row: AuthoredMediaVariant = {
    name: "tablet",
    definition: "@custom-variant tablet { @media (width <= 768px) { @slot; } }",
    mediaQueryKey: "(width <= 768px)",
    path: "app.css",
  };
  const proving: MediaProbes = {
    resolves: (name) => name === "tablet",
    expansionMatches: (name, key) => name === "tablet" && key === "(width <= 768px)",
  };

  // Two byte-identical declarations in one collection are one source
  // declaring the variant twice, which is a collision.
  const duplicated = allocateMediaNames(
    [collection({ conditions, authoredVariants: [row, { ...row }] })],
    remTokens,
    proving,
  );
  assert.equal(duplicated.names.get("(width <= 768px)")?.reused, false);

  // The same row reported by two package collections is one shared entry
  // graph and deduplicates back to a single reusable registration.
  const shared = allocateMediaNames(
    [
      collection({ conditions, authoredVariants: [row] }),
      collection({ authoredVariants: [{ ...row }] }),
    ],
    remTokens,
    proving,
  );
  assert.equal(shared.names.get("(width <= 768px)")?.reused, true);
});

test("cross-package conditions share one deduplicated allocation", () => {
  const first = collect({
    stylesheets: [
      { cssPath: "a/card.css", cssSource: "@media (width <= 768px) { .card { margin: 0; } }" },
    ],
    themeTokens: remTokens,
  });
  const second = collect({
    stylesheets: [
      { cssPath: "b/hero.css", cssSource: "@media (max-width: 768px) { .hero { margin: 1px; } }" },
    ],
    themeTokens: remTokens,
  });
  const allocation = allocateMediaNames([first, second], remTokens, neverProbes);
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
  const allocation = allocateMediaNames([collection({ conditions })], {}, neverProbes);
  assert.equal(allocation.names.get("(width <= 48em)")?.name, "width-lte-768px");
  assert.equal(allocation.names.get("(width <= 768px)")?.name, "width-lte-768px-11111111");
});

test("identical digests for distinct keys never share a name", () => {
  const shared = {
    kind: "customVariant" as const,
    preferredName: "width-gte-env",
    digest: "deadbeef",
    cssPath: "card.css",
  };
  const conditions = [
    { ...shared, key: "(width >= env(A))", order: 1 },
    { ...shared, key: "(width >= env(B))", order: 2 },
    { ...shared, key: "(width >= env(C))", order: 3 },
    { ...shared, key: "(width >= env(D))", order: 4 },
  ];
  const allocation = allocateMediaNames([collection({ conditions })], {}, neverProbes);
  const names = [...allocation.names.values()].map((entry) => entry.name);
  assert.equal(new Set(names).size, names.length);
  // The preferred, digest-suffixed, and namespaced forms are each claimed
  // once; the remaining key falls back to the arbitrary variant instead of
  // sharing a name.
  assert.equal(names.length, 3);
  assert.equal(allocation.fallbacks.size, 1);
});

test("overlong preferred names are limited before allocation", () => {
  const condition = {
    key: "(width <= 1e60rem)",
    kind: "customVariant" as const,
    preferredName: `width-lte-${"1".repeat(60)}rem`,
    digest: "abcdef01",
    cssPath: "card.css",
    order: 1,
  };
  const allocation = allocateMediaNames([collection({ conditions: [condition] })], {}, neverProbes);
  const entry = allocation.names.get(condition.key);
  assert.ok(entry);
  assert.ok(entry.name.length <= 48, entry.name);
  assert.match(entry.name, /-abcdef01$/);
});
