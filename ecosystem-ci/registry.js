import { spawn } from 'node:child_process';
import { closeSync, openSync } from 'node:fs';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import net from 'node:net';
import { dirname, join } from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);

export function registryConfig({ storage, allowPublish }) {
  const publish = allowPublish ? '$all' : 'nobody';
  return `storage: ${JSON.stringify(storage)}
auth:
  htpasswd:
    file: ${JSON.stringify(join(storage, 'htpasswd'))}
    max_users: ${allowPublish ? 1 : -1}
uplinks:
  npmjs:
    url: https://registry.npmjs.org/
packages:
  'tw-migrate':
    access: $all
    publish: ${publish}
    unpublish: nobody
    proxy: false
  'tw-migrate-*':
    access: $all
    publish: ${publish}
    unpublish: nobody
    proxy: false
  '@*/*':
    access: $all
    publish: nobody
    unpublish: nobody
    proxy: npmjs
  '**':
    access: $all
    publish: nobody
    unpublish: nobody
    proxy: npmjs
log: { type: stdout, format: pretty, level: http }
`;
}

async function availablePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close((error) => error ? reject(error) : resolve(port));
    });
  });
}

async function waitForRegistry(url, child, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`registry exited with status ${child.exitCode}`);
    try {
      const response = await fetch(`${url}/-/ping`, { signal: AbortSignal.timeout(500) });
      if (response.ok) return;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`registry did not start within ${timeoutMs}ms`);
}

async function terminate(child, timeoutMs = 5_000) {
  if (child.exitCode !== null) return;
  child.kill('SIGTERM');
  const exited = new Promise((resolve) => child.once('exit', resolve));
  const timedOut = await Promise.race([
    exited.then(() => false),
    new Promise((resolve) => setTimeout(() => resolve(true), timeoutMs)),
  ]);
  if (timedOut && child.exitCode === null) {
    child.kill('SIGKILL');
    await exited;
  }
}

export async function startRegistry({ root, artifactRoot, allowPublish, timeoutMs = 15_000 }) {
  const verdaccioBin = join(dirname(require.resolve('verdaccio/package.json')), 'bin', 'verdaccio');
  await Promise.all([mkdir(root, { recursive: true }), mkdir(artifactRoot, { recursive: true })]);
  const storage = join(root, 'storage');
  const configPath = join(root, allowPublish ? 'bootstrap.yaml' : 'sealed.yaml');
  const logPath = join(artifactRoot, allowPublish ? 'registry-bootstrap.log' : 'registry-install.log');
  await mkdir(storage, { recursive: true });
  await writeFile(configPath, registryConfig({ storage, allowPublish }));
  const port = await availablePort();
  const url = `http://127.0.0.1:${port}`;
  const log = openSync(logPath, 'a');
  const child = spawn(process.execPath, [verdaccioBin, '--config', configPath, '--listen', `127.0.0.1:${port}`], {
    stdio: ['ignore', log, log],
    windowsHide: true,
  });

  try {
    await waitForRegistry(url, child, timeoutMs);
  } catch (error) {
    await terminate(child);
    closeSync(log);
    const output = await readFile(logPath, 'utf8').catch(() => '');
    throw new Error(`${error.message}\nregistry log: ${logPath}\n${output}`);
  }

  let stopped = false;
  return {
    url,
    logPath,
    async stop() {
      if (stopped) return;
      stopped = true;
      await terminate(child);
      closeSync(log);
    },
  };
}
