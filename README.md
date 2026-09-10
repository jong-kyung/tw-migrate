# tw-migrate

Migrate static React/Next.js, Vue 3 SFC, and HTML stylesheet usage to Tailwind CSS v4 utilities. Supports CSS, SCSS, Sass, Less, and CSS Modules.

Requires Node.js 22.23.2+ and [Tailwind CSS v4 configured in your project](https://tailwindcss.com/docs/installation/) with a CSS entry such as `@import "tailwindcss";`. For Sass or Less, install the compiler in the project you are migrating.

## Usage

Run from the package you want to migrate:

```bash
npx tw-migrate --dry-run                  # Preview changes
npx tw-migrate                            # Apply changes
npx tw-migrate path/to/Button.module.scss # Migrate one stylesheet
npx tw-migrate --workspaces               # Migrate all packages
npx tw-migrate --help                     # See all options
```

`pnpm dlx tw-migrate` and `yarn dlx tw-migrate` accept the same arguments.

**The CLI writes files by default.** Review the preview before applying. Unsupported or ambiguous rules stay in place with [warnings](https://github.com/jong-kyung/tw-migrate/blob/main/docs/warnings.md). Use `--tailwind-css path/to/globals.css` to select an entry when discovery is ambiguous.

## More

- [Supported behavior and design](https://github.com/jong-kyung/tw-migrate/tree/main/rfcs/)
- [Contributing and local development](https://github.com/jong-kyung/tw-migrate/blob/main/CONTRIBUTING.md)
