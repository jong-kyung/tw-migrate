import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import net from 'node:net';
import { isAbsolute, relative } from 'node:path';

// Windows resolves bare `npm`/`pnpm`/`npx`/`corepack` to a `.cmd` shim that
// spawn() can only execute through a shell.
export function platformCommand(name) {
  return process.platform === 'win32' ? `${name}.cmd` : name;
}

export function inside(path, root) {
  const rel = relative(root, path);
  return rel === '' || (!rel.startsWith('..') && !isAbsolute(rel));
}

export async function availablePort() {
  return new Promise((resolvePort, reject) => {
    const server = net.createServer();
    server.unref();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close((error) => error ? reject(error) : resolvePort(port));
    });
  });
}

export async function sha256(path) {
  return createHash('sha256').update(await readFile(path)).digest('hex');
}
