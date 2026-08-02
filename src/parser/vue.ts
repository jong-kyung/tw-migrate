import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { join } from "node:path";

import { utf8OffsetMap } from "./html.ts";
import { loadProjectModule } from "./style-compiler.ts";
import { staticImportBindings, staticImports, staticStringExpression } from "../native.ts";
import type { StaticImportBinding } from "../native.ts";
import type { SourceMapping } from "./style-compiler.ts";
import type { MigrationWarning } from "../types.ts";

const ESCAPE_SELECTOR = /(?:::v-|:)(?:deep|global|slotted)\(([^)]*)\)/g;
const ESCAPE_RESIDUE = /(?:>>>|\/deep\/|::v-deep|:deep|::v-slotted|:slotted|::v-global|:global)/;
const SUPPORTED_STYLE_ATTRIBUTES = new Set(["lang", "module", "scoped", "src"]);
const SUPPORTED_STYLE_LANGUAGES = new Set<string | undefined>([
  undefined,
  "css",
  "scss",
  "sass",
  "less",
]);

// @vue/compiler-core node and element kinds; @vue/compiler-sfc does not
// re-export the enums, so the numeric values are pinned here.
const NODE_ELEMENT = 1;
const NODE_TEXT = 2;
const NODE_INTERPOLATION = 5;
const PROP_ATTRIBUTE = 6;
const PROP_DIRECTIVE = 7;
const TAG_ELEMENT = 0;
const TAG_COMPONENT = 1;
const TAG_SLOT = 2;

// Minimal structural view of the target project's @vue/compiler-sfc output;
// the analysis duck-types the descriptor and template AST the same way the
// untyped implementation did.
interface SfcPosition {
  offset: number;
}

interface SfcLoc {
  start: SfcPosition;
  end: SfcPosition;
}

interface TemplateExpression {
  content: string;
  isStatic?: boolean;
}

interface TemplateContent {
  content?: string;
  trim?: () => string;
}

interface TemplateProp {
  type: number;
  name: string;
  loc: SfcLoc;
  arg?: TemplateExpression;
  exp?: TemplateExpression;
  value?: { content: string; loc: SfcLoc };
}

interface TemplateNode {
  type: number;
  tag: string;
  tagType: number;
  props?: TemplateProp[];
  children?: TemplateNode[];
  loc: SfcLoc;
  content?: TemplateContent;
}

interface TemplateRoot extends TemplateNode {
  children: TemplateNode[];
}

interface SfcStyleBlock {
  content: string;
  loc: SfcLoc;
  attrs: Record<string, string | true>;
  lang?: string;
  src?: string;
  module?: string | boolean;
  scoped?: boolean;
}

interface SfcScriptBlock {
  content: string;
  lang?: string;
  src?: string;
}

interface SfcCustomBlock {
  type: string;
  loc: SfcLoc;
}

interface SfcDescriptor {
  template?: { lang?: string; src?: string; ast: TemplateRoot } | null;
  styles: SfcStyleBlock[];
  customBlocks: SfcCustomBlock[];
  script?: SfcScriptBlock | null;
  scriptSetup?: SfcScriptBlock | null;
}

interface SfcParseError {
  message: string;
  loc?: { start?: { offset?: number } };
}

export interface VueCompiler {
  parse: (
    source: string,
    options: { filename: string },
  ) => { descriptor: SfcDescriptor; errors: SfcParseError[] };
}

export interface LoadedVueCompiler {
  compiler?: VueCompiler;
  unsupportedVersion?: string;
}

export interface VueStyleBlock {
  outerStart: number;
  outerEnd: number;
  contentStart: number;
  contentEnd: number;
  syntax: string;
  content: string;
  // Populated by the migration orchestrator when preprocessor block content
  // is compiled for planner analysis.
  analysisSource?: string;
  sourcePath?: string;
  sourceMappings?: SourceMapping[];
}

export interface VueTemplateAttribute {
  value: string;
  start: number;
  end: number;
  synthetic?: boolean;
  // Cleared by the migration orchestrator on shadow-only sites that must
  // never be rewritten or counted as a reference.
  writable?: boolean;
}

export interface VueModuleBinding {
  name: string;
  start: number;
  end: number;
}

export interface VueTemplateSite {
  tag: string;
  nodeStart: number;
  classAttribute?: VueTemplateAttribute;
  idAttribute?: VueTemplateAttribute;
  matchClasses?: string[];
  moduleBinding?: VueModuleBinding;
}

