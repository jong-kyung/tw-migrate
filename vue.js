import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

// @vue/compiler-core node and element kinds; @vue/compiler-sfc does not
// re-export the enums, so the numeric values are pinned here.
const NODE_ELEMENT = 1;
const PROP_ATTRIBUTE = 6;
const PROP_DIRECTIVE = 7;
const TAG_ELEMENT = 0;
const TAG_COMPONENT = 1;
const TAG_SLOT = 2;
const TAG_TEMPLATE = 3;

// Resolve the target project's own Vue 3 compiler. Vue 2 resolves but is
// unsupported; a missing Vue installation is a recoverable package failure
// mirroring a missing Sass compiler.
export async function loadProjectVueCompiler(packageRoot) {
  const projectRequire = createRequire(join(packageRoot, "package.json"));
  let packagePath;
  try {
    packagePath = projectRequire.resolve("vue/package.json");
  } catch {
    throw new Error("Vue 3 with compiler-sfc must be installed in the target project.");
  }
  const { version } = JSON.parse(await readFile(packagePath, "utf8"));
  if (!String(version).startsWith("3.")) return { unsupportedVersion: String(version) };
  let modulePath;
  try {
    modulePath = projectRequire.resolve("vue/compiler-sfc");
  } catch {
    throw new Error("Vue 3 with compiler-sfc must be installed in the target project.");
  }
  const imported = await import(pathToFileURL(modulePath));
  return { compiler: imported.default ?? imported };
}

// Lower one SFC to the planner contract: plain-CSS scoped blocks in absolute
// byte offsets, literal template class sites for the HTML matching model,
// and the open-surface retention decision. Returns `retained: true` when the
// whole file must stay untouched.
export function analyzeVueSource(compiler, path, source) {
  const warnings = [];
  const warn = (code, start, end, message) =>
    warnings.push({ code, file: path, start, end, message });

  const { descriptor, errors } = compiler.parse(source, { filename: path });
  if (errors.length > 0) {
    const offset = errors[0]?.loc?.start?.offset ?? 0;
    warn(
      "unsupported-sfc-block",
      offset,
      offset,
      `The SFC could not be parsed: ${errors[0].message}`,
    );
    return { warnings: toByteWarnings(source, warnings), retained: true };
  }

  const template = descriptor.template;
  if (!template || template.lang || template.src) {
    const reason = !template
      ? "The SFC has no template block, so its class usage cannot be analyzed."
      : "Only the default template language with inline content is analyzed.";
    warn("unsupported-sfc-block", 0, 0, reason);
    return { warnings: toByteWarnings(source, warnings), retained: true };
  }

  const blocks = [];
  for (const style of descriptor.styles) {
    const start = style.loc.start.offset;
    const end = style.loc.end.offset;
    if (style.src !== undefined) {
      warn("unsupported-sfc-block", start, end, "A <style src> block is not analyzed.");
      continue;
    }
    if (style.module !== undefined) {
      warn("unsupported-sfc-block", start, end, "<style module> blocks are not supported yet.");
      continue;
    }
    if (style.lang && style.lang !== "css") {
      warn(
        "preprocessor-style-block",
        start,
        end,
        `A <style lang="${style.lang}"> block is not migrated yet.`,
      );
      continue;
    }
    if (!style.scoped) {
      warn(
        "unscoped-style-block",
        start,
        end,
        "A <style> block without `scoped` is global CSS and is retained.",
      );
      continue;
    }
    const outerStart = source.lastIndexOf("<style", start);
    const closing = source.indexOf("</style>", end);
    if (outerStart < 0 || closing < 0) {
      warn("unsupported-sfc-block", start, end, "The style block tags could not be located.");
      continue;
    }
    blocks.push({
      outerStart,
      outerEnd: closing + "</style>".length,
      contentStart: start,
      contentEnd: end,
    });
  }

  const state = { elements: [], dynamic: false, componentTags: false };
  visitTemplateNode(source, template.ast, state);
  const alwaysRenderedRoots = template.ast.children.filter(
    (node) =>
      node.type === NODE_ELEMENT &&
      (node.tagType === TAG_ELEMENT || node.tagType === TAG_COMPONENT) &&
      !node.props?.some(
        (prop) =>
          prop.type === PROP_DIRECTIVE && ["if", "else-if", "else", "for"].includes(prop.name),
      ),
  ).length;

  // Priority order: the most specific open surface names the retention.
  const retention = state.dynamic
    ? "dynamic-template-class"
    : state.componentTags
      ? "component-class-target"
      : alwaysRenderedRoots < 2
        ? "open-root-fallthrough"
        : undefined;

  const scriptText = [descriptor.script?.content, descriptor.scriptSetup?.content]
    .filter(Boolean)
    .join("\n");

  const offset = (index) => Buffer.byteLength(source.slice(0, index));
  const attribute = (value) =>
    value && { ...value, start: offset(value.start), end: offset(value.end) };
  return {
    warnings: toByteWarnings(source, warnings),
    blocks: blocks.map((block) => ({
      outerStart: offset(block.outerStart),
      outerEnd: offset(block.outerEnd),
      contentStart: offset(block.contentStart),
      contentEnd: offset(block.contentEnd),
    })),
    htmlElements: state.elements.map((element) => ({
      classAttribute: attribute(element.classAttribute),
      idAttribute: attribute(element.idAttribute),
    })),
    scriptText,
    retention,
    retained: false,
  };
}

