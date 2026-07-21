import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { chmod, mkdtemp, mkdir, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import test from 'node:test';
import { promisify } from 'node:util';

import { __unstable__loadDesignSystem as loadDesignSystem } from 'tailwindcss';

import { migrate } from '../index.js';

const run = promisify(execFile);
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

test('migrates a global selector whose class name recurs in a pseudo-class argument', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      writeFile(join(cwd, 'legacy.css'), '.a:not(.abc) { padding: 13px; }\n'),
      writeFile(join(cwd, 'Card.tsx'), 'export const Card = () => <div className="a" />;\n'),
    ]);
    const report = await migrate({ cwd, cssFile: 'legacy.css' });
    assert.deepEqual(report.candidates, ['[&:not(.abc)]:p-[13px]']);
    assert.match(report.diff, /className="a \[&:not\(\.abc\)\]:p-\[13px\]"/);
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

test('preserves the Tailwind utility prefix before variants', async () => {
  const cwd = await fixture({
    css: '@media (min-width: 48rem) { .button { padding: 13px; } }\n',
    tailwind: '@import "tailwindcss" prefix(tw);\n',
  });
  try {
    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, ['tw:md:p-[13px]']);
    assert.match(report.diff, /className="tw:md:p-\[13px\]"/);
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

test('retains a CSS Module composed by another stylesheet', async () => {
  const cwd = await fixture();
  try {
    await writeFile(
      join(cwd, 'Consumer.module.css'),
      ".fancyButton {\n  composes: button from './Button.module.css';\n  border: 1px solid;\n}\n",
    );

    const report = await migrate({ cwd, cssFile: 'Button.module.css', write: true });
    assert.equal(report.convertedRules, 0);
    assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), initialCss);
    assert.ok(report.warnings.some((warning) => warning.code === 'unsupported-css-module-reference'));
  } finally {
    await cleanup(cwd);
  }
});

test('retains a CSS Module imported via url() by another stylesheet', async () => {
  const cwd = await fixture();
  try {
    await writeFile(join(cwd, 'legacy.css'), '@import url(./Button.module.css);\n.page { color: red; }\n');

    const report = await migrate({ cwd, cssFile: 'Button.module.css', write: true });
    assert.equal(report.convertedRules, 0);
    assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), initialCss);
    assert.ok(report.warnings.some((warning) => warning.code === 'unsupported-css-module-reference'));
  } finally {
    await cleanup(cwd);
  }
});

test('ignores commented Tailwind imports when detecting the entry', async () => {
  const cwd = await fixture();
  try {
    await writeFile(
      join(cwd, 'notes.css'),
      '/* setup example: @import "tailwindcss"; */\n.note { color: red; }\n',
    );

    const report = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(report.candidates, ['p-[13px]']);
  } finally {
    await cleanup(cwd);
  }
});

test('scans cjs and cts references before deleting a CSS Module', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      writeFile(
        join(cwd, 'helper.cjs'),
        "const styles = require('./Button.module.css');\nmodule.exports = styles.button;\n",
      ),
      writeFile(
        join(cwd, 'helper.cts'),
        "import styles from './Button.module.css';\nexport const buttonClass = styles.button;\n",
      ),
    ]);

    const report = await migrate({ cwd, cssFile: 'Button.module.css', write: true });
    assert.equal(report.convertedRules, 0);
    assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), initialCss);
    assert.match(await readFile(join(cwd, 'helper.cjs'), 'utf8'), /Button\.module\.css/);
    assert.match(await readFile(join(cwd, 'helper.cts'), 'utf8'), /Button\.module\.css/);
  } finally {
    await cleanup(cwd);
  }
});

test(
  'preserves source file permissions when writing changes',
  { skip: process.platform === 'win32' },
  async () => {
    const cwd = await fixture();
    try {
      const sourcePath = join(cwd, 'Button.tsx');
      await chmod(sourcePath, 0o751);

      await migrate({ cwd, cssFile: 'Button.module.css', write: true });

      assert.equal((await stat(sourcePath)).mode & 0o777, 0o751);
    } finally {
      await cleanup(cwd);
    }
  },
);