export interface VueStyleImport {
  reference: string;
  start: number;
  end: number;
}

export interface VueComponentEdge {
  parent: string;
  child: string;
  site: VueTemplateSite;
}

// The orchestrator-owned fields are populated by index.ts while building the
// package component graph, after analysis produced the object.
interface VueAnalysisBase {
  warnings: MigrationWarning[];
  resolvedComponents?: VueComponentEdge[];
  componentsOpen?: boolean;
  setupImports?: Set<string>;
  rootIdsOverridden?: boolean;
}

export type VueAnalysis =
  | (VueAnalysisBase & { retained: true })
  | (VueAnalysisBase & {
      retained: false;
      blocks: VueStyleBlock[];
      unscopedBlocks: VueStyleBlock[];
      moduleBlocks: VueStyleBlock[];
      moduleClosureBroken: boolean;
      htmlElements: VueTemplateSite[];
      componentSites: VueTemplateSite[];
      rootStarts: (number | undefined)[];
      rootVFor: boolean;
      rootFragment: boolean;
      scriptText: string;
      scriptStyleImports: string[];
      scriptImportsUnverifiable: boolean;
      styleBlockImports: VueStyleImport[];
      componentImports: StaticImportBinding[];
      fallthroughUnverifiable: boolean;
      dynamic: boolean;
      vHtml: boolean;
      hasSlot: boolean;
      alwaysRenderedRoots: number;
      shadowCssTexts: string[];
      unscopedShadowCssTexts: string[];
      shadowModuleCssTexts: string[];
      shadowPreprocessorTexts: string[];
      unscopedShadowPreprocessorTexts: string[];
      escapeUnverifiable: boolean;
    });

interface TemplateSiteAttributes {
  classAttribute?: VueTemplateAttribute;
  idAttribute?: VueTemplateAttribute;
  matchClasses?: string[];
}

interface TemplateState {
  elements: VueTemplateSite[];
  components: VueTemplateSite[];
  dynamic: boolean;
  vHtml: boolean;
  hasSlot: boolean;
  expressionTexts: string[];
}

// Resolve the target project's own Vue 3 compiler. Vue 2 resolves but is
// unsupported; a missing Vue installation is a recoverable package failure
// mirroring a missing Sass compiler.
export async function loadProjectVueCompiler(packageRoot: string): Promise<LoadedVueCompiler> {
  const projectRequire = createRequire(join(packageRoot, "package.json"));
  let packagePath;
  try {
    packagePath = projectRequire.resolve("vue/package.json");
  } catch {
    throw new Error("Vue 3 with compiler-sfc must be installed in the target project.");
  }
  const { version } = JSON.parse(await readFile(packagePath, "utf8"));
  if (!String(version).startsWith("3.")) return { unsupportedVersion: String(version) };
  return {
    compiler: await loadProjectModule<VueCompiler>(
      packageRoot,
      "vue/compiler-sfc",
      "Vue 3 with compiler-sfc must be installed in the target project.",
    ),
  };
}

