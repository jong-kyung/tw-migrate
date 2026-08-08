import assert from "node:assert/strict";
import { test } from "vite-plus/test";

import { collectMediaConditions } from "../src/native.ts";
import { resolveMediaNames } from "../src/plan/media.ts";
import type { AuthoredMediaVariant, MediaCollection, MediaProbes } from "../src/plan/media.ts";

function collection(overrides: Partial<MediaCollection> = {}): MediaCollection {
  return { components: [], authoredVariants: [], ...overrides };
}

function collect(request: object): MediaCollection {
  const parsed: MediaCollection = JSON.parse(collectMediaConditions(JSON.stringify(request)));
  return parsed;
}

const neverProbes: MediaProbes = { resolves: () => false, expansionMatches: () => false };
const remTokens = { "breakpoint-md": "48rem", "breakpoint-lg": "64rem" };

test("verified built-ins are reused and unverified ones are generated", () => {
  const collected = collect({
    stylesheets: [
      {
        cssPath: "card.css",
        cssSource: "@media screen and (prefers-color-scheme: dark) { .card { color: white; } }",
      },
    ],
    themeTokens: remTokens,
  });

  const verifying: MediaProbes = {
    resolves: (name) => name === "dark",
    expansionMatches: (name, key) => name === "dark" && key === "(prefers-color-scheme: dark)",
  };
  const verified = resolveMediaNames([collected], remTokens, verifying);
  const dark = verified.names.get("(prefers-color-scheme: dark)");
  assert.ok(dark);
  assert.equal(dark.kind, "builtin");
  assert.equal(dark.name, "dark");
  assert.equal(verified.names.get("screen")?.kind, "generated");
  assert.equal(verified.names.get("screen")?.name, "screen");

  // A project that redefined dark with selector semantics fails the
  // expansion proof, so the condition receives its own component variant.
  const redefined: MediaProbes = {
    resolves: (name) => name === "dark",
    expansionMatches: () => false,
  };
  const generated = resolveMediaNames([collected], remTokens, redefined);
  const own = generated.names.get("(prefers-color-scheme: dark)");
  assert.ok(own);
  assert.equal(own.kind, "generated");
  assert.equal(own.name, "prefers-color-scheme-dark");
});

test("existing breakpoints are reused per component including max forms", () => {
  const collected = collect({
    stylesheets: [
      {
        cssPath: "card.css",
        cssSource: "@media screen and (48rem <= width < 64rem) { .card { margin: 0; } }",
      },
    ],
    themeTokens: remTokens,
  });
  const resolution = resolveMediaNames([collected], remTokens, neverProbes);
  assert.equal(resolution.names.get("(width >= 48rem)")?.kind, "breakpoint");
  assert.equal(resolution.names.get("(width >= 48rem)")?.name, "md");
  assert.equal(resolution.names.get("(width < 64rem)")?.kind, "breakpoint");
  assert.equal(resolution.names.get("(width < 64rem)")?.name, "max-lg");
  assert.equal(resolution.names.get("screen")?.kind, "generated");
});

test("identical existing definitions are adopted regardless of authorship", () => {
  const collected = collect({
    stylesheets: [
      { cssPath: "card.css", cssSource: "@media (width <= 768px) { .card { margin: 0; } }" },
    ],
    themeTokens: remTokens,
  });
  const identical: AuthoredMediaVariant = {
    name: "width-lte-768px",
    definition: "@custom-variant width-lte-768px { @media (width <= 768px) { @slot; } }",
    mediaQueryKey: "(width <= 768px)",
    path: "app.css",
  };
  const adopted = resolveMediaNames(
    [collection({ components: collected.components, authoredVariants: [identical] })],
    remTokens,
    neverProbes,
  );
  const entry = adopted.names.get("(width <= 768px)");
  assert.ok(entry);
  assert.equal(entry.kind, "adopted");
  assert.equal(entry.name, "width-lte-768px");

  // The same name with a different meaning is never touched; the component
  // falls to the digest name.
  const different: AuthoredMediaVariant = { ...identical, mediaQueryKey: "(width <= 900px)" };
  const blocked = resolveMediaNames(
    [collection({ components: collected.components, authoredVariants: [different] })],
    remTokens,
    neverProbes,
  );
  const digest = blocked.names.get("(width <= 768px)");
  assert.ok(digest);
  assert.equal(digest.kind, "generated");
  assert.match(digest.name, /^twm-media-[0-9a-f]{16}$/);
});