test('a second run after applying a global migration is a no-op', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      writeFile(join(cwd, 'legacy.css'), '.card { padding: 13px; }\n#hero { height: 100vh; }\n'),
      writeFile(join(cwd, 'Card.tsx'), 'export const Card = () => <main id="hero" className="card" />;\n'),
    ]);
    const first = await migrate({ cwd, cssFile: 'legacy.css', write: true });
    assert.deepEqual(first.changedFiles, ['Card.tsx']);

    const second = await migrate({ cwd, cssFile: 'legacy.css' });
    assert.deepEqual(second.changedFiles, []);
    assert.equal(second.diff, '');
  } finally {
    await cleanup(cwd);
  }
});

test('a second run after a partial module migration is a no-op', async () => {
  const cwd = await fixture({
    css: '.button { padding: 13px; }\n.other { display: grid; }\n',
  });
  try {
    const first = await migrate({ cwd, cssFile: 'Button.module.css', write: true });
    assert.deepEqual(first.changedFiles, ['Button.module.css', 'Button.tsx']);

    const second = await migrate({ cwd, cssFile: 'Button.module.css' });
    assert.deepEqual(second.changedFiles, []);
    assert.equal(second.diff, '');
    assert.match(await readFile(join(cwd, 'Button.module.css'), 'utf8'), /\.other/);
  } finally {
    await cleanup(cwd);
  }
});

test('fails fast when leftover files from an interrupted run exist', async () => {
  const cwd = await fixture();
  try {
    await writeFile(join(cwd, '.Button.tsx.tw-migrate-backup-123-0'), 'old content');
    await assert.rejects(migrate({ cwd, cssFile: 'Button.module.css' }), /interrupted run/);
  } finally {
    await cleanup(cwd);
  }
});

test(
  'restores originals when a write fails partway',
  { skip: process.platform === 'win32' },
  async () => {
    const cwd = await fixture();
    try {
      const componentTsx =
        "import styles from '../Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n";
      await mkdir(join(cwd, 'components'));
      await Promise.all([
        rm(join(cwd, 'Button.tsx')),
        writeFile(join(cwd, 'components', 'Button.tsx'), componentTsx),
      ]);

      // Read-only root: staging in components/ and its backup rename succeed,
      // then deleting Button.module.css (backup rename in the root) fails.
      await chmod(cwd, 0o555);
      try {
        await assert.rejects(migrate({ cwd, cssFile: 'Button.module.css', write: true }));
      } finally {
        await chmod(cwd, 0o755);
      }

      assert.equal(await readFile(join(cwd, 'components', 'Button.tsx'), 'utf8'), componentTsx);
      assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), initialCss);
      const leftovers = (await readdir(join(cwd, 'components'))).filter((name) =>
        name.includes('.tw-migrate-'),
      );
      assert.deepEqual(leftovers, []);
    } finally {
      await cleanup(cwd);
    }
  },
);

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

test('auto-discovers every CSS target in the current package', async () => {
  const cwd = await fixture();
  try {
    const report = await migrate({ cwd });
    assert.deepEqual(report.changedFiles, ['Button.module.css', 'Button.tsx']);
    assert.deepEqual(report.failures, []);
    assert.deepEqual(report.candidates, ['p-[13px]']);
  } finally {
    await cleanup(cwd);
  }
});