// Lower one SFC to the planner contract: plain-CSS scoped blocks in absolute
// byte offsets and literal template class sites for the HTML matching model.
// Returns `retained: true` when the whole file must stay untouched.
export function analyzeVueSource(compiler: VueCompiler, path: string, source: string): VueAnalysis {
  const warnings: MigrationWarning[] = [];
  const warn = (code: string, start: number, end: number, message: string): number =>
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
  if (descriptor.customBlocks.length > 0) {
    const block = descriptor.customBlocks[0];
    warn(
      "unsupported-sfc-block",
      block.loc.start.offset,
      block.loc.end.offset,
      `The custom <${block.type}> block is not analyzed.`,
    );
    return { warnings: toByteWarnings(source, warnings), retained: true };
  }
  const blocks: VueStyleBlock[] = [];
  const unscopedBlocks: VueStyleBlock[] = [];
  const moduleBlocks: VueStyleBlock[] = [];
  const styleBlockImports: VueStyleImport[] = [];
  // Retained blocks are still real CSS that can win the cascade against the
  // utilities replacing a deleted scoped rule. Plain-CSS text feeds the
  // planner's parsed shadow index; preprocessor text is screened by the
  // caller before joining it.
  const shadowCssTexts: string[] = [];
  const unscopedShadowCssTexts: string[] = [];
  const shadowModuleCssTexts: string[] = [];
  const shadowPreprocessorTexts: string[] = [];
  const unscopedShadowPreprocessorTexts: string[] = [];
  let escapeUnverifiable = false;
  let moduleSiblingUnsupported = false;
  for (const style of descriptor.styles) {
    const start = style.loc.start.offset;
    const end = style.loc.end.offset;
    const usesDefaultModuleBinding = style.module === true || style.module === "$style";
    const unsupportedAttributes = Object.keys(style.attrs).filter(
      (attribute) => !SUPPORTED_STYLE_ATTRIBUTES.has(attribute),
    );
    if (unsupportedAttributes.length > 0) {
      warn(
        "unsupported-sfc-block",
        start,
        end,
        `Unsupported <style> attributes: ${unsupportedAttributes.join(", ")}.`,
      );
      if (usesDefaultModuleBinding) moduleSiblingUnsupported = true;
      escapeUnverifiable = true;
      continue;
    }
    if (style.src !== undefined) {
      if (style.module !== undefined) {
        warn("unsupported-sfc-block", start, end, "A <style module src> block is not supported.");
        if (usesDefaultModuleBinding) moduleSiblingUnsupported = true;
        escapeUnverifiable = true;
      } else {
        styleBlockImports.push({ reference: style.src, start, end });
      }
      continue;
    }
    if (style.module !== undefined && style.module !== true) {
      // `$style` explicitly names the same binding as a bare module, so this
      // unsupported sibling must keep every block feeding that object alive.
      if (usesDefaultModuleBinding) moduleSiblingUnsupported = true;
      // Named module bindings are rare and unproven; the block still feeds
      // the module shadow channel (type selectors stay global).
      warn("unsupported-sfc-block", start, end, "Named <style module> blocks are not supported.");
      shadowModuleCssTexts.push(style.content);
      continue;
    }
    if (!SUPPORTED_STYLE_LANGUAGES.has(style.lang)) {
      // Every unnamed module block feeds the same `$style` object, so an
      // unsupported sibling supplies classes the closure cannot see; the
      // whole module must retain.
      if (usesDefaultModuleBinding) moduleSiblingUnsupported = true;
      warn(
        "preprocessor-style-block",
        start,
        end,
        `The <style lang="${style.lang}"> language is not supported.`,
      );
      shadowPreprocessorTexts.push(style.content);
      continue;
    }
    const outerStart = source.lastIndexOf("<style", start);
    const closing = source.slice(end).match(/^<\/style\s*>/)?.[0];
    if (outerStart < 0 || !closing) {
      warn("unsupported-sfc-block", start, end, "The style block tags could not be located.");
      escapeUnverifiable = true;
      continue;
    }
    const block: VueStyleBlock = {
      outerStart,
      outerEnd: end + closing.length,
      contentStart: start,
      contentEnd: end,
      syntax: style.lang ?? "css",
      content: style.content,
    };
    if (style.module === true) {
      // Module class and id names are localized at build time, but type and
      // attribute selectors (and `:global` escapes) stay global. The caller
      // feeds these blocks into the module shadow channel after compiling
      // preprocessor content, so they are not pushed as raw text here.
      moduleBlocks.push(block);
      continue;
    }
    if (!style.scoped) {
      unscopedBlocks.push(block);
      if (style.lang && style.lang !== "css") {
        unscopedShadowPreprocessorTexts.push(style.content);
      } else {
        unscopedShadowCssTexts.push(style.content);
      }
      continue;
    }
    // Scope-escape selectors (`:deep`, `:global`, `:slotted`) reach elements
    // outside this SFC even from a scoped block, so their inner selectors
    // must join the cascade-shadow corpus. Nested or paren-less escape forms
    // cannot be extracted textually and make the corpus unverifiable.
    for (const [, inner] of style.content.matchAll(ESCAPE_SELECTOR)) {
      shadowCssTexts.push(`${inner} {}`);
      if (inner.includes("(")) escapeUnverifiable = true;
    }
    if (ESCAPE_RESIDUE.test(style.content.replace(ESCAPE_SELECTOR, " "))) {
      escapeUnverifiable = true;
    }
    blocks.push(block);
  }

  const state: TemplateState = {
    elements: [],
    components: [],
    dynamic: false,
    vHtml: false,
    hasSlot: false,
    expressionTexts: [],
  };
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

  // Script contents are outside template-class analysis, but the raw text
  // still protects external CSS Modules from unsafe deletion.
  const scriptText = [descriptor.script?.content, descriptor.scriptSetup?.content]
    .filter(Boolean)
    .join("\n");
  const fallthroughUnverifiable =
    Boolean(descriptor.script) ||
    descriptor.scriptSetup?.src !== undefined ||
    /\b(?:defineOptions|defineProps|inheritAttrs)\b/.test(descriptor.scriptSetup?.content ?? "");
  // A script whose imports cannot be read (external src, unsupported
  // language, parse failure) may still load global CSS invisible to the
  // shadow corpus, so its presence alone opens that surface.
  let scriptImportsUnverifiable = false;
  const styleImports = [descriptor.script, descriptor.scriptSetup].flatMap((script) => {
    if (!script) return [];
    if (script.src || ![undefined, "js", "jsx", "ts", "tsx"].includes(script.lang)) {
      scriptImportsUnverifiable = true;
      return [];
    }
    try {
      return staticImports(`${path}.${script.lang ?? "js"}`, script.content);
    } catch {
      scriptImportsUnverifiable = true;
      return [];
    }
  });

  let componentImports: StaticImportBinding[] = [];
  if (
    descriptor.scriptSetup &&
    !descriptor.scriptSetup.src &&
    [undefined, "js", "jsx", "ts", "tsx"].includes(descriptor.scriptSetup.lang)
  ) {
    try {
      componentImports = staticImportBindings(
        `${path}.${descriptor.scriptSetup.lang ?? "js"}`,
        descriptor.scriptSetup.content,
      );
    } catch {
      componentImports = [];
    }
  }

  const offsets = utf8OffsetMap(source, [
    ...warnings.flatMap((warning) => [warning.start, warning.end]),
    ...[...blocks, ...unscopedBlocks, ...moduleBlocks].flatMap((block) => [
      block.outerStart,
      block.outerEnd,
      block.contentStart,
      block.contentEnd,
    ]),
    ...styleBlockImports.flatMap((entry) => [entry.start, entry.end]),
    ...[...state.elements, ...state.components].flatMap((element) =>
      [
        element.nodeStart,
        element.classAttribute,
        element.idAttribute,
        element.moduleBinding,
      ].flatMap((value) =>
        value === undefined ? [] : typeof value === "number" ? [value] : [value.start, value.end],
      ),
    ),
  ]);
  // Every queried index was fed into the map above, so a miss is a bug in
  // this module rather than a recoverable input condition.
  const offset = (index: number): number => {
    const byte = offsets.get(index);
    if (byte === undefined) throw new Error(`No byte offset was mapped for index ${index}`);
    return byte;
  };
  const attribute = (value: VueTemplateAttribute | undefined): VueTemplateAttribute | undefined =>
    value && { ...value, start: offset(value.start), end: offset(value.end) };
  const toByteBlock = (block: VueStyleBlock): VueStyleBlock => ({
    ...block,
    outerStart: offset(block.outerStart),
    outerEnd: offset(block.outerEnd),
    contentStart: offset(block.contentStart),
    contentEnd: offset(block.contentEnd),
  });
  const toByteSite = (element: VueTemplateSite): VueTemplateSite => ({
    tag: element.tag,
    nodeStart: offset(element.nodeStart),
    classAttribute: attribute(element.classAttribute),
    idAttribute: attribute(element.idAttribute),
    matchClasses: element.matchClasses,
    moduleBinding: element.moduleBinding && {
      name: element.moduleBinding.name,
      start: offset(element.moduleBinding.start),
      end: offset(element.moduleBinding.end),
    },
  });
  return {
    warnings: warnings.map((warning) => ({
      ...warning,
      start: offset(warning.start),
      end: offset(warning.end),
    })),
    blocks: blocks.map(toByteBlock),
    unscopedBlocks: unscopedBlocks.map(toByteBlock),
    moduleBlocks: moduleBlocks.map(toByteBlock),
    // `$style` outside the proven direct member sites (any expression,
    // interpolation, or script text) makes the module's consumers
    // unprovable.
    moduleClosureBroken:
      moduleSiblingUnsupported ||
      // An unreadable script could reference `$style` invisibly.
      scriptImportsUnverifiable ||
      /\$style|useCssModule/.test([...state.expressionTexts, scriptText].join("\n")),
    htmlElements: state.elements.map(toByteSite),
    componentSites: state.components.map(toByteSite),
    // Slot and template roots are element nodes without a template site, so
    // their starts were never fed into the offset map and stay undefined;
    // nodeStart comparisons in the caller then never match them.
    rootStarts: template.ast.children
      .filter((node) => node.type === NODE_ELEMENT)
      .map((node) => offsets.get(node.loc.start.offset)),
    // A root-level `v-for` renders a fragment, so a lone AST root is not a
    // fallthrough-eligible single root.
    rootVFor: template.ast.children.some(
      (node) =>
        node.type === NODE_ELEMENT &&
        node.props?.some((prop) => prop.type === PROP_DIRECTIVE && prop.name === "for"),
    ),
    // Non-comment text or interpolation roots also fragment the render
    // output, defeating attribute fallthrough.
    rootFragment:
      template.ast.children.filter(
        (node) =>
          node.type === NODE_ELEMENT ||
          node.type === NODE_INTERPOLATION ||
          (node.type === NODE_TEXT && node.content?.trim?.() !== ""),
      ).length !== 1,
    scriptText,
    // Script imports follow module-specifier semantics (bare = package);
    // style-block `@import`s follow CSS semantics (bare = relative). They
    // must resolve differently, so they stay separate.
    scriptStyleImports: [...new Set(styleImports)],
    scriptImportsUnverifiable,
    styleBlockImports: styleBlockImports.map((entry) => ({
      ...entry,
      start: offset(entry.start),
      end: offset(entry.end),
    })),
    componentImports,
    fallthroughUnverifiable,
    dynamic: state.dynamic,
    vHtml: state.vHtml,
    hasSlot: state.hasSlot,
    alwaysRenderedRoots,
    shadowCssTexts,
    unscopedShadowCssTexts,
    shadowModuleCssTexts,
    shadowPreprocessorTexts,
    unscopedShadowPreprocessorTexts,
    escapeUnverifiable,
    retained: false,
  };
}

