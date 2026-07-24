# RFC: Browser Ecosystem End-to-End Validation

## Status

Proposed

## Summary

`tw-migrate` currently verifies planner output, source edits, CLI behavior, and transactional writes without proving that a migrated application renders the same result in a browser. This RFC adds an opt-in Vitest Browser Mode suite that compares migration results in Chromium before and after migration.

The suite has two permanent coverage layers:

1. Twelve repository-owned controlled fixtures cover the Cartesian product of React+Vite, Next.js, and Vite+HTML with CSS, SCSS, indented Sass, and Less.
2. Three to five pinned public projects cover installation and runtime compatibility in existing Tailwind CSS v4 applications.

External projects supplement the controlled fixtures. They do not replace them. Public projects cannot reliably provide every runtime and stylesheet combination, stable source expectations, or long-lived interaction probes. Controlled fixtures provide that deterministic contract; external projects detect integration assumptions that the fixtures do not model.

External repositories are checked out only by the dedicated GitHub Actions workflow. The repository does not vendor them or require contributors to maintain local sibling clones.

## Background

The existing test layers answer different questions:

- Rust tests verify parsing, selector analysis, and utility planning.
- Node tests verify the public API, source maps, and byte-exact rewrites.
- Packaged CLI snapshots verify installation, output, exit status, and filesystem behavior.

None of these layers starts a real application toolchain and asks Chromium whether the migrated utilities preserve the computed result. A candidate can compile and a source rewrite can look correct while Vite, Next.js, PostCSS, a preprocessor, or Tailwind produces different browser styles.

An external-only matrix was considered and rejected. Public Tailwind v4 projects do not provide a dependable twelve-cell set, particularly for `.module.sass` and Next.js Less. Their source, scripts, routes, and selectors also change independently of this repository. Filling the gaps by patching those projects would turn them into less deterministic controlled fixtures with a network dependency.

## Goals

1. Prove that migrated utilities preserve standard computed styles in Chromium.
2. Cover React+Vite, Next.js, and Vite+HTML across CSS, SCSS, Sass, and Less.
3. Exercise CSS Modules in React and Next.js and global stylesheets in static HTML.
4. Verify exact migration reports and source bytes for controlled cases.
5. Verify second-run idempotency for every case.
6. Run against packed root and native packages rather than checkout entrypoints or addons.
7. Exercise a small set of unmodified public Tailwind v4 projects at immutable commits.
8. Produce bounded, allowlisted diagnostics for CI failures.
9. Keep the default Rust, Node, and packaged snapshot suites independent of browsers and external clones.

## Non-Goals

1. Replacing planner, Node, or packaged CLI tests.
2. Using screenshots as the pass/fail oracle.
3. Testing Firefox or WebKit before Chromium coverage proves stable.
4. Comparing Tailwind or application custom properties.
5. Exhaustively testing animation timelines or pseudo-elements.
6. Adding Tailwind, routes, selectors, or migration targets to external projects.
7. Automatically updating external project pins.
8. Running external repository code with secrets, write permissions, or persisted checkout credentials.
9. Making the browser suite part of the default `pnpm test` command.

## Coverage Model

### Controlled fixtures

The controlled corpus owns combination completeness:

| Runtime | CSS | SCSS | Sass | Less |
| --- | --- | --- | --- | --- |
| React+Vite | required | required | required | required |
| Next.js | required | required | required | required |
| Vite+HTML | required | required | required | required |

React+Vite and Next.js use CSS Modules. Vite+HTML uses a linked global stylesheet. Each fixture is a small runnable application with exact dependency versions and migration expectations.

Every controlled case declares independent probes for:

- base state;
- hover;
- focus;
- focus-visible;
- below the target breakpoint;
- above the target breakpoint.

Each probe uses a stable role, accessible name, text, data attribute, or id selector. Class selectors are prohibited because migration changes classes. A stable `data-identity` sequence proves that baseline and post-migration captures inspect the same elements in the same order.

Controlled cases also assert:

- the exact first `MigrationReport`;
- expected candidate tokens and warning output;
- exact bytes for every migration-owned changed file;
- an empty second-run diff and `changedFiles` list;
- an unchanged source-scoped tree after the second run.

These fixtures remain after external coverage is added. Removing them would discard the only deterministic proof of the twelve runtime/style cells and the exact rewrite contract.

### External ecosystem projects

The external corpus owns real-project compatibility rather than matrix completeness. It contains three to five public, non-archived, non-fork repositories pinned by full commit SHA.

A project qualifies only when it already has:

- Tailwind CSS v4;
- a committed npm, pnpm, or Yarn lockfile;
- a portable install and start path;
- a route that does not require authentication or an external API;
- a stable non-class selector;
- an existing stylesheet rule that `tw-migrate` converts;
- reproducible execution on Ubuntu, macOS, and Windows.

