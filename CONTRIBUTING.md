# Contributing

## Setup

Install [Vite+](https://viteplus.dev/guide/), Git, npm, Rust, and your platform's native build tools. Use the Node version in [`.node-version`](.node-version), the Rust toolchain in [`rust-toolchain.toml`](rust-toolchain.toml), and the package manager pinned in [`package.json`](package.json). Vite+ resolves the pinned package manager for you.

```bash
vp install --frozen-lockfile
vp run build:debug
node src/bin.ts --help
```

The CLI and Node tests need the native addon from `build:debug`. Setup requires registry access for uncached dependencies; no `.env` or local service is required.

To try the CLI on another project, run the built entrypoint from that project's directory:

```bash
cd /path/to/your-app
node /path/to/tw-migrate/src/bin.ts --dry-run
```

Use `node <script> --help` directly because `vp node` consumes `--help` itself. Use `vp run <name>` for package scripts; built-in commands such as `vp build` and `vp test` are separate.

## Validation

Start with the focused check for your change, then run the owning suite.

| Change                  | Check                                                    |
| ----------------------- | -------------------------------------------------------- |
| Documentation           | `git diff --check` and verify links and commands         |
| Rust logic              | `cargo test` or `cargo test <filter>`                    |
| TypeScript/API          | `vp run build:debug`, then `vp test` or `vp test <file>` |
| CLI output or writes    | `vp run test:snapshots`                                  |
| Packaging               | `vp run build && vp run artifacts`                       |
| Formatting, lint, types | `vp check`                                               |

Full browser-free validation:

```bash
vp check
vp run test
vp run test:snapshots
```

`vp run test` runs Rust tests, builds the debug addon, and runs Node tests. Plain `cargo test` excludes packaged snapshots; `cargo test --workspace` includes them and needs release artifacts and registry access.

### Packaged CLI snapshots

Read [`crates/snapshots/README.md`](crates/snapshots/README.md) before changing fixtures or reviewing snapshots. The suite packs and installs the root and native packages outside the repository, with one registry-backed npm install per process.

```bash
vp run snapshots:prepare
cargo test -p tw-migrate-snapshots safety_missing_sass
```

Install `cargo-insta` with `cargo install cargo-insta --version 1.48.0 --locked` when reviewing snapshots. Check exit status and file changes before accepting a snapshot, and keep the shared cross-platform normalization narrow.

### Browser compatibility

```bash
vp run build
vp run artifacts
vp exec playwright install chromium
vp run test:ecosystem --case react-vite-css
```

Use `--case production-react-vite-css` for the installed CLI production-build smoke, or `--all` for all controlled cases. External project cases run only in CI. The default test and snapshot commands do not run browsers.

The [CI workflow](.github/workflows/ci.yml) and [Ecosystem browser workflow](.github/workflows/ecosystem.yml) use reduced routine coverage, with full weekly and manual runs. Adding the `test:e2e` PR label triggers browser coverage; pushing to an already-labeled PR does not retrigger it. Release publication requires full CI and ecosystem validation. See the [browser ecosystem RFC](rfcs/browser-ecosystem-e2e.md) for the matrix, assertions, and failure artifacts.

## Changes and regression tests

- Keep each change focused and include a reproducer for bugs.
- Put Rust parser/planner tests beside their implementation, API and byte-preservation assertions in `test/`, and CLI status/output/write cases in packaged snapshots.
- Update CLI fixtures and snapshots together. Preserve exact reports, source bytes, causal witnesses, and browser probes when changing the ecosystem harness.
- Document new warning codes in [`docs/warnings.md`](docs/warnings.md) and update `WARNING_CODES` in `crates/tw_migrate_planner/src/lib.rs`; its test checks the list against emitted codes.
- Leave generated `dist/`, native `.node` files, temporary workspaces, and logs out of commits.

For code ownership and migration safety invariants, see [`AGENTS.md`](AGENTS.md). Design decisions and supported scope live in [`rfcs/`](rfcs/).

## Toolchain maintenance

When upgrading `vite-plus`, run `vp migrate` to realign its bundled Vitest catalog pin. Use `vp toolchain` for tool versions, `vp why <package>` for dependency paths, and `vp env doctor` for setup problems. Native package staging uses `vp run artifacts`; `vp run snapshots:prepare` removes stale platform addons before rebuilding.