// Post-plan integrity check: the edited SFC must still parse, and each
// remaining supported scoped block contents are returned for validation by
// the caller.
export function verifyVueSource(
  compiler: VueCompiler,
  path: string,
  source: string,
  includeUnscoped = false,
): { content: string; syntax: string }[] {
  const { descriptor, errors } = compiler.parse(source, { filename: path });
  if (errors.length > 0) {
    throw new Error(`Edited SFC no longer parses: ${path}: ${errors[0].message}`);
  }
  return descriptor.styles
    .filter(
      (style) =>
        style.src === undefined &&
        (style.module === undefined || style.module === true) &&
        (style.scoped || style.module === true || includeUnscoped) &&
        SUPPORTED_STYLE_LANGUAGES.has(style.lang),
    )
    .map((style) => ({ content: style.content, syntax: style.lang ?? "css" }));
}

function visitTemplateNode(source: string, node: TemplateNode, state: TemplateState): void {
  if (node.type === NODE_ELEMENT) {
    if (node.tagType === TAG_SLOT) state.hasSlot = true;
    const bindingClasses: string[] = [];
    let classOpaque = false;
    let moduleBinding: VueModuleBinding | undefined;
    for (const prop of node.props ?? []) {
      if (prop.type !== PROP_DIRECTIVE) continue;
      let provenModuleExpression = false;
      if (prop.name === "bind") {
        if (!prop.arg || !prop.arg.isStatic) {
          classOpaque = true;
        } else if (prop.arg.content === "class") {
          const member =
            node.tagType === TAG_ELEMENT && prop.exp
              ? /^\s*\$style\s*\.\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*$/.exec(prop.exp.content)
              : undefined;
          if (member && !moduleBinding) {
            // A proven `$style` member yields a hashed class that literal
            // and scoped analysis never see, so it is not an opaque surface.
            moduleBinding = {
              name: member[1],
              start: attributeRemovalStart(source, node, prop),
              end: prop.loc.end.offset,
            };
            provenModuleExpression = true;
          } else {
            const value = staticClassBinding(prop);
            if (value === undefined) classOpaque = true;
            else bindingClasses.push(...value.split(/[\t\n\f\r ]+/).filter(Boolean));
          }
        } else if (prop.arg.content === "id") {
          classOpaque = true;
        }
      }
      // Every unproven expression joins the module closure scan: `$style`
      // escaping the proven form anywhere retains the module. A dynamic
      // directive argument (`v-bind:[expr]`, `v-on:[expr]`) evaluates its
      // expression at render time, so it joins the scan too.
      if (!provenModuleExpression && prop.exp?.content) {
        state.expressionTexts.push(prop.exp.content);
      }
      if (prop.arg && !prop.arg.isStatic && prop.arg.content) {
        state.expressionTexts.push(prop.arg.content);
      }
      // Injected markup carries no scope attribute, so scoped proofs are
      // unaffected -- but it can use any class an unscoped rule targets.
      if (prop.name === "html") state.vHtml = true;
    }
    if (classOpaque) state.dynamic = true;
    const site = templateSite(source, node, bindingClasses, classOpaque, state);
    const element = { tag: node.tag, nodeStart: node.loc.start.offset, moduleBinding, ...site };
    if (node.tagType === TAG_COMPONENT) state.components.push(element);
    else if (node.tagType === TAG_ELEMENT) state.elements.push(element);
  }
  if (node.type === NODE_INTERPOLATION && node.content?.content) {
    state.expressionTexts.push(node.content.content);
  }
  for (const child of node.children ?? []) visitTemplateNode(source, child, state);
}

