import { defineConfig } from "vite-plus";

// Files whose exact bytes carry meaning, so the formatter must not own them:
//
// - `crates/snapshots/fixtures` is deliberately malformed CSS/JS/HTML that
//   exercises the migrator's error paths, so formatting it fails outright.
// - `ecosystem-ci/fixtures` holds apps whose post-migration bytes are pinned
//   in `expected.json`, so formatting it rewrites the assertions.
// - `.github` holds CI contracts whose literal layout is asserted by
//   `test/ecosystem-harness.test.ts` (reflowing a matrix array breaks it).
const pinnedPatterns = ["crates/snapshots/fixtures/**", "ecosystem-ci/fixtures/**", ".github/**"];

// `dist/` is the generated vp pack bundle; the formatter and linter own sources only.
const generatedPatterns = ["dist/**"];

export default defineConfig({
  pack: {
    entry: ["src/bin.ts", "src/index.ts"],
    fixedExtension: false,
    // The published declarations are the curated root index.d.ts until the
    // sources are TypeScript; dts generation takes over in that phase.
    dts: false,
  },
  staged: {
    "*": "vp check --fix",
  },
  fmt: {
    ignorePatterns: [...pinnedPatterns, ...generatedPatterns],
  },
  lint: {
    ignorePatterns: [...pinnedPatterns, ...generatedPatterns],
    jsPlugins: [{ name: "vite-plus", specifier: "vite-plus/oxlint-plugin" }],
    rules: { "vite-plus/prefer-vite-plus-imports": "error" },
    options: { typeAware: true, typeCheck: true },
  },
});
