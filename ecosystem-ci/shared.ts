import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import net from "node:net";
import { isAbsolute, relative } from "node:path";

// Windows resolves bare `npm`/`pnpm`/`npx`/`corepack` to a `.cmd` shim that
// spawn() can only execute through a shell.
export function platformCommand(name: string): string {
  return process.platform === "win32" ? `${name}.cmd` : name;
}

export function inside(path: string, root: string): boolean {
  const rel = relative(root, path);
  return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
}

export async function availablePort(): Promise<number> {
  return new Promise((resolvePort, reject) => {
    const server = net.createServer();
    server.unref();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (address === null || typeof address === "string") {
        server.close(() => reject(new Error("listening socket reported no numeric port")));
        return;
      }
      const { port } = address;
      server.close((error) => (error ? reject(error) : resolvePort(port)));
    });
  });
}

export async function sha256(path: string): Promise<string> {
  return createHash("sha256")
    .update(await readFile(path))
    .digest("hex");
}
