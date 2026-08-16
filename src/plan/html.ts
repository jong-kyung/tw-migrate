import { basename, dirname, extname, resolve } from "node:path";

import { parseHtmlSource } from "../parser/html.ts";
import { isPreprocessorPath } from "../parser/style-compiler.ts";
import {
  addStyleDependent,
  cssImports,
  errorCode,
  isStylesheetModule,
  isStylesheetPath,
  isWithin,
  localHrefTarget,
  snapshotFile,
} from "../util/shared.ts";
import { importsStylesheet } from "./entry.ts";
import type {
  HtmlContext,
  MigrationWarning,
  PreparedHtml,
  PreparedSourceFile,
  RemovableLink,
  SourceFile,
} from "../types.ts";

export async function preparePackageHtml({
  packageRoot,
  sourceFiles,
  styleSources,
  snapshots,
  pathOwners,
  styleDependents,
}: {
  packageRoot: string;
  sourceFiles: SourceFile[];
  styleSources: Map<string, string>;
  snapshots: Map<string, string>;
  pathOwners: Map<string, string | undefined>;
  styleDependents: Map<string, string[]>;
}): Promise<PreparedHtml> {
  const files: PreparedSourceFile[] = [];
  const stylePaths = new Set<string>();
  const generatedPaths = new Set<string>();
  const removableLinks: RemovableLink[] = [];
  const warnings: MigrationWarning[] = [];

  const htmlFiles = sourceFiles.filter((file) => extname(file.path) === ".html");
  // Package-owned HTML goes first so stylesheets it discovers are claimed for
  // this package before foreign consumers are matched against that ownership.
  const orderedFiles = [
    ...htmlFiles.filter((file) => pathOwners.get(file.path) === packageRoot),
    ...htmlFiles.filter((file) => pathOwners.get(file.path) !== packageRoot),
  ];
  for (const file of orderedFiles) {
    const owner = pathOwners.get(file.path);
    // Foreign HTML is analyzed only as a consumer of this package's
    // stylesheets; its other links and attributes are its own package's
    // concern, so they never warn here.
    const foreign = owner !== packageRoot;
    if (foreign && !owner) continue;
    const referenceRoot = foreign ? owner : packageRoot;
    const parsed = parseHtmlSource(file.path, file.source);
    const contexts: HtmlContext[] = [];
    let linkBase: string | undefined = dirname(file.path);
    const base = parsed.bases[0];
    if (base) {
      const baseReference = base.href.split(/[?#]/, 1)[0];
      const basePath =
        base.writable &&
        (baseReference === ""
          ? file.path
          : localHtmlReference(referenceRoot, dirname(file.path), base.href));
      if (!basePath || !isWithin(referenceRoot, basePath)) {
        if (!foreign) {
          warnings.push(
            htmlWarning(
              "unsupported-html-base",
              file.path,
              base.start,
              base.end,
              "A remote or unrepresentable base URL prevents safe stylesheet link resolution.",
            ),
          );
        }
        linkBase = undefined;
      } else {
        linkBase = base.href.split(/[?#]/, 1)[0].endsWith("/") ? basePath : dirname(basePath);
      }
    }
    for (const link of parsed.links) {
      const linkedPath = linkBase && localHtmlReference(referenceRoot, linkBase, link.href);
      if (
        !linkedPath ||
        !isWithin(packageRoot, linkedPath) ||
        (foreign && pathOwners.get(linkedPath) !== packageRoot)
      ) {
        if (!foreign) {
          warnings.push(
            htmlWarning(
              "unsupported-html-stylesheet-link",
              file.path,
              link.start,
              link.end,
              "Only local package stylesheet links are analyzed.",
            ),
          );
        }
        continue;
      }
      const variants = mediaVariants(link.media);
      if (variants === undefined) {
        const cssPath =
          (foreign
            ? undefined
            : inferredPreprocessorPath({
                path: linkedPath,
                packageRoot,
                styleSources,
                pathOwners,
                styleDependents,
              })) ?? linkedPath;
        contexts.push({ cssPath, variants: [], direct: true, analyzable: false });
        if (!foreign) {
          warnings.push(
            htmlWarning(
              "unsupported-link-media",
              file.path,
              link.start,
              link.end,
              `The stylesheet link media condition ${JSON.stringify(link.media)} cannot be represented safely.`,
            ),
          );
        }
        continue;
      }
      const contextStart = contexts.length;
      await collectHtmlStyleContexts({
        path: linkedPath,
        variants,
        direct: true,
        packageRoot,
        sourceFiles,
        styleSources,
        snapshots,
        pathOwners,
        styleDependents,
        stylePaths,
        generatedPaths,
        contexts,
        warnings,
        visited: new Set(),
      });
      const directContext = contexts[contextStart];
      if (directContext?.direct) {
        removableLinks.push({
          filePath: file.path,
          cssPath: directContext.cssPath,
          href: link.href,
          media: link.media,
        });
      }
    }

    if (foreign && contexts.length === 0) continue;
    if (!foreign && contexts.length > 0) {
      for (const attribute of parsed.dynamicAttributes) {
        warnings.push(
          htmlWarning(
            "dynamic-html-attribute",
            file.path,
            attribute.start,
            attribute.end,
            "This HTML attribute is not a safely writable quoted literal.",
          ),
        );
      }
    }
    files.push({
      ...file,
      htmlElements: parsed.elements,
      htmlStylesheets: deduplicateHtmlContexts(contexts),
      htmlReferencesSafe: parsed.dynamicAttributes.length === 0,
      htmlScriptText: parsed.scriptText,
    });
  }

  return { files, stylePaths, generatedPaths, removableLinks, warnings };
}

interface HtmlContextState {
  path: string;
  variants: string[];
  direct: boolean;
  packageRoot: string;
  sourceFiles: SourceFile[];
  styleSources: Map<string, string>;
  snapshots: Map<string, string>;
  pathOwners: Map<string, string | undefined>;
  styleDependents: Map<string, string[]>;
  stylePaths: Set<string>;
  generatedPaths: Set<string>;
  contexts: HtmlContext[];
  warnings: MigrationWarning[];
  visited: Set<string>;
}

async function collectHtmlStyleContexts(state: HtmlContextState): Promise<void> {
  const key = `${state.path}\0${state.variants.join(":")}`;
  if (state.visited.has(key)) return;
  state.visited.add(key);
  if (!isStylesheetPath(state.path)) return;
  const owner = state.pathOwners.get(state.path);
  if (owner && owner !== state.packageRoot) {
    state.warnings.push(
      htmlWarning(
        "cross-package-stylesheet-link",
        state.path,
        0,
        0,
        "A stylesheet owned by another package is not analyzed outside workspace mode.",
      ),
    );
    return;
  }

  let source;
  try {
    source =
      state.styleSources.get(state.path) ?? (await snapshotFile(state.snapshots, state.path));
  } catch (error) {
    if (errorCode(error) === "ENOENT") {
      if (extname(state.path) === ".css") addInferredPreprocessorContext(state);
      return;
    }
    throw error;
  }
  if (!state.styleSources.has(state.path)) state.styleSources.set(state.path, source);
  if (!owner) state.pathOwners.set(state.path, state.packageRoot);

  if (extname(state.path) === ".css" && addInferredPreprocessorContext(state)) return;

  state.stylePaths.add(state.path);
  state.contexts.push({
    cssPath: state.path,
    variants: state.variants,
    direct: state.direct,
    analyzable: true,
  });
  if (extname(state.path) !== ".css") return;

  for (const imported of cssImports(state.path, source)) {
    const variants = mediaVariants(imported.media);
    if (variants === undefined) {
      state.warnings.push(
        htmlWarning(
          "unsupported-link-media",
          state.path,
          imported.start,
          imported.end,
          `The stylesheet import media condition ${JSON.stringify(imported.media)} cannot be represented safely.`,
        ),
      );
      continue;
    }
    const importedPath = localHtmlReference(state.packageRoot, dirname(state.path), imported.href);
    if (!importedPath || !isWithin(state.packageRoot, importedPath)) continue;
    // Link-discovered stylesheets never went through indexStylesheetDependents,
    // so record their import edges here or deletion could leave this importer
    // pointing at a removed module.
    if (
      importedPath !== state.path &&
      (isStylesheetModule(importedPath) || isPreprocessorPath(importedPath))
    ) {
      addStyleDependent(state.styleDependents, importedPath, state.path);
    }
    await collectHtmlStyleContexts({
      ...state,
      path: importedPath,
      // Deduplicate so a cyclic import chain cannot grow the variant list and
      // mint a fresh visited key on every lap.
      variants: [...new Set([...state.variants, ...variants])],
      direct: false,
    });
  }
}

function inferredPreprocessorPath(
  state: Pick<
    HtmlContextState,
    "path" | "packageRoot" | "styleSources" | "pathOwners" | "styleDependents"
  >,
): string | undefined {
  const stem = basename(state.path, ".css");
  const matches = [...state.styleSources.keys()].filter(
    (path) =>
      isPreprocessorPath(path) &&
      state.pathOwners.get(path) === state.packageRoot &&
      !basename(path).startsWith("_") &&
      !state.styleDependents.has(path) &&
      basename(path, extname(path)) === stem,
  );
  return matches.length === 1 ? matches[0] : undefined;
}

function addInferredPreprocessorContext(state: HtmlContextState): boolean {
  // A source importing the generated CSS pins the artifact itself: excluding
  // it from planning while migrating the inferred entry could delete the only
  // source able to rebuild the file that import depends on.
  if (
    state.styleSources.has(state.path) &&
    state.sourceFiles.some(
      (file) => extname(file.path) !== ".html" && importsStylesheet(file, state.path),
    )
  ) {
    return false;
  }
  const path = inferredPreprocessorPath(state);
  if (!path) return false;
  state.generatedPaths.add(state.path);
  state.stylePaths.add(path);
  state.contexts.push({
    cssPath: path,
    variants: state.variants,
    direct: state.direct,
    analyzable: true,
  });
  state.warnings.push(
    htmlWarning(
      "inferred-preprocessor-source",
      state.path,
      0,
      0,
      `The linked CSS was matched to the unique preprocessor filename ${basename(path)}.`,
    ),
  );
  return true;
}

export function localHtmlReference(
  packageRoot: string,
  base: string,
  reference: string,
): string | undefined {
  const path = localHrefTarget(reference);
  if (path === undefined) return undefined;
  let decoded;
  try {
    decoded = decodeURIComponent(path);
  } catch {
    return undefined;
  }
  return decoded.startsWith("/") ? resolve(packageRoot, `.${decoded}`) : resolve(base, decoded);
}

function mediaVariants(media: string): string[] | undefined {
  const normalized = media.trim().toLowerCase();
  if (!normalized || normalized === "all") return [];
  if (normalized === "print") return ["print"];
  return undefined;
}

function deduplicateHtmlContexts(contexts: HtmlContext[]): HtmlContext[] {
  const unique = new Map<string, HtmlContext>();
  for (const context of contexts) {
    const key = `${context.cssPath}\0${context.variants.join(":")}\0${context.analyzable}`;
    const existing = unique.get(key);
    if (existing) existing.direct ||= context.direct;
    else unique.set(key, { ...context });
  }
  return [...unique.values()];
}

function htmlWarning(
  code: string,
  file: string,
  start: number,
  end: number,
  message: string,
): MigrationWarning {
  return { code, file, start, end, message };
}