External cases keep their original source, package manager, lockfile, and scripts. The harness does not inject routes or probes. Each case verifies the declared migration target, browser equality, and source-scoped idempotency. It does not duplicate the controlled corpus's exhaustive report and byte expectations.

External repositories are checked out only in CI under the runner's temporary directory. The workflow uses a pinned repository and commit from typed manifest data, verifies `HEAD`, and discards the checkout with the ephemeral runner. Local controlled runs never depend on `../vite-plus`, another sibling checkout, or a previously cloned project.

Network, checkout, install, or upstream script failures fail the external case. The workflow does not silently skip them.

### Production CLI smoke

One React+Vite CSS fixture also exercises the installed CLI and production build boundary. It captures a clean production baseline, runs package-wide migration through the installed binary, verifies a second CLI no-op, deletes generated output, rebuilds, and compares the production preview.

Targeted controlled cases continue to use the installed public `migrate()` API because its structured report supports exact assertions.

## Browser Oracle

### Dynamic baseline

Each case captures its baseline immediately before migration on the same OS and Chromium installation used for the post-migration capture. The suite does not commit computed-style golden files. This avoids treating platform-specific browser serialization as a product regression.

### Computed-style comparison

For every matched element, the harness reads every property exposed by `getComputedStyle()`, sorts property names, and compares exact values. Properties beginning with `--` are excluded because Tailwind uses internal custom properties whose representation can change while the resulting standard properties remain equal.

The exclusion does not permit a no-op migration to pass. Controlled cases pair the browser comparison with exact candidates, reports, and source bytes.

### Causal witness

Before migration, the harness temporarily withholds the target stylesheet and captures every probe. Each probe must differ from its baseline in at least one standard computed-style property. This proves that the selected stylesheet contributes to every asserted state.

After migration, the harness also captures the app with the migrated legacy stylesheet withheld. That utilities-only capture must equal the baseline. Retained legacy CSS therefore cannot hide missing or incorrect generated utilities.

### Capture stability

Each probe runs in a fresh detached Playwright page. The harness:

1. sets the viewport before navigation;
2. navigates to the declared route;
3. waits for the declared readiness selector and fonts;
4. disables transitions and animations;
5. performs one trusted Playwright interaction when declared;
6. waits for layout to settle;
7. checks cardinality and identity;
8. records standard computed styles.

Only capture operations may retry. They receive one initial attempt and at most three retries, each with a fresh page. Migration, source, report, idempotency, and semantic comparison failures fail immediately.

## Package Isolation

The suite tests the publication boundary rather than the checkout boundary.

For each operating system, one package job:

1. builds the native addon;
2. packs the root package and current-platform native package;
3. records package names, versions, commit, platform, addon digest, and tarball digests in a provenance manifest;
4. uploads those exact artifacts for the case jobs on that OS.

A case job publishes the artifacts to a fresh Verdaccio registry, installs a temporary driver with `--ignore-scripts`, and verifies the resolved root package, native package, and addon against provenance. It rejects checkout paths, workspace links, wrong-platform addons, stale artifacts, and digest mismatches.

Verdaccio may proxy normal application dependencies to npm. It must not fetch `tw-migrate` or `tw-migrate-*` packages from upstream. Publishing is enabled only during bootstrap; the install phase uses sealed storage. The registry stops before application code starts.

## Case Lifecycle

A targeted case follows this order:

1. Validate the manifest before executing project data.
2. Install and verify packed packages.
3. Start the baseline server and capture every probe.
4. Stop the server.
5. Capture the stylesheet-withheld causal witness.
6. Restore the source and run the first migration.
7. Verify reports, candidates, changed files, and expected bytes.
8. Snapshot migration-owned sources and run migration again.
9. Verify the second report and source tree are unchanged.
10. Clear generated output and framework caches.
11. Capture the utilities-only result with migrated legacy CSS withheld.
12. Restore migrated source, start a fresh server, and capture post-migration probes.
13. Compare baseline and post-migration cardinality, identity, and computed styles.
14. Stop all processes and delete temporary successful-run data.

Every child process has a bounded timeout and bounded process-tree termination. A teardown failure fails an otherwise successful case and does not replace an earlier primary failure.

## Manifest Contract

`ecosystem-ci/projects.json` is typed data, not executable configuration. Controlled, production-smoke, and external entries use separate schemas.

Validation rejects:

- unknown fields or duplicate IDs;
- duplicate controlled runtime/style cells;
- class-based selectors;
- missing readiness, cardinality, source, or probe declarations;
- non-full external commit SHAs;
- local, SSH, or non-HTTPS external repository URLs;
- command strings, shell fragments, custom environment variables, and eval predicates;
- absolute paths, traversal, and symlinks escaping the case root;
- package-manager and lockfile mismatches.

External commands come from reviewed identifiers and argument arrays. Manifest values never become shell programs, runner labels, action names, artifact roots, or workflow expressions.

## CI and Contributor Workflow

The browser suite runs in a separate GitHub Actions workflow on:

- a designated pull-request label;
- pushes to `main`;
- manual dispatch.

It does not run on a cron schedule. The workflow uses GitHub-hosted Ubuntu, macOS, and Windows runners with Chromium. Package jobs run once per OS; case jobs run independently for each OS and case with `fail-fast: false`.

The workflow uses `pull_request`, never `pull_request_target`. Permissions are limited to `contents: read`, checkout credentials are not persisted, and external child processes receive no secrets, OIDC credentials, cloud credentials, or repository tokens.

Contributors can run one controlled case with:

```bash
pnpm test:ecosystem --case react-vite-css
```

Running the complete controlled corpus requires an explicit `--all`. External cases are CI-only because they execute pinned third-party source and require network cloning. Contributors use manual workflow dispatch when they need to reproduce an external case in the supported environment.

## Failure Diagnostics

Each case receives a workflow-owned artifact root and writes a phase ledger before setup begins. On failure, the suite may retain only explicitly declared regular files under that root:

- phase ledger;
- computed-style JSON;
- migration reports or CLI output;
- source diff;
- per-attempt screenshots;
- browser console and page errors;
- registry, install, build, and server logs.

Upload preparation rejects undeclared files, symlinks, paths outside the root, and oversized artifacts. Raw external child output is written to inert files; workflow logs contain harness-owned summaries.

## Version Contract

The harness pins compatible versions rather than inheriting application toolchain drift:

- Vitest and `@vitest/browser-playwright`: 4.1.10;
- Playwright: 1.61.1;
- Verdaccio: 6.8.0;
- Tailwind CSS integrations: 4.3.3;
- Vite: 8.1.5;
- Next.js: 15.5.21;
- React and ReactDOM: 19.2.8;
- `next-with-less`: 3.0.1.

External projects retain their own pinned dependencies.

## Delivery

### PR 1: Harness and runtime proof

The first PR provides:

- strict manifest and case selection;
- package staging, provenance, and sealed registry installation;
- browser lifecycle and computed-style oracle;
- React+Vite, Next.js, and Vite+HTML plain CSS fixtures;
- the initial three-OS workflow.

This slice proves the shared architecture with one stylesheet language across all runtimes.

### PR 2: Matrix and ecosystem completion

The second PR adds:

- the remaining nine SCSS, Sass, and Less controlled cells;
- the production CLI smoke;
- three to five admitted external projects with CI-only checkout;
- final contributor and diagnostics documentation.

External projects do not retire or reduce the controlled matrix.

## Testing Strategy

Harness unit tests cover:

- strict manifest validation and case selection;
- complete controlled inventory;
- package provenance and installed layout checks;
- source-scoped idempotency;
- retry boundaries and process teardown;
- artifact allowlisting;
- workflow-to-manifest matrix consistency.

Focused browser tests cover one case at a time through the installed package boundary. The full workflow covers every admitted case on all three operating systems.

Existing commands retain their current scope:

- `pnpm test` runs Rust and Node regression tests without a browser or clone;
- `pnpm test:snapshots` runs packaged CLI snapshots;
- `pnpm test:ecosystem --case <id>` runs one controlled browser case;
- the dedicated workflow owns external project checkout and execution.

## Success Criteria

1. All twelve controlled runtime/style cells pass exact source, report, idempotency, and browser checks on Ubuntu, macOS, and Windows.
2. Every controlled probe proves a stylesheet-dependent baseline and utilities-only equivalence.
3. The production CLI smoke passes clean pre/post builds through packed packages.
4. Three to five unmodified external projects pass at reviewed full SHAs on all three operating systems.
5. Package provenance proves that no case loaded the checkout entrypoint, checkout addon, workspace link, or upstream product package.
6. External code runs only in ephemeral, read-only, no-secret CI jobs.
7. Failures retain enough bounded evidence to identify the reached phase without uploading arbitrary workspace contents.
8. Default tests and packaged snapshots require no Chromium, Verdaccio, or external repository checkout.

## Accepted Trade-offs

1. Controlled fixtures cost maintenance but provide deterministic Cartesian coverage that public projects cannot supply.
2. External pins may fail because an upstream dependency or registry is unavailable; visible failure is preferred to silent skipping.
3. Chromium-only coverage does not prove browser-engine equivalence. Firefox and WebKit remain follow-up work until Chromium coverage is stable.
4. Exact standard computed-style comparison may expose OS serialization differences. Runtime-local baselines reduce this risk without weakening equality on one runner.
5. Excluding custom properties can miss a custom-property-only regression, so controlled source and report assertions remain mandatory.
6. OS-by-case jobs cost more than grouped jobs but isolate native-package, project, and diagnostic failures.

## Deferred Work

- Firefox and WebKit coverage.
- Automated external pin updates.
- Remote dependency caches or measured sharding.
- Shared package-staging code with the packaged snapshot runner.
- Screenshot-based visual regression coverage for differences that computed styles cannot represent.
