import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import test from 'node:test';

import { migrate } from '../index.js';

const initialCss = '.button { padding: 13px; }\n';
const initialTsx = "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n";

async function fixture({
  css = initialCss,
  tsx = initialTsx,
  tailwind = '@import "tailwindcss";\n',
} = {}) {
  await mkdir('.tmp', { recursive: true });
  const cwd = await mkdtemp(join(process.cwd(), '.tmp', 'fixture-'));
  await Promise.all([
    writeFile(join(cwd, 'package.json'), '{"private":true}'),
    writeFile(join(cwd, 'globals.css'), tailwind),
    writeFile(join(cwd, 'Button.module.css'), css),
    writeFile(join(cwd, 'Button.tsx'), tsx),
  ]);
  return cwd;
}

async function cleanup(cwd) {
  await rm(cwd, { recursive: true, force: true });
}

test('updates global classes and ids while retaining global CSS', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      writeFile(join(cwd, 'legacy.css'), '.card { padding: 13px; }\n#hero { height: 100vh; }\n'),
      writeFile(join(cwd, 'Card.tsx'), 'export const Card = () => <main id="hero" className="card" />;\n'),
    ]);
    const report = await migrate({ cwd, cssFile: 'legacy.css' });
    assert.deepEqual(report.changedFiles, ['Card.tsx']);
    assert.deepEqual(report.candidates, ['h-[100vh]', 'p-[13px]']);
    assert.equal(report.convertedRules, 0);
    assert.equal(report.retainedRules, 2);
    assert.match(report.diff, /className="card p-\[13px\] h-\[100vh\]"/);
    assert.ok(report.warnings.every((warning) => warning.code === 'retained-global-rule'));
  } finally {
    await cleanup(cwd);
  }
});

test('converts a bounded breakpoint range to stacked variants', async () => {
  const cwd = await fixture({
    css: '@media (min-width: 48rem) and (max-width: 63.999rem) { .button { padding: 13px; } }\n',
  });
  try {
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, ['md:max-lg:p-[13px]']);
    assert.match(report.diff, /className="md:max-lg:p-\[13px\]"/);
  } finally {
    await cleanup(cwd);
  }
});

test('uses an exact project theme token before arbitrary fallback', async () => {
  const cwd = await fixture({
    tailwind: '@import "tailwindcss";\n@theme { --spacing-card: 13px; }\n',
  });
  try {
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, ['p-card']);
    assert.match(report.diff, /className="p-card"/);
  } finally {
    await cleanup(cwd);
  }
});

test('previews without writing, applies, and is idempotent', async () => {
  const cwd = await fixture();
  try {
    const preview = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(preview.changedFiles, ['Button.module.css', 'Button.tsx']);
    assert.deepEqual(preview.candidates, ['p-[13px]']);
    assert.equal(preview.convertedRules, 1);
    assert.equal(preview.retainedRules, 0);
    assert.match(preview.diff, /className="p-\[13px\]"/);
    assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), initialCss);
    assert.equal(await readFile(join(cwd, 'Button.tsx'), 'utf8'), initialTsx);

    const applied = await migrate({ cwd, cssFile: 'Button.module.css', write: true });
    assert.deepEqual(applied.changedFiles, preview.changedFiles);
    assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), '\n');
    assert.equal(
      await readFile(join(cwd, 'Button.tsx'), 'utf8'),
      'export const Button = () => <button className="p-[13px]">Save</button>;\n',
    );

    const secondRun = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(secondRun.changedFiles, []);
    assert.equal(secondRun.diff, '');
  } finally {
    await cleanup(cwd);
  }
});