test("owned names fall to the digest and owned digests fall back", () => {
  const collected = collect({
    stylesheets: [
      { cssPath: "card.css", cssSource: "@media (width <= 768px) { .card { margin: 0; } }" },
    ],
    themeTokens: remTokens,
  });
  const component = collected.components[0];
  assert.ok(component);

  const probeOwned: MediaProbes = {
    resolves: (name) => name === "width-lte-768px",
    expansionMatches: () => false,
  };
  const digest = resolveMediaNames([collected], remTokens, probeOwned);
  assert.match(digest.names.get("(width <= 768px)")?.name ?? "", /^twm-media-[0-9a-f]{16}$/);

  const everything: MediaProbes = { resolves: () => true, expansionMatches: () => false };
  const exhausted = resolveMediaNames([collected], remTokens, everything);
  assert.equal(exhausted.names.size, 0);
  assert.ok(exhausted.fallbacks.has("(width <= 768px)"));

  const digestOwner: AuthoredMediaVariant = {
    name: `twm-media-${component.digest}`,
    definition: "@custom-variant x { &:hover { @slot; } }",
    mediaQueryKey: null,
    path: "app.css",
  };
  const collided = resolveMediaNames(
    [
      collection({
        components: [{ ...component, readableName: null }],
        authoredVariants: [digestOwner],
      }),
    ],
    remTokens,
    neverProbes,
  );
  assert.equal(collided.names.size, 0);
  assert.ok(collided.fallbacks.has("(width <= 768px)"));
});

test("reserved and theme names block readable generated names", () => {
  const collected = collect({
    stylesheets: [
      {
        cssPath: "card.css",
        cssSource:
          "@media (width <= 768px) { .card { margin: 0; } }\n@media (md) { .card { color: red; } }",
      },
    ],
    themeTokens: remTokens,
  });
  const resolution = resolveMediaNames([collected], remTokens, neverProbes, ["width-lte-768px"]);
  assert.match(resolution.names.get("(width <= 768px)")?.name ?? "", /^twm-media-/);
  // The boolean feature (md) would name itself md, which the breakpoint
  // namespace reserves.
  assert.match(resolution.names.get("(md)")?.name ?? "", /^twm-media-/);
});

test("cross-package components share one deduplicated resolution", () => {
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
  const resolution = resolveMediaNames([first, second], remTokens, neverProbes);
  assert.equal(resolution.names.size, 1);
  const entry = resolution.names.get("(width <= 768px)");
  assert.ok(entry);
  assert.equal(entry.cssPath, "a/card.css");

  const again = resolveMediaNames([first, second], remTokens, neverProbes);
  assert.deepEqual([...resolution.names.keys()], [...again.names.keys()]);
});

test("distinct keys never share a name even with colliding digests", () => {
  const shared = {
    whole: false,
    readableName: "width-gte-env",
    digest: "00000000deadbeef",
    builtin: null,
    breakpoint: null,
    cssPath: "card.css",
  };
  const components = [
    { ...shared, key: "(width >= env(A))", order: 1 },
    { ...shared, key: "(width >= env(B))", order: 2 },
    { ...shared, key: "(width >= env(C))", order: 3 },
  ];
  const resolution = resolveMediaNames([collection({ components })], {}, neverProbes);
  const names = [...resolution.names.values()].map((entry) => entry.name);
  assert.equal(new Set(names).size, names.length);
  assert.equal(names.length, 2);
  assert.equal(resolution.fallbacks.size, 1);
});