test('batch migration updates one source from multiple CSS Modules without lost edits', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      rm(join(cwd, 'Button.module.css')),
      rm(join(cwd, 'Button.tsx')),
      writeFile(join(cwd, 'A.module.css'), '.a { padding: 13px; }\n'),
      writeFile(join(cwd, 'B.module.css'), '.b { color: red; }\n'),
      writeFile(
        join(cwd, 'App.tsx'),
        "import a from './A.module.css';\nimport b from './B.module.css';\nexport const App = () => <><div className={a.a} /><div className={b.b} /></>;\n",
      ),
    ]);

    const report = await migrate({ cwd, write: true });
    assert.deepEqual(report.changedFiles, ['A.module.css', 'App.tsx', 'B.module.css']);
    assert.equal(
      await readFile(join(cwd, 'App.tsx'), 'utf8'),
      'export const App = () => <><div className="p-[13px]" /><div className="text-[red]" /></>;\n',
    );
  } finally {
    await cleanup(cwd);
  }
});

test('explicit CSS paths bypass Git ignore filtering', async () => {
  const cwd = await fixture();
  try {
    await run('git', ['init', '-q'], { cwd });
    await Promise.all([
      writeFile(join(cwd, '.gitignore'), 'Ignored.module.css\n'),
      writeFile(join(cwd, 'Ignored.module.css'), '.ignored { display: grid; }\n'),
      writeFile(
        join(cwd, 'Ignored.tsx'),
        "import styles from './Ignored.module.css';\nexport const Ignored = () => <div className={styles.ignored} />;\n",
      ),
    ]);

    const automatic = await migrate({ cwd });
    assert.ok(!automatic.changedFiles.includes('Ignored.module.css'));
    const explicit = await migrate({ cwd, cssFile: 'Ignored.module.css' });
    assert.deepEqual(explicit.candidates, ['grid']);
    assert.ok(explicit.changedFiles.includes('Ignored.module.css'));
  } finally {
    await cleanup(cwd);
  }
});

test('reference-only workspace consumers prevent CSS Module deletion', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      rm(join(cwd, 'Button.module.css')),
      rm(join(cwd, 'Button.tsx')),
      run('git', ['init', '-q'], { cwd }),
      mkdir(join(cwd, 'shared')),
      mkdir(join(cwd, 'app')),
    ]);
    await Promise.all([
      writeFile(join(cwd, 'shared', 'package.json'), '{"private":true}'),
      writeFile(join(cwd, 'shared', 'globals.css'), '@import "tailwindcss";\n'),
      writeFile(join(cwd, 'shared', 'Button.module.css'), '.button { padding: 13px; }\n'),
      writeFile(join(cwd, 'app', 'package.json'), '{"private":true}'),
      writeFile(
        join(cwd, 'app', 'Button.tsx'),
        "import styles from '../shared/Button.module.css';\nexport const Button = () => <button className={styles.button} />;\n",
      ),
    ]);

    const report = await migrate({ cwd: join(cwd, 'shared'), write: true });
    assert.deepEqual(report.changedFiles, []);
    assert.equal(await readFile(join(cwd, 'shared', 'Button.module.css'), 'utf8'), '.button { padding: 13px; }\n');
    assert.ok(report.warnings.some((warning) => warning.code === 'reference-only-css-module-consumer'));
  } finally {
    await cleanup(cwd);
  }
});

test('workspace mode updates a selected cross-package consumer', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      rm(join(cwd, 'Button.module.css')),
      rm(join(cwd, 'Button.tsx')),
      rm(join(cwd, 'globals.css')),
      run('git', ['init', '-q'], { cwd }),
      mkdir(join(cwd, 'shared')),
      mkdir(join(cwd, 'app')),
    ]);
    await Promise.all([
      writeFile(join(cwd, 'shared', 'package.json'), '{"private":true}'),
      writeFile(join(cwd, 'shared', 'globals.css'), '@import "tailwindcss";\n'),
      writeFile(join(cwd, 'shared', 'Button.module.css'), '.button { padding: 13px; }\n'),
      writeFile(join(cwd, 'app', 'package.json'), '{"private":true}'),
      writeFile(
        join(cwd, 'app', 'Button.tsx'),
        "import styles from '../shared/Button.module.css';\nexport const Button = () => <button className={styles.button} />;\n",
      ),
    ]);

    const report = await migrate({ cwd, workspaces: true, write: true });
    assert.ok(report.changedFiles.includes('app/Button.tsx'));
    await assert.rejects(readFile(join(cwd, 'shared', 'Button.module.css'), 'utf8'), { code: 'ENOENT' });
    assert.equal(
      await readFile(join(cwd, 'app', 'Button.tsx'), 'utf8'),
      'export const Button = () => <button className="p-[13px]" />;\n',
    );
  } finally {
    await cleanup(cwd);
  }
});

