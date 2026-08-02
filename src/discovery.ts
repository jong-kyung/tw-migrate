import { execFile } from "node:child_process";
import { readFile, readdir, realpath, stat } from "node:fs/promises";
import { basename, dirname, join, relative, resolve } from "node:path";
import { promisify } from "node:util";

import {
  IGNORED_DIRECTORIES,
  SOURCE_EXTENSIONS,
  errorCode,
  extension,
  isStylesheetPath,
  isWithin,
  rejectSymlinkTarget,
} from "./util/shared.ts";
import type { MigrateOptions, Scope } from "./types.ts";

const run = promisify(execFile);

export async function resolveScope(options: MigrateOptions): Promise<Scope> {
  const cwd = await realpath(resolve(options.cwd ?? process.cwd()));
  const currentPackage = await findPackageRoot(cwd);
  const gitRoot = await findGitRoot(currentPackage);
  const workspaceRoot =
    gitRoot && !(await isIgnoredByGit(gitRoot, currentPackage)) ? gitRoot : currentPackage;
  const allPaths = await discoverFiles(workspaceRoot, workspaceRoot === gitRoot);
  // Ignore filtering scopes what gets migrated, never what gets scanned:
  // gitignored consumers and stylesheets must still block unsafe deletion.
  const scannedPaths =
    workspaceRoot === gitRoot
      ? [
          ...new Set([
            ...allPaths,
            ...(await collectFiles(workspaceRoot, isRelevantDiscoveredFile)),
          ]),
        ]
      : [...allPaths];
  const explicitStyle = options.styleFile ? resolve(cwd, options.styleFile) : undefined;
  const configuredEntry = options.tailwindCss ? resolve(cwd, options.tailwindCss) : undefined;
  if (explicitStyle && !isStylesheetPath(explicitStyle) && extension(explicitStyle) !== ".vue") {
    throw new TypeError("Only .css, .scss, .sass, .less, and .vue files can be migrated");
  }
  if (configuredEntry && extension(configuredEntry) !== ".css") {
    throw new TypeError("The Tailwind CSS entry must be a .css file");
  }
  if (configuredEntry && !(await stat(configuredEntry)).isFile()) {
    throw new TypeError("The Tailwind CSS entry must be a file");
  }
  for (const path of [explicitStyle, configuredEntry]) {
    if (path) await rejectSymlinkTarget(path, currentPackage);
  }
  if (explicitStyle && !isWithin(currentPackage, explicitStyle)) {
    throw new TypeError("The selected stylesheet must belong to the current package");
  }
  for (const path of [explicitStyle, configuredEntry]) {
    if (path && !allPaths.includes(path)) allPaths.push(path);
    if (path && !scannedPaths.includes(path)) scannedPaths.push(path);
  }
  allPaths.sort();
  scannedPaths.sort();
  const targetable = new Set(allPaths);

  const allPackageRoots = await discoverPackageRoots(workspaceRoot, allPaths);
  if (!allPackageRoots.includes(currentPackage)) allPackageRoots.push(currentPackage);
  for (const path of [explicitStyle, configuredEntry]) {
    if (!path) continue;
    const owner = await findPackageRoot(dirname(path));
    if (!allPackageRoots.includes(owner)) allPackageRoots.push(owner);
  }
  allPackageRoots.sort();
  const rootsByDepth = [...allPackageRoots].sort((left, right) => right.length - left.length);
  const pathOwners = new Map(
    scannedPaths.map((path): [string, string | undefined] => [
      path,
      rootsByDepth.find((root) => isWithin(root, path)),
    ]),
  );
  if (explicitStyle && pathOwners.get(explicitStyle) !== currentPackage) {
    throw new TypeError("The selected stylesheet must belong to the current package");
  }
  if (configuredEntry && pathOwners.get(configuredEntry) !== currentPackage) {
    throw new TypeError("The Tailwind CSS entry must belong to the current package");
  }
  const selectedPackages = options.workspaces ? allPackageRoots : [currentPackage];
  return {
    cwd,
    workspaceRoot,
    scannedPaths,
    targetable,
    explicitStyle,
    configuredEntry,
    pathOwners,
    selectedPackages,
  };
}

async function findPackageRoot(start: string): Promise<string> {
  let directory = start;
  while (true) {
    try {
      await readFile(join(directory, "package.json"), "utf8");
      return directory;
    } catch (error) {
      if (errorCode(error) !== "ENOENT") throw error;
    }
    const parent = dirname(directory);
    if (parent === directory) throw new Error(`No package.json was found from ${start}`);
    directory = parent;
  }
}

async function findGitRoot(cwd: string): Promise<string | undefined> {
  try {
    const { stdout } = await run("git", ["rev-parse", "--show-toplevel"], { cwd });
    return resolve(stdout.trim());
  } catch {
    return undefined;
  }
}

async function isIgnoredByGit(gitRoot: string, path: string): Promise<boolean> {
  if (gitRoot === path) return false;
  try {
    await run("git", ["check-ignore", "-q", "--", relative(gitRoot, path)], { cwd: gitRoot });
    return true;
  } catch {
    return false;
  }
}

async function discoverFiles(root: string, useGit: boolean): Promise<string[]> {
  if (!useGit) return collectFiles(root, isRelevantDiscoveredFile);

  const { stdout } = await run("git", ["ls-files", "-co", "--exclude-standard", "-z", "--", "."], {
    cwd: root,
    maxBuffer: 64 * 1024 * 1024,
  });
  const paths = stdout
    .split("\0")
    .filter(Boolean)
    .map((path) => resolve(root, path))
    .filter((path) => !hasIgnoredDirectory(root, path) && isRelevantDiscoveredFile(path));
  const existing = await Promise.all(
    paths.map(async (path) => {
      try {
        return (await stat(path)).isFile() ? path : undefined;
      } catch {
        return undefined;
      }
    }),
  );
  return existing.flatMap((path) => (path ? [path] : [])).sort();
}

async function discoverPackageRoots(workspaceRoot: string, paths: string[]): Promise<string[]> {
  const roots = paths.filter((path) => basename(path) === "package.json").map(dirname);
  try {
    await readFile(join(workspaceRoot, "package.json"), "utf8");
    roots.push(workspaceRoot);
  } catch (error) {
    if (errorCode(error) !== "ENOENT") throw error;
  }
  return [...new Set(roots)].sort();
}

function hasIgnoredDirectory(root: string, path: string): boolean {
  return relative(root, path)
    .split(/[\\/]/)
    .some((part) => IGNORED_DIRECTORIES.has(part));
}

function isRelevantDiscoveredFile(path: string): boolean {
  return (
    basename(path) === "package.json" ||
    isStylesheetPath(path) ||
    SOURCE_EXTENSIONS.has(extension(path))
  );
}

export async function collectFiles(
  root: string,
  include: (path: string) => boolean,
): Promise<string[]> {
  const files: string[] = [];
  async function visit(directory: string): Promise<void> {
    const entries = await readdir(directory, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        if (!IGNORED_DIRECTORIES.has(entry.name)) await visit(path);
      } else if (entry.isFile() && include(path)) {
        files.push(path);
      }
    }
  }
  await visit(root);
  return files;
}