// Post-plan integrity check: the edited SFC must still parse, and each
// remaining plain-CSS scoped block's contents are returned for CSS
// validation by the caller.
export function verifyVueSource(compiler, path, source) {
  const { descriptor, errors } = compiler.parse(source, { filename: path });
  if (errors.length > 0) {
    throw new Error(`Edited SFC no longer parses: ${path}: ${errors[0].message}`);
  }
  return descriptor.styles
    .filter(
      (style) =>
        style.scoped &&
        style.src === undefined &&
        style.module === undefined &&
        (!style.lang || style.lang === "css"),
    )
    .map((style) => style.content);
}

function visitTemplateNode(source, node, state) {
  if (node.type === NODE_ELEMENT) {
    let classBound = false;
    for (const prop of node.props ?? []) {
      if (prop.type !== PROP_DIRECTIVE) continue;
      // `:class`, or a spread `v-bind="..."`, can put any class anywhere.
      if (prop.name === "bind" && (!prop.arg || prop.arg.content === "class")) {
        state.dynamic = true;
        classBound = true;
      }
    }
    if (node.tagType === TAG_COMPONENT) {
      state.componentTags = true;
    } else if (node.tagType === TAG_ELEMENT) {
      const classAttribute = literalAttribute(source, node, "class");
      const idAttribute = literalAttribute(source, node, "id");
      const hasClassAttr = node.props?.some(
        (prop) => prop.type === PROP_ATTRIBUTE && prop.name === "class",
      );
      const hasIdAttr = node.props?.some(
        (prop) => prop.type === PROP_ATTRIBUTE && prop.name === "id",
      );
      // An attribute that exists but is not a safely writable quoted literal
      // makes the template's class set unprovable.
      if ((hasClassAttr && !classAttribute) || (hasIdAttr && !idAttribute)) {
        state.dynamic = true;
      } else if (!classBound && (classAttribute || idAttribute)) {
        let site = classAttribute;
        if (!site && idAttribute) {
          const insertion = classInsertionOffset(source, node);
          if (insertion === undefined) {
            state.dynamic = true;
          } else {
            site = { value: "", start: insertion, end: insertion, synthetic: true };
          }
        }
        if (site) state.elements.push({ classAttribute: site, idAttribute });
      }
    }
  }
  for (const child of node.children ?? []) visitTemplateNode(source, child, state);
}

// The inner span of a quoted, entity-free, literal attribute value, or
// undefined when the attribute is absent or cannot be edited safely.
function literalAttribute(source, node, name) {
  const prop = node.props?.find((prop) => prop.type === PROP_ATTRIBUTE && prop.name === name);
  if (!prop?.value) return undefined;
  const start = prop.value.loc.start.offset;
  const end = prop.value.loc.end.offset;
  const raw = source.slice(start, end);
  const quote = raw[0];
  if ((quote !== '"' && quote !== "'") || raw[raw.length - 1] !== quote) return undefined;
  const value = raw.slice(1, -1);
  if (value !== prop.value.content || value.includes("&")) return undefined;
  return { value, start: start + 1, end: end - 1 };
}

function classInsertionOffset(source, node) {
  const propsEnd = Math.max(
    node.loc.start.offset + 1 + node.tag.length,
    ...(node.props ?? []).map((prop) => prop.loc.end.offset),
  );
  const tagEnd = source.indexOf(">", propsEnd);
  if (tagEnd < 0) return undefined;
  let offset = tagEnd;
  while (offset > node.loc.start.offset && /\s/.test(source[offset - 1])) offset -= 1;
  if (source[offset - 1] === "/") offset -= 1;
  return offset;
}

function toByteWarnings(source, warnings) {
  const offset = (index) => Buffer.byteLength(source.slice(0, index));
  return warnings.map((warning) => ({
    ...warning,
    start: offset(warning.start),
    end: offset(warning.end),
  }));
}