test('rejects positional CSS owned by a nested package', async () => {
  const cwd = await fixture();
  try {
    await mkdir(join(cwd, 'nested'));
    await Promise.all([
      writeFile(join(cwd, 'nested', 'package.json'), '{"private":true}'),
      writeFile(join(cwd, 'nested', 'Nested.module.css'), '.nested { display: grid; }\n'),
    ]);

    await assert.rejects(
      migrate({ cwd, cssFile: 'nested/Nested.module.css' }),
      /must belong to the current package/,
    );
  } finally {
    await cleanup(cwd);
  }
});

test('rejects a Tailwind override owned by another package', async () => {
  const cwd = await fixture();
  try {
    await mkdir(join(cwd, 'nested'));
    await Promise.all([
      writeFile(join(cwd, 'nested', 'package.json'), '{"private":true}'),
      writeFile(join(cwd, 'nested', 'globals.css'), '@import "tailwindcss";\n'),
    ]);

    await assert.rejects(
      migrate({ cwd, tailwindCss: 'nested/globals.css' }),
      /Tailwind CSS entry must belong to the current package/,
    );
  } finally {
    await cleanup(cwd);
  }
});

test('excludes every detected Tailwind entry when an override selects one', async () => {
  const cwd = await fixture();
  try {
    await writeFile(join(cwd, 'admin.css'), '@import "tailwindcss";\n');

    const report = await migrate({ cwd, tailwindCss: 'globals.css' });
    assert.equal(report.rules.length, 1);
    assert.ok(report.warnings.every((warning) => warning.file !== 'admin.css'));
    assert.ok(!report.changedFiles.includes('admin.css'));
  } finally {
    await cleanup(cwd);
  }
});

test('combines classes from two stylesheets on one element end-to-end', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      rm(join(cwd, 'Button.module.css')),
      rm(join(cwd, 'Button.tsx')),
      writeFile(join(cwd, 'title.module.css'), '.title { padding: 13px; }\n'),
      writeFile(join(cwd, 'accent.module.css'), '.accent { color: red; }\n'),
      writeFile(
        join(cwd, 'Card.tsx'),
        "import title from './title.module.css';\nimport accent from './accent.module.css';\nexport const Card = () => <div className={`${title.title} ${accent.accent}`} />;\n",
      ),
    ]);

    const report = await migrate({ cwd, write: true });
    assert.equal(report.convertedRules, 2);
    await assert.rejects(readFile(join(cwd, 'title.module.css'), 'utf8'), { code: 'ENOENT' });
    await assert.rejects(readFile(join(cwd, 'accent.module.css'), 'utf8'), { code: 'ENOENT' });
    assert.equal(
      await readFile(join(cwd, 'Card.tsx'), 'utf8'),
      'export const Card = () => <div className="p-[13px] text-[red]" />;\n',
    );
  } finally {
    await cleanup(cwd);
  }
});

test('retains conflicting rules combined from two stylesheets on one element', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      rm(join(cwd, 'Button.module.css')),
      rm(join(cwd, 'Button.tsx')),
      writeFile(join(cwd, 'title.module.css'), '.title { padding: 13px; }\n'),
      writeFile(join(cwd, 'accent.module.css'), '.accent { padding: 4px; }\n'),
      writeFile(
        join(cwd, 'Card.tsx'),
        "import title from './title.module.css';\nimport accent from './accent.module.css';\nexport const Card = () => <div className={`${title.title} ${accent.accent}`} />;\n",
      ),
    ]);

    const report = await migrate({ cwd, write: true });
    assert.equal(report.convertedRules, 0);
    assert.equal(await readFile(join(cwd, 'title.module.css'), 'utf8'), '.title { padding: 13px; }\n');
    assert.equal(await readFile(join(cwd, 'accent.module.css'), 'utf8'), '.accent { padding: 4px; }\n');
    assert.match(await readFile(join(cwd, 'Card.tsx'), 'utf8'), /title\.module\.css/);
  } finally {
    await cleanup(cwd);
  }
});

