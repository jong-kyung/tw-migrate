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
- direct CSS Module members and static template literals
- global `className` and `id` literals
- common state pseudo-classes
- global arbitrary descendant variants
- exact Tailwind theme tokens with arbitrary-value fallback
- spacing shorthand normalization
- exact Tailwind breakpoints
- CSS Module cleanup when every reference is safely migrated

Dynamic class builders, unproven CSS Module relationships, unsupported at-rules, `!important`, and `composes` dependencies are retained with warnings.

See [the RFC](./rfcs/css-to-tailwind-migration-cli.md) for the complete design and remaining scope.