// Inline attributes consume their separator so deletion leaves no double
// space. Multiline attributes keep their newline and indentation byte-exact.
function attributeRemovalStart(source: string, node: TemplateNode, prop: TemplateProp): number {
  const attributeStart = prop.loc.start.offset;
  let start = attributeStart;
  while (start > node.loc.start.offset + 1 && /\s/.test(source[start - 1])) start -= 1;
  return /[\r\n]/.test(source.slice(start, attributeStart)) ? attributeStart : start;
}

function staticClassBinding(prop: TemplateProp): string | undefined {
  if (!prop.exp) return undefined;
  try {
    return staticStringExpression("Component.js", prop.exp.content) ?? undefined;
  } catch {
    return undefined;
  }
}

function templateSite(
  source: string,
  node: TemplateNode,
  bindingClasses: string[],
  classOpaque: boolean,
  state: TemplateState,
): TemplateSiteAttributes {
  let classAttribute = literalAttribute(source, node, "class");
  const idAttribute = literalAttribute(source, node, "id");
  const hasClassAttr = node.props?.some(
    (prop) => prop.type === PROP_ATTRIBUTE && prop.name === "class",
  );
  const hasIdAttr = node.props?.some((prop) => prop.type === PROP_ATTRIBUTE && prop.name === "id");
  if ((hasClassAttr && !classAttribute) || (hasIdAttr && !idAttribute)) {
    state.dynamic = true;
    return { idAttribute };
  }

  const matchClasses = bindingClasses.length
    ? [...(classAttribute?.value.split(/[\t\n\f\r ]+/).filter(Boolean) ?? []), ...bindingClasses]
    : undefined;
  if (
    !classAttribute &&
    (bindingClasses.length > 0 || (!classOpaque && (node.tagType === TAG_COMPONENT || idAttribute)))
  ) {
    const insertion = classInsertionOffset(source, node);
    if (insertion === undefined) state.dynamic = true;
    else classAttribute = { value: "", start: insertion, end: insertion, synthetic: true };
  }
  return { classAttribute, idAttribute, matchClasses };
}

// The inner span of a quoted, entity-free, literal attribute value, or
// undefined when the attribute is absent or cannot be edited safely.
function literalAttribute(
  source: string,
  node: TemplateNode,
  name: string,
): VueTemplateAttribute | undefined {
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

function classInsertionOffset(source: string, node: TemplateNode): number | undefined {
  const propsEnd = Math.max(
    node.loc.start.offset + 1 + node.tag.length,
    ...(node.props ?? []).map((prop) => prop.loc.end.offset),
  );
  const tagEnd = source.indexOf(">", propsEnd);
  if (tagEnd < 0) return undefined;
  let offset = tagEnd;
  if (source[offset - 1] === "/") offset -= 1;
  while (offset > node.loc.start.offset && /\s/.test(source[offset - 1])) offset -= 1;
  return offset;
}

function toByteWarnings(source: string, warnings: MigrationWarning[]): MigrationWarning[] {
  const offsets = utf8OffsetMap(
    source,
    warnings.flatMap((warning) => [warning.start, warning.end]),
  );
  return warnings.map((warning) => ({
    ...warning,
    start: offsets.get(warning.start) ?? warning.start,
    end: offsets.get(warning.end) ?? warning.end,
  }));
}