test('detects source changes between planning reads', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      rm(join(cwd, 'Button.module.css')),
      rm(join(cwd, 'Button.tsx')),
      rm(join(cwd, 'globals.css')),
      run('git', ['init', '-q'], { cwd }),
      mkdir(join(cwd, 'aaa')),
      mkdir(join(cwd, 'bbb')),
    ]);
    const laterEntry = join(cwd, 'bbb', 'globals.css');
    await Promise.all([
      writeFile(join(cwd, 'aaa', 'package.json'), '{"private":true}'),
      writeFile(join(cwd, 'aaa', 'globals.css'), '@import "tailwindcss";\n@plugin "./mutate.cjs";\n'),
      writeFile(join(cwd, 'aaa', 'A.module.css'), '.a { padding: 13px; }\n'),
      writeFile(
        join(cwd, 'aaa', 'App.tsx'),
        "import styles from './A.module.css';\nexport const App = () => <div className={styles.a} />;\n",
      ),
      // Package aaa plans first; its plugin mutates bbb's already-snapshotted
      // entry, so bbb's later planning read must fire the planning-time guard.
      writeFile(
        join(cwd, 'aaa', 'mutate.cjs'),
        `const fs = require('node:fs');\nfs.appendFileSync(${JSON.stringify(laterEntry)}, '/* mutated */\\n');\nmodule.exports = () => {};\n`,
      ),
      writeFile(join(cwd, 'bbb', 'package.json'), '{"private":true}'),
      writeFile(laterEntry, '@import "tailwindcss";\n'),
      writeFile(join(cwd, 'bbb', 'B.module.css'), '.b { padding: 4px; }\n'),
      writeFile(
        join(cwd, 'bbb', 'B.tsx'),
        "import styles from './B.module.css';\nexport const B = () => <div className={styles.b} />;\n",
      ),
    ]);

    await assert.rejects(
      migrate({ cwd, workspaces: true }),
      /Source changed during planning: .*bbb[/\\]globals\.css/,
    );
  } finally {
    await cleanup(cwd);
  }
});

test('detects leftover files stranded outside the selected package', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      run('git', ['init', '-q'], { cwd }),
      mkdir(join(cwd, 'packages')),
    ]);
    await Promise.all([mkdir(join(cwd, 'packages', 'a')), mkdir(join(cwd, 'packages', 'b'))]);
    await Promise.all([
      writeFile(join(cwd, 'packages', 'a', 'package.json'), '{"private":true}'),
      writeFile(join(cwd, 'packages', 'b', 'package.json'), '{"private":true}'),
      writeFile(join(cwd, 'packages', 'b', '.B.tsx.tw-migrate-backup-1-0'), 'stranded original'),
    ]);

    // Running from packages/a must still surface the stranded backup a
    // crashed --workspaces run left in packages/b.
    await assert.rejects(migrate({ cwd: join(cwd, 'packages', 'a') }), /interrupted run/);
  } finally {
    await cleanup(cwd);
  }
});

