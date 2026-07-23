import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, symlink, unlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';
import test from 'node:test';

import { __unstable__loadDesignSystem as loadDesignSystem } from 'tailwindcss';

import { migrate } from '../index.js';
import { sourceMappings } from '../style-compiler.js';

const initialCss = '.button { padding: 13px; }\n';
const initialTsx = "import styles from './Button.module.css';\nexport const Button = () => <button className={styles.button}>Save</button>;\n";

async function fixture({ css = initialCss, tsx = initialTsx } = {}) {
  await mkdir('.tmp', { recursive: true });
  const cwd = await mkdtemp(join(process.cwd(), '.tmp', 'fixture-'));
  await Promise.all([
    writeFile(join(cwd, 'package.json'), '{"private":true}'),
    writeFile(join(cwd, 'globals.css'), '@import "tailwindcss";\n'),
    writeFile(join(cwd, 'Button.module.css'), css),
    writeFile(join(cwd, 'Button.tsx'), tsx),
  ]);
  return cwd;
}

async function cleanup(cwd) {
  await rm(cwd, { recursive: true, force: true });
}

test('canonicalizes aliased cwd paths before Git discovery', async () => {
  const cwd = await fixture();
  const alias = `${cwd}-alias`;
  let linked = false;
  try {
    execFileSync('git', ['init', '-q'], { cwd });
    await symlink(cwd, alias, process.platform === 'win32' ? 'junction' : 'dir');
    linked = true;
    const report = await migrate({ cwd: alias });
    assert.deepEqual(report.changedFiles, ['Button.module.css', 'Button.tsx']);
  } finally {
    if (linked) await unlink(alias);
    await cleanup(cwd);
  }
});

test('validates API-only migration options', async () => {
  const cwd = await fixture();
  try {
    await assert.rejects(
      migrate({ cwd, cssFile: 'Button.module.css' }),
      /cssFile has been replaced by styleFile/,
    );
    await assert.rejects(
      migrate({ cwd, styleFile: 'Button.module.css', workspaces: true }),
      /styleFile cannot be combined with workspaces/,
    );
    await assert.rejects(
      migrate({ cwd, styleFile: 'legacy.pcss' }),
      /Only \.css, \.scss, \.sass, and \.less files can be migrated/,
    );
    await assert.rejects(
      migrate({ cwd, tailwindCss: 'globals.scss' }),
      /Tailwind CSS entry must be a \.css file/,
    );
  } finally {
    await cleanup(cwd);
  }
});

test('normalizes separators when resolving source map roots', () => {
  const sourceRoot = pathToFileURL(`${join(tmpdir(), 'nested')}/`).href;
  assert.equal(
    sourceMappings({ version: 3, sourceRoot, sources: ['input.scss'], names: [], mappings: 'AAAA' })[0].sourcePath,
    join(tmpdir(), 'nested', 'input.scss'),
  );
  assert.equal(
    sourceMappings({ version: 3, sourceRoot, sources: ['../input.scss'], names: [], mappings: 'AAAA' })[0].sourcePath,
    join(tmpdir(), 'input.scss'),
  );
  assert.equal(
    sourceMappings({ version: 3, sourceRoot: `${sourceRoot}/`, sources: ['../input.scss'], names: [], mappings: 'AAAA' })[0].sourcePath,
    join(tmpdir(), 'input.scss'),
  );
});

test('retains nested SCSS rules whose expansion prevents a unique authored mapping', async () => {
  const cwd = await fixture();
  try {
    await Promise.all([
      rm(join(cwd, 'Button.module.css')),
      writeFile(join(cwd, 'Card.module.scss'), '.parent { padding: 13px; .child { margin: 12px; } }\n'),
      writeFile(
        join(cwd, 'Card.tsx'),
        "import styles from './Card.module.scss';\nexport const Card = () => <div className={styles.parent}><span className={styles.child} /></div>;\n",
      ),
    ]);
    const report = await migrate({ cwd, styleFile: 'Card.module.scss' });
    assert.deepEqual(
      report.warnings.map((warning) => [warning.code, warning.start, warning.end]),
      [['unproven-source-map', 0, 0]],
    );
  } finally {
    await cleanup(cwd);
  }
});

test('retains a disproven SCSS descendant relationship with authored offsets', async () => {
  const source = '$m: 12px;\n.parent { padding: 13px; }\n.parent .child { margin: $m; }\n';
  const cwd = await fixture();
  try {
    await Promise.all([
      rm(join(cwd, 'Button.module.css')),
      writeFile(join(cwd, 'Card.module.scss'), source),
      writeFile(
        join(cwd, 'Card.tsx'),
        "import styles from './Card.module.scss';\nexport const Card = () => <><div className={styles.parent} /><span className={styles.child} /></>;\n",
      ),
    ]);
    const report = await migrate({ cwd, styleFile: 'Card.module.scss' });
    const warning = report.warnings.find((entry) => entry.code === 'unproven-css-module-relationship');
    const start = source.indexOf('.parent .child');
    assert.deepEqual(
      [warning.file, warning.start, warning.end],
      ['Card.module.scss', start, source.indexOf('}', start) + 1],
    );
  } finally {
    await cleanup(cwd);
  }
});

