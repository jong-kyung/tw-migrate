# Packaged CLI snapshots

These tests build the release addon, pack the root and current-platform npm packages, install them in an OS temporary directory, and run the installed `tw-migrate` binary against copied fixtures. The suite performs one registry-backed npm install per test process, so it requires network access.

## Run

```bash
pnpm test:snapshots
```

The workspace `default-members` excludes this crate, so plain `cargo test` does not trigger packaging or network access. `cargo test --workspace` does include it and therefore requires artifacts from `pnpm snapshots:prepare`.

To iterate on one case:

```bash
pnpm snapshots:prepare
cargo test -p tw-migrate-snapshots safety_missing_sass
```

## Review changes

Install the pinned review CLI once:

```bash
cargo install cargo-insta --version 1.48.0 --locked
```

Then build the package and review snapshot differences:

```bash
pnpm snapshots:prepare
cargo insta test -p tw-migrate-snapshots --review
```

Use `cargo insta reject` to remove pending `.snap.new` files. Before committing, check for stale snapshots with:

```bash
cargo insta test --check --unreferenced reject -p tw-migrate-snapshots
```

Each fixture directory contains project files plus a strict `case.toml`. Each snapshot records the expected exit status, stdout, stderr, and immediate workspace delta for every step. Baselines are shared across Linux, macOS, and Windows; normalization is limited to line endings, path separators, known temporary roots, and transaction tokens. Do not normalize product-visible differences merely to make a platform pass.