test('ignores unparseable gitignored files without module references', async () => {
  const cwd = await fixture();
  try {
    await run('git', ['init', '-q'], { cwd });
    await Promise.all([
      writeFile(join(cwd, '.gitignore'), 'coverage/\n'),
      mkdir(join(cwd, 'coverage')),
    ]);
    await Promise.all([
      writeFile(join(cwd, 'coverage', 'report.js'), '<% generated: not JavaScript %>\n'),
      // Mentions ".module.css" but never names the target module: it must
      // pass the text filter yet still have no effect on the migration.
      writeFile(
        join(cwd, 'coverage', 'summary.js'),
        '<% files: ["other.module.css"] — not JavaScript %>\n',
      ),
    ]);

    const report = await migrate({ cwd, write: true });
    assert.equal(report.convertedRules, 1);
    await assert.rejects(readFile(join(cwd, 'Button.module.css'), 'utf8'), { code: 'ENOENT' });
  } finally {
    await cleanup(cwd);
  }
});

test('retains a module named by an unparseable gitignored file', async () => {
  const cwd = await fixture();
  try {
    await run('git', ['init', '-q'], { cwd });
    await writeFile(join(cwd, '.gitignore'), 'template.js\n');
    await writeFile(
      join(cwd, 'template.js'),
      '<% import styles from "./Button.module.css" %>\n',
    );

    const report = await migrate({ cwd, write: true });
    assert.equal(report.convertedRules, 0);
    assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), initialCss);
    assert.ok(report.warnings.some((warning) => warning.code === 'unsupported-css-module-reference'));
  } finally {
    await cleanup(cwd);
  }
});

test('retains a module referenced only from a gitignored consumer', async () => {
  const cwd = await fixture();
  try {
    await run('git', ['init', '-q'], { cwd });
    await Promise.all([
      writeFile(join(cwd, '.gitignore'), 'generated.js\n'),
      writeFile(
        join(cwd, 'generated.js'),
        "import styles from './Button.module.css';\nexport const buttonClass = styles.button;\n",
      ),
    ]);

    const report = await migrate({ cwd, write: true });
    assert.equal(report.convertedRules, 0);
    assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), initialCss);
    assert.match(await readFile(join(cwd, 'generated.js'), 'utf8'), /Button\.module\.css/);
    assert.ok(!report.changedFiles.includes('generated.js'));
  } finally {
    await cleanup(cwd);
  }
});

test('retains a module composed by a gitignored stylesheet', async () => {
  const cwd = await fixture();
  try {
    await run('git', ['init', '-q'], { cwd });
    await Promise.all([
      writeFile(join(cwd, '.gitignore'), 'Consumer.module.css\n'),
      writeFile(
        join(cwd, 'Consumer.module.css'),
        ".fancy {\n  composes: button from './Button.module.css';\n}\n",
      ),
    ]);

    const report = await migrate({ cwd, write: true });
    assert.equal(report.convertedRules, 0);
    assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), initialCss);
    assert.ok(report.warnings.some((warning) => warning.code === 'unsupported-css-module-reference'));
    assert.ok(!report.changedFiles.includes('Consumer.module.css'));
  } finally {
    await cleanup(cwd);
  }
});

test('verifies reference-only source snapshots before writing', async () => {
  const cwd = await fixture({
    tailwind: '@import "tailwindcss";\n@plugin "./mutate.cjs";\n',
  });
  try {
    await Promise.all([
      run('git', ['init', '-q'], { cwd }),
      mkdir(join(cwd, 'external')),
    ]);
    const externalPath = join(cwd, 'external', 'Note.tsx');
    await Promise.all([
      writeFile(join(cwd, 'external', 'package.json'), '{"private":true}'),
      writeFile(externalPath, 'export const Note = () => <div />;\n'),
      writeFile(
        join(cwd, 'mutate.cjs'),
        `const fs = require('node:fs');\nfs.appendFileSync(${JSON.stringify(externalPath)}, '// changed during planning\\n');\nmodule.exports = () => {};\n`,
      ),
    ]);

    await assert.rejects(
      migrate({ cwd, write: true }),
      /Source changed after planning: .*external[/\\]Note\.tsx/,
    );
    assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), initialCss);
  } finally {
    await cleanup(cwd);
  }
});

