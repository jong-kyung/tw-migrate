import { defineConfig } from "vite-plus";

// Test fixtures are inputs, not sources. `crates/snapshots/fixtures` holds
// deliberately malformed CSS/JS/HTML that exercises the migrator's error
// paths, and `ecosystem-ci/fixtures` holds apps whose post-migration bytes are
// pinned in `expected.json` — formatting either one rewrites the assertions.
const fixturePatterns = ["crates/snapshots/fixtures/**", "ecosystem-ci/fixtures/**"];

export default defineConfig({
  staged: {
    "*": "vp check --fix",
  },
  fmt: {
    ignorePatterns: fixturePatterns,
  },
  lint: {
    ignorePatterns: fixturePatterns,
    jsPlugins: [{ name: "vite-plus", specifier: "vite-plus/oxlint-plugin" }],
    rules: { "vite-plus/prefer-vite-plus-imports": "error" },
    options: { typeAware: true, typeCheck: true },
  },
});
