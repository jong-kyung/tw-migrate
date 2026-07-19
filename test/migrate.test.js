import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import test from 'node:test';

import { migrate } from '../index.js';

const initialCss = '.button { padding: 13px; }\n';
const initialTsx = "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n";

async function fixture() {
  await mkdir('.tmp', { recursive: true });
  const cwd = await mkdtemp(join(process.cwd(), '.tmp', 'fixture-'));
  await Promise.all([
    writeFile(join(cwd, 'package.json'), '{"private":true}'),
    writeFile(join(cwd, 'globals.css'), '@import "tailwindcss";\n'),
    writeFile(join(cwd, 'Button.module.css'), initialCss),
    writeFile(join(cwd, 'Button.tsx'), initialTsx),
  ]);
  return cwd;
}

async function cleanup(cwd) {
  await rm(cwd, { recursive: true, force: true });
}

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