test('--force never swallows cross-group plan collisions', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      rm(join(cwd, 'Button.module.css')),
      rm(join(cwd, 'Button.tsx')),
      rm(join(cwd, 'globals.css')),
      run('git', ['init', '-q'], { cwd }),
      mkdir(join(cwd, 'a')),
      mkdir(join(cwd, 'b')),
      mkdir(join(cwd, 'app')),
    ]);
    await Promise.all([
      writeFile(join(cwd, 'a', 'package.json'), '{"private":true}'),
      writeFile(join(cwd, 'a', 'globals.css'), '@import "tailwindcss";\n'),
      writeFile(join(cwd, 'a', 'A.module.css'), '.a { padding: 13px; }\n'),
      writeFile(join(cwd, 'b', 'package.json'), '{"private":true}'),
      writeFile(join(cwd, 'b', 'globals.css'), '@import "tailwindcss";\n'),
      writeFile(join(cwd, 'b', 'B.module.css'), '.b { color: red; }\n'),
      writeFile(join(cwd, 'app', 'package.json'), '{"private":true}'),
      writeFile(
        join(cwd, 'app', 'App.tsx'),
        "import a from '../a/A.module.css';\nimport b from '../b/B.module.css';\nexport const App = () => <><div className={a.a} /><div className={b.b} /></>;\n",
      ),
    ]);

    await assert.rejects(
      migrate({ cwd, workspaces: true, force: true, write: true }),
      /Multiple package groups planned changes for/,
    );
  } finally {
    await cleanup(cwd);
  }
});

test('--force skips a package with malformed input CSS', async () => {
  const cwd = await fixture({ css: '}\n' });
  try {
    const report = await migrate({ cwd, force: true, write: true });

    assert.deepEqual(report.changedFiles, []);
    assert.equal(report.failures.length, 1);
    assert.equal(report.failures[0].package, '.');
    assert.match(report.failures[0].message, /Failed to parse .*Button\.module\.css/);
    assert.equal(await readFile(join(cwd, 'Button.module.css'), 'utf8'), '}\n');
    assert.equal(await readFile(join(cwd, 'Button.tsx'), 'utf8'), initialTsx);
  } finally {
    await cleanup(cwd);
  }
});

test('--force skips a broken workspace package and applies successful groups', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      rm(join(cwd, 'Button.module.css')),
      rm(join(cwd, 'Button.tsx')),
      rm(join(cwd, 'globals.css')),
      run('git', ['init', '-q'], { cwd }),
      mkdir(join(cwd, 'good')),
      mkdir(join(cwd, 'broken')),
    ]);
    await Promise.all([
      writeFile(join(cwd, 'good', 'package.json'), '{"private":true}'),
      writeFile(join(cwd, 'good', 'globals.css'), '@import "tailwindcss";\n'),
      writeFile(join(cwd, 'good', 'Good.module.css'), '.good { display: grid; }\n'),
      writeFile(
        join(cwd, 'good', 'Good.tsx'),
        "import styles from './Good.module.css';\nexport const Good = () => <div className={styles.good} />;\n",
      ),
      writeFile(join(cwd, 'broken', 'package.json'), '{"private":true}'),
      writeFile(join(cwd, 'broken', 'globals.css'), '@import "tailwindcss";\n'),
      writeFile(join(cwd, 'broken', 'Broken.module.css'), '}\n'),
    ]);

    await assert.rejects(migrate({ cwd, workspaces: true, write: true }));
    const report = await migrate({ cwd, workspaces: true, force: true, write: true });
    assert.deepEqual(report.failures.map((failure) => failure.package), ['broken']);
    assert.ok(report.changedFiles.includes('good/Good.module.css'));
    await assert.rejects(readFile(join(cwd, 'good', 'Good.module.css'), 'utf8'), { code: 'ENOENT' });
    assert.equal(
      await readFile(join(cwd, 'good', 'Good.tsx'), 'utf8'),
      'export const Good = () => <div className="grid" />;\n',
    );
  } finally {
    await cleanup(cwd);
  }
});
