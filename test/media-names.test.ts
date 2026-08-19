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
  const dark = verified.get("(prefers-color-scheme: dark)");
  assert.ok(dark);
  assert.equal(dark.kind, "builtin");
  assert.equal(dark.name, "dark");
  assert.equal(verified.get("screen")?.kind, "generated");
  assert.equal(verified.get("screen")?.name, "screen");

  // A project that redefined dark with selector semantics fails the
  // expansion proof, so the condition receives its own component variant.
  const redefined: MediaProbes = {
    resolves: (name) => name === "dark",
    expansionMatches: () => false,
  };
  const generated = resolveMediaNames([collected], remTokens, redefined);
  const own = generated.get("(prefers-color-scheme: dark)");
  assert.ok(own);
  assert.equal(own.kind, "generated");
  assert.equal(own.name, "prefers-color-scheme-dark");
});

test("existing breakpoints are reused per component only with verified expansions", () => {
  const collected = collect({
    stylesheets: [
      {
        cssPath: "card.css",
        cssSource: "@media screen and (48rem <= width < 64rem) { .card { margin: 0; } }",
      },
    ],
    themeTokens: remTokens,
  });
  const proving: MediaProbes = {
    resolves: (name) => name === "md" || name === "max-lg",
    expansionMatches: (name, key) =>
      (name === "md" && key === "(width >= 48rem)") ||
      (name === "max-lg" && key === "(width < 64rem)"),
  };
  const resolution = resolveMediaNames([collected], remTokens, proving);
  assert.equal(resolution.get("(width >= 48rem)")?.kind, "breakpoint");
  assert.equal(resolution.get("(width >= 48rem)")?.name, "md");
  assert.equal(resolution.get("(width < 64rem)")?.kind, "breakpoint");
  assert.equal(resolution.get("(width < 64rem)")?.name, "max-lg");
  assert.equal(resolution.get("screen")?.kind, "generated");

  // A custom variant can shadow a breakpoint name while the theme token
  // keeps its value; the shadowed name is never reused.
  const shadowed: MediaProbes = {
    resolves: (name) => name === "md" || name === "max-lg",
    expansionMatches: () => false,
  };
  const generated = resolveMediaNames([collected], remTokens, shadowed);
  assert.equal(generated.get("(width >= 48rem)")?.kind, "generated");
  assert.equal(generated.get("(width >= 48rem)")?.name, "width-gte-48rem");
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
    mediaQueryKey: "(width <= 768px)",
  };
  const proving: MediaProbes = {
    resolves: (name) => name === "width-lte-768px",
    expansionMatches: (name, key) => name === "width-lte-768px" && key === "(width <= 768px)",
  };
  const adopted = resolveMediaNames(
    [collection({ components: collected.components, authoredVariants: [identical] })],
    remTokens,
    proving,
  );
  const entry = adopted.get("(width <= 768px)");
  assert.ok(entry);
  assert.equal(entry.kind, "adopted");
  assert.equal(entry.name, "width-lte-768px");

  // The same name with a different meaning is never touched; the component
  // falls to the digest name.
  const different: AuthoredMediaVariant = { ...identical, mediaQueryKey: "(width <= 900px)" };
  const blocked = resolveMediaNames(
    [collection({ components: collected.components, authoredVariants: [different] })],
    remTokens,
    proving,
  );
  const digest = blocked.get("(width <= 768px)");
  assert.ok(digest);
  assert.equal(digest.kind, "generated");
  assert.match(digest.name, /^twm-media-[0-9a-f]{16}$/);

  // A plugin can register the same name with different effective semantics
  // behind an identical-looking stylesheet definition; the failing
  // expansion proof blocks adoption and the component falls to the digest.
  const shadowed: MediaProbes = {
    resolves: (name) => name === "width-lte-768px",
    expansionMatches: () => false,
  };
  const unadopted = resolveMediaNames(
    [collection({ components: collected.components, authoredVariants: [identical] })],
    remTokens,
    shadowed,
  );
  const fallback = unadopted.get("(width <= 768px)");
  assert.ok(fallback);
  assert.equal(fallback.kind, "generated");
  assert.match(fallback.name, /^twm-media-[0-9a-f]{16}$/);
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
  assert.match(digest.get("(width <= 768px)")?.name ?? "", /^twm-media-[0-9a-f]{16}$/);

  const everything: MediaProbes = { resolves: () => true, expansionMatches: () => false };
  // An absent key keeps the arbitrary-variant fallback.
  const exhausted = resolveMediaNames([collected], remTokens, everything);
  assert.equal(exhausted.size, 0);

  const digestOwner: AuthoredMediaVariant = {
    name: `twm-media-${component.digest}`,
    mediaQueryKey: null,
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
  assert.equal(collided.size, 0);
});

test("verified identical definitions bypass usage reservations", () => {
  const collected = collect({
    stylesheets: [
      { cssPath: "card.css", cssSource: "@media (width <= 768px) { .card { margin: 0; } }" },
    ],
    themeTokens: remTokens,
  });
  const identical: AuthoredMediaVariant = {
    name: "width-lte-768px",
    mediaQueryKey: "(width <= 768px)",
  };
  const proving: MediaProbes = {
    resolves: (name) => name === "width-lte-768px",
    expansionMatches: (name, key) => name === "width-lte-768px" && key === "(width <= 768px)",
  };
  // A second run finds its own first-run name in the scanned corpus; the
  // corpus reservation must not block adopting the verified definition.
  const resolution = resolveMediaNames(
    [collection({ components: collected.components, authoredVariants: [identical] })],
    remTokens,
    proving,
    ["width-lte-768px"],
  );
  const entry = resolution.get("(width <= 768px)");
  assert.ok(entry);
  assert.equal(entry.kind, "adopted");
  assert.equal(entry.name, "width-lte-768px");
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
  assert.match(resolution.get("(width <= 768px)")?.name ?? "", /^twm-media-/);
  // The boolean feature (md) would name itself md, which the breakpoint
  // namespace reserves.
  assert.match(resolution.get("(md)")?.name ?? "", /^twm-media-/);
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
  assert.equal(resolution.size, 1);
  const entry = resolution.get("(width <= 768px)");
  assert.ok(entry);
  const again = resolveMediaNames([first, second], remTokens, neverProbes);
  assert.deepEqual([...resolution.keys()], [...again.keys()]);
});

test("distinct keys never share a name even with colliding digests", () => {
  const shared = {
    readableName: "width-gte-env",
    digest: "00000000deadbeef",
    builtin: null,
    breakpoint: null,
  };
  const components = [
    { ...shared, key: "(width >= env(A))" },
    { ...shared, key: "(width >= env(B))" },
    { ...shared, key: "(width >= env(C))" },
  ];
  const resolution = resolveMediaNames([collection({ components })], {}, neverProbes);
  const names = [...resolution.values()].map((entry) => entry.name);
  assert.equal(new Set(names).size, names.length);
  assert.equal(names.length, 2);
  assert.ok(!resolution.has("(width >= env(C))"));
});
