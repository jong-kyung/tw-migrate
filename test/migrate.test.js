import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import test from 'node:test';

import { __unstable__loadDesignSystem as loadDesignSystem } from 'tailwindcss';

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

test('converts nested media and supports rules to stacked variants', async () => {
  const cwd = await fixture({
    css: '@media (min-width: 48rem) { .button { padding: 1rem; } @supports (display: grid) { .button { display: grid; } } }\n',
  });
  try {
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, ['md:p-4', 'md:supports-[display:grid]:grid']);
    assert.deepEqual(report.changedFiles, ['Button.module.css', 'Button.tsx']);
  } finally {
    await cleanup(cwd);
  }
});

test('converts Tailwind conditional variants and moves global definitions', async () => {
  const cwd = await fixture({
    css: '@property --progress { syntax: "<number>"; inherits: false; initial-value: 0; }\n@media (prefers-reduced-motion: reduce) { @starting-style { @container (min-width: 28rem) { .button { display: grid; } } } }\n@media (prefers-color-scheme: dark) { .button { color: white; } }\n',
  });
  try {
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, [
      'dark:text-[white]',
      'motion-reduce:starting:@md:grid',
    ]);
    assert.deepEqual(report.changedFiles, ['Button.module.css', 'Button.tsx', 'globals.css']);
    assert.match(report.diff, /@property --progress/);
  } finally {
    await cleanup(cwd);
  }
});

test('converts conditions nested inside style rules', async () => {
  const cwd = await fixture({
    css: '.button { opacity: 1; @starting-style { opacity: 0; } @media (prefers-reduced-motion: reduce) { display: none; } }\n',
  });
  try {
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, [
      '[opacity:1]',
      'motion-reduce:hidden',
      'starting:[opacity:0]',
    ]);
    assert.equal(report.convertedRules, 1);
  } finally {
    await cleanup(cwd);
  }
});

test('converts compound media and named container queries to arbitrary variants', async () => {
  const cwd = await fixture({
    css: '@media screen and (min-width: 40rem) and (orientation: landscape) { .button { display: grid; } }\n@container card_grid (min-width: 20rem) and (max-width: 40rem) { .button { color: red; } }\n',
  });
  try {
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, [
      '[@container_card\\_grid_(min-width:20rem)_and_(max-width:40rem)]:text-[red]',
      '[@media_screen_and_(min-width:40rem)_and_(orientation:landscape)]:grid',
    ]);
    assert.equal(report.convertedRules, 2);
  } finally {
    await cleanup(cwd);
  }
});

test('warns when a generated utility conflicts with a static template class', async () => {
  const cwd = await fixture({
    tsx: "import styles from './Button.module.css';\nexport const Button = () => <button className={`${styles.button} p-2`}>Save</button>;\n",
  });
  try {
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.equal(report.warnings[0].code, 'existing-tailwind-conflict');
    assert.match(report.diff, /className="p-\[13px\] p-2"/);
  } finally {
    await cleanup(cwd);
  }
});

test('accepts candidates already emitted by the Tailwind entry', async () => {
  const cwd = await fixture({
    tailwind: '@import "tailwindcss";\n@source inline("p-[13px]");\n',
  });
  try {
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, ['p-[13px]']);
    assert.equal(report.convertedRules, 1);
  } finally {
    await cleanup(cwd);
  }
});

test('loads Tailwind config and plugin modules', async () => {
  const cwd = await fixture({
    tailwind: '@import "tailwindcss";\n@config "./tailwind.config.js";\n@plugin "./tailwind-plugin.js";\n',
  });
  try {
    await Promise.all([
      writeFile(join(cwd, 'tailwind.config.js'), 'module.exports = {};\n'),
      writeFile(
        join(cwd, 'tailwind-plugin.js'),
        'module.exports = function ({ addUtilities }) { addUtilities({ ".plugin-test": { display: "block" } }); };\n',
      ),
    ]);
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, ['p-[13px]']);
  } finally {
    await cleanup(cwd);
  }
});

test('moves local keyframes to Tailwind before deleting a CSS Module', async () => {
  const cwd = await fixture({
    css: '@keyframes fade { from { opacity: 0; } to { opacity: 1; } }\n.button { animation: fade 1s; }\n',
  });
  try {
    const preview = await migrate({ cwd, cssFile: 'Button.module.css' });
    const match = /^\[animation:(tw-migrate-[a-f0-9]+-fade)_1s\]$/.exec(preview.candidates[0]);
    assert.ok(match);
    assert.deepEqual(preview.changedFiles, ['Button.module.css', 'Button.tsx', 'globals.css']);
    assert.match(preview.diff, new RegExp(`@keyframes ${match[1]}`));

    await migrate({ cwd, cssFile: 'Button.module.css', write: true });
    await assert.rejects(readFile(join(cwd, 'Button.module.css'), 'utf8'), { code: 'ENOENT' });
    assert.match(await readFile(join(cwd, 'Button.tsx'), 'utf8'), new RegExp(match[1]));
    assert.match(await readFile(join(cwd, 'globals.css'), 'utf8'), new RegExp(`@keyframes ${match[1]}`));
  } finally {
    await cleanup(cwd);
  }
});

test('escapes literal underscores in arbitrary values', async () => {
  const cwd = await fixture({
    css: '.button { --font-key: Open_Sans; }\n',
  });
  try {
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, ['[--font-key:Open\\_Sans]']);
    assert.ok(report.diff.includes('Open\\\\_Sans'));

    const designSystem = await loadDesignSystem('@tailwind utilities;');
    const [css] = designSystem.candidatesToCss(report.candidates);
    assert.match(css, /Open_Sans/);
    assert.doesNotMatch(css, /Open Sans/);
  } finally {
    await cleanup(cwd);
  }
});

test('preserves functional values in spacing shorthands', async () => {
  const cwd = await fixture({
    css: '.button { margin: calc(100% - 1rem); padding: var(--space, 1rem); }\n',
  });
  try {
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, [
      'm-[calc(100%_-_1rem)]',
      'p-[var(--space,_1rem)]',
    ]);
    assert.equal(report.convertedRules, 1);
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

test('scans mjs and mts references before deleting a CSS Module', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      writeFile(
        join(cwd, 'helper.mjs'),
        "import styles from './Button.module.css';\nexport const buttonClass = styles.button;\n",
      ),
      writeFile(
        join(cwd, 'helper.mts'),
        "import styles from './Button.module.css';\nexport const buttonClass = styles.button;\n",
      ),
    ]);

    const report = await migrate({ cwd, cssFile: 'Button.module.css', write: true });
    assert.equal(report.convertedRules, 0);
    assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), initialCss);
    assert.match(await readFile(join(cwd, 'helper.mjs'), 'utf8'), /Button\.module\.css/);
    assert.match(await readFile(join(cwd, 'helper.mts'), 'utf8'), /Button\.module\.css/);
  } finally {
    await cleanup(cwd);
  }
});

test('previews and applies a complete CSS Module migration', async () => {
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
    await assert.rejects(readFile(join(cwd, 'Button.module.css'), 'utf8'), { code: 'ENOENT' });
    assert.equal(
      await readFile(join(cwd, 'Button.tsx'), 'utf8'),
      'export const Button = () => <button className="p-[13px]">Save</button>;\n',
    );
  } finally {
    await cleanup(cwd);
  }
});
