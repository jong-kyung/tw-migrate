import { defineConfig } from "vite-plus";

// Files whose exact bytes carry meaning, so the formatter must not own them:
//
// - `crates/snapshots/fixtures` is deliberately malformed CSS/JS/HTML that
//   exercises the migrator's error paths, so formatting it fails outright.
// - `ecosystem-ci/fixtures` holds apps whose post-migration bytes are pinned
//   in `expected.json`, so formatting it rewrites the assertions.
// - `.github` keeps pinned action references and embedded shell scripts out
//   of automatic formatting and lint fixes.
const pinnedPatterns = ["crates/snapshots/fixtures/**", "ecosystem-ci/fixtures/**", ".github/**"];

// `dist/` is the generated vp pack bundle; the formatter and linter own sources only.
const generatedPatterns = ["dist/**"];

export default defineConfig({
  test: {
    // Full migrations replan with candidate canonicalization, whose first
    // design-system lookup builds Tailwind's utility index; slower CI
    // runners exceed the 5s default by a wide margin.
    testTimeout: 60000,
  },
  pack: {
    entry: ["src/bin.ts", "src/index.ts"],
    fixedExtension: false,
    dts: true,
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
