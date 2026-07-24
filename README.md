# tw-migrate

Preview and migrate static React/Next.js stylesheet references to Tailwind v4 utilities.

```bash
pnpm install
pnpm build:debug
node bin/tw-migrate.js                         # Preview the current package
node bin/tw-migrate.js --write                 # Migrate the current package
node bin/tw-migrate.js path/to/Button.module.scss
node bin/tw-migrate.js --workspaces --write    # Migrate every package
```

The CLI previews changes by default. Pass `--tailwind-css path/to/globals.css` when the current package has multiple Tailwind entries. `--force` skips package groups that fail discovery or input parsing; plan-integrity and write failures always stop the run.

## Current support

- `.css`, `.scss`, `.sass`, and `.less` stylesheets
- SCSS/Sass/Less values evaluated with the target project's installed compiler; ambiguous mixin and partial origins are retained
- `.js`, `.jsx`, `.ts`, and `.tsx` source files
- static `.html` literal `class`/`id` attributes scoped by local external stylesheet links (link-level `print` media supported; other link media conditions are retained)
- direct CSS Module members, static template literals, and static expression literals
- global `className` and `id` literals
- multi-compound CSS Module selectors whose element relationships are proven from the JSX graph
- common state pseudo-classes, global arbitrary descendant variants, and conditional at-rules (`@media`, `@supports`, `@container`, `@starting-style`)
- the tier-1 property mapping families with shorthand/longhand normalization
- exact Tailwind theme tokens and breakpoints with arbitrary-value fallback
- generated candidates are compiled against the project's Tailwind entry; failures retain the source rule
- batch migration of every stylesheet in a package, and `--workspaces` runs across packages
- CSS Module cleanup when every reference is safely migrated

Everything outside this subset is retained and reported with one of the warning codes below.

## Warning codes

| Code | Meaning |
| --- | --- |
| `aliased-css-module-reference` | A CSS Module class is aliased to a local binding, so the module is retained. |
| `batch-stylesheet-conflict` | Utilities generated from different stylesheets conflict on the same JSX element, so the contributing rule is retained. |
| `candidate-compilation-failure` | A generated candidate did not compile under the project's Tailwind entry, so its rule is retained. |
| `computed-css-module-reference` | A computed CSS Module access cannot be verified, so the module is retained. |
| `cross-package-stylesheet-link` | A linked stylesheet is owned by another package, so it is not analyzed outside workspace mode. |
| `css-module-composes` | The rule uses or is targeted by `composes`, so it is retained. |
| `dynamic-class-name` | A `className` value is dynamic, so the element cannot be migrated. |
| `dynamic-html-attribute` | An HTML attribute is not a safely writable quoted literal, so the element cannot be migrated. |
| `existing-tailwind-conflict` | A generated utility may conflict with a Tailwind class already on the element. |
| `inferred-preprocessor-source` | A linked CSS file was matched to a uniquely named preprocessor source file. |
| `module-utilities-conflict` | Utilities generated from different module classes on one element overlap, so their rules are retained. |
| `non-classname-css-module-reference` | A CSS Module class is used outside a supported `className`, so the module is retained. |
| `rebuild-required` | A preprocessor entry was migrated; rebuild it to refresh its generated CSS. |
| `reference-only-css-module-consumer` | A reference-only (non-writable) source uses the CSS Module, so it is retained. |
| `retained-global-rule` | Global CSS is never deleted automatically. |
| `shared-preprocessor-source` | A Sass partial must be analyzed through every consuming entry, so it is retained. |
| `unproven-css-module-relationship` | A compound selector's element relationship could not be proven for every usage. |
| `unproven-script-reference` | An inline script names a CSS Module class, so the module is retained. |
| `unproven-source-map` | A generated rule does not map uniquely to one authored source rule, so it is retained. |
| `unresolved-selector-target` | No exclusively supported `className` references were found for the rule. |
| `unsupported-animation` | The animation references keyframes that cannot be converted. |
| `unsupported-at-rule` | The rule contains or sits inside an at-rule outside the supported set. |
| `unsupported-container-query` | The `@container` condition has no Tailwind variant equivalent. |
| `unsupported-css-module-reference` | The CSS Module has an import or reference that cannot be migrated safely. |
| `unsupported-declaration` | A declaration is outside the supported property subset. |
| `unsupported-html-base` | A remote or unrepresentable base URL prevents safe stylesheet link resolution. |
| `unsupported-html-stylesheet-link` | Only local package stylesheet links are analyzed. |
| `unsupported-important` | `!important` declarations are not migrated. |
| `unsupported-link-media` | A stylesheet link or `@import` media condition cannot be represented safely. |
| `unsupported-media-query` | The `@media` condition has no Tailwind variant equivalent. |
| `unsupported-nested-at-rule` | A nested conditional at-rule could not be fully converted. |
| `unsupported-overlap` | Shorthand and longhand declarations overlap in a way that cannot be normalized. |
| `unsupported-rule-content` | The rule contains non-declaration content that cannot be converted. |
| `unsupported-selector` | The selector is outside the supported subset. |
| `unsupported-starting-style` | The `@starting-style` condition could not be converted. |
| `unsupported-supports-query` | The `@supports` condition has no Tailwind variant equivalent. |
| `unsupported-value` | A declaration value cannot be represented as a Tailwind utility. |

See the [core RFC](./rfcs/css-to-tailwind-migration-cli.md) and [preprocessor/HTML RFC](./rfcs/preprocessor-and-html-migration.md) for the complete design and remaining scope.

## Testing the packaged CLI

Run `pnpm test:snapshots` to build, pack, install, and test the published CLI shape against the cross-platform snapshot corpus. The command uses the npm registry and performs one shared install per test process. See [`crates/snapshots/README.md`](./crates/snapshots/README.md) for targeted runs and snapshot review commands.

## Testing browser ecosystem compatibility

Build the current-platform package and install Chromium before a focused browser run:

```bash
pnpm build
pnpm artifacts
pnpm exec playwright install chromium
pnpm test:ecosystem --case react-vite-css
```

Use `--case production-react-vite-css` for the installed CLI production-build smoke, or `--all` for all twelve controlled runtime/stylesheet cells. The default `pnpm test` and packaged snapshots remain browser-free.

Pinned external projects run only in the **Ecosystem browser** GitHub Actions workflow on `main`, manual dispatch, or a pull request carrying the `ecosystem` label. The workflow checks them out under the runner's temporary directory without credentials or secrets; there is intentionally no contributor-facing local external command.

On failure, each OS/case job uploads only its bounded phase ledger, computed-style captures, screenshots, migration output, source diff, and registry/install/build/server logs. See the [browser ecosystem RFC](./rfcs/browser-ecosystem-e2e.md) for immutable external evidence and the manifest, isolation, and oracle contracts.