test('only follows real top-level CSS imports and preserves media warning offsets', async () => {
  const cwd = await fixture();
  const source = '/* 한글 */\n@import "./print.css" print;\n@import "./speech.css" speech;\n.fake::before { content: "@import \'./trap.css\';"; }\n';
  try {
    await Promise.all([
      writeFile(join(cwd, 'base.css'), source),
      writeFile(join(cwd, 'print.css'), '.print { padding: 13px; }\n'),
      writeFile(join(cwd, 'speech.css'), '.speech { height: 100vh; }\n'),
      writeFile(
        join(cwd, 'index.html'),
        '<link rel="stylesheet" href="./base.css"><div class="print speech"></div>\n',
      ),
    ]);
    const report = await migrate({ cwd });
    const warning = report.warnings.find(
      (entry) => entry.code === 'unsupported-link-media' && entry.file === 'base.css',
    );
    assert.equal(
      warning.start,
      Buffer.byteLength(source.slice(0, source.indexOf('@import "./speech.css"'))),
    );
  } finally {
    await cleanup(cwd);
  }
});

test('anchors Sass compile-failure warnings to authored offsets', async () => {
  const source = '$space: 13px;\n.pad { padding: $space; }\n.button { COLOR: red; }\n';
  const cwd = await fixture();
  try {
    await Promise.all([
      rm(join(cwd, 'Button.module.css')),
      writeFile(join(cwd, 'Button.module.scss'), source),
      writeFile(
        join(cwd, 'Button.tsx'),
        "import styles from './Button.module.scss';\nexport const Button = () => <button className={styles.button}><i className={styles.pad} /></button>;\n",
      ),
    ]);
    const report = await migrate({ cwd, styleFile: 'Button.module.scss' });
    const warning = report.warnings.find((entry) => entry.code === 'candidate-compilation-failure');
    const start = source.indexOf('.button');
    const end = source.indexOf('}', start) + 1;
    assert.equal(warning.file, 'Button.module.scss');
    assert.ok(warning.start >= start && warning.end <= end && warning.end > warning.start);
  } finally {
    await cleanup(cwd);
  }
});

test('escapes literal underscores in arbitrary values', async () => {
  const cwd = await fixture({ css: '.button { --font-key: Open_Sans; }\n' });
  try {
    const report = await migrate({ cwd, styleFile: 'Button.module.css' });
    const designSystem = await loadDesignSystem('@tailwind utilities;');
    const css = designSystem.candidatesToCss(report.candidates).join('');
    assert.match(css, /Open_Sans/);
    assert.doesNotMatch(css, /Open Sans/);
  } finally {
    await cleanup(cwd);
  }
});

test('round-trips quoted values and urls through arbitrary candidates', async () => {
  const cwd = await fixture({
    css: '.button { background-image: url("a_b.png"); font-family: "My Font", sans-serif; content: "a_b"; width: calc(min(100%, 50vw)); }\n',
  });
  try {
    const report = await migrate({ cwd, styleFile: 'Button.module.css' });
    const designSystem = await loadDesignSystem('@tailwind utilities;');
    const css = designSystem.candidatesToCss(report.candidates).join('');
    assert.match(css, /url\("a_b\.png"\)/);
    assert.match(css, /"My Font", sans-serif/);
    assert.match(css, /content: "a_b"/);
    assert.match(css, /calc\(min\(100%, 50vw\)\)/);
  } finally {
    await cleanup(cwd);
  }
});

test('preserves CRLF line endings through a partial migration', async () => {
  const cwd = await fixture({
    css: '.button {\r\n  padding: 13px;\r\n}\r\n.other {\r\n  display: grid;\r\n}\r\n',
    tsx: initialTsx.replaceAll('\n', '\r\n'),
  });
  try {
    await migrate({ cwd, styleFile: 'Button.module.css', write: true });
    assert.deepEqual(
      await readFile(join(cwd, 'Button.module.css')),
      Buffer.from('\r\n.other {\r\n  display: grid;\r\n}\r\n'),
    );
    assert.deepEqual(
      await readFile(join(cwd, 'Button.tsx')),
      Buffer.from(
        "import styles from './Button.module.css';\r\nexport const Button = () => <button className=\"p-[13px]\">Save</button>;\r\n",
      ),
    );
  } finally {
    await cleanup(cwd);
  }
});
