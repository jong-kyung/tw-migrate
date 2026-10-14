#!/usr/bin/env bash
set -euo pipefail

version="${1:?Pass the published version}"

# Retry the entire install because the root and platform packages can reach
# registry replicas at different times.
attempt=1
until [ "$attempt" -gt 5 ]; do
  install_dir="$(mktemp -d)"
  if (
    # Bash ignores errexit inside an if condition, so guard each setup command.
    cd "$install_dir" || exit 1
    npm init -y >/dev/null || exit 1
    npm install "tw-migrate@$version" tailwindcss@4.3.3 || exit 1

    # The root package must load the platform's optional dependency.
    if compgen -G 'node_modules/tw-migrate/*.node' > /dev/null; then
      echo "::error::The root package must not bundle native addons"
      exit 1
    fi

    cat > globals.css <<'EOF' || exit 1
@import "tailwindcss";
EOF
    cat > Button.module.css <<'EOF' || exit 1
.button { display: flex; }
EOF
    cat > Button.tsx <<'EOF' || exit 1
import "./globals.css";
import styles from "./Button.module.css";
export const Button = () => <button className={styles.button}>Hello</button>;
EOF
    cat > check.mjs <<'EOF' || exit 1
import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { migrate } from 'tw-migrate';

if (process.argv.includes('--written')) {
  assert.match(await readFile('Button.tsx', 'utf8'), /className="flex"/);
  assert.equal(existsSync('Button.module.css'), false);
} else {
  const paths = ['globals.css', 'Button.module.css', 'Button.tsx'];
  const before = await Promise.all(paths.map((path) => readFile(path, 'utf8')));
  const report = await migrate({ styleFile: 'Button.module.css', write: false });
  assert.equal(report.convertedRules, 1);
  assert.equal(report.retainedRules, 0);
  assert.deepEqual(report.failures, []);
  assert.deepEqual(report.warnings, []);
  assert.ok(report.candidates.includes('flex'));
  assert.deepEqual(await Promise.all(paths.map((path) => readFile(path, 'utf8'))), before);
}
EOF

    # --help exits before loading the native addon, so exercise real planning.
    node check.mjs \
      && npx --no-install tw-migrate Button.module.css --dry-run \
      && npx --yes "tw-migrate@$version" Button.module.css --dry-run \
      && pnpm dlx "tw-migrate@$version" Button.module.css --dry-run \
      && npx --no-install tw-migrate Button.module.css \
      && node check.mjs --written
  ); then
    echo "Installed and exercised tw-migrate@$version on attempt $attempt"
    exit 0
  fi
  echo "Attempt $attempt failed; the registry may still be propagating"
  attempt=$((attempt + 1))
  sleep 20
done
echo "::error::tw-migrate@$version did not install and migrate after 5 attempts"
exit 1
