import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { join } from "node:path";

import { classInsertionOffset, offsetLookup, utf8OffsetMap } from "./html.ts";
import { loadProjectModule } from "./style-compiler.ts";
import { expressionAnalysis, sourceAnalysis, stylesheetAnalysis } from "../native.ts";
import type {
  ExpressionAnalysis,
  SourceAnalysis,
  SourceImportRecord,
  StaticImportBinding,
} from "../native.ts";
import type { MigrationWarning } from "../types.ts";
import type { SourceMapping } from "./style-compiler.ts";

const SUPPORTED_STYLE_ATTRIBUTES = new Set(["lang", "module", "scoped", "src"]);
const SUPPORTED_STYLE_LANGUAGES = new Set<string | undefined>([
  undefined,
  "css",
  "scss",
  "sass",
  "less",
]);
const SUPPORTED_SCRIPT_LANGUAGES = new Set<string | undefined>([
  undefined,
  "js",
  "jsx",
  "ts",
  "tsx",
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

interface LoadedVueCompiler {
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
  shadowSource?: string;
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

interface VueModuleBinding {
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

interface VueStyleImport {
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
export interface VueStyleRange {
  start: number;
  end: number;
  /** The block's declared style language, defaulting to css. */
  lang: string;
  /** The block's external source reference, when it loads one. */
  src?: string;
}

interface VueAnalysisBase {
  warnings: MigrationWarning[];
  styleRanges: VueStyleRange[];
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
      scriptImports: SourceImportRecord[];
      scriptImportsUnverifiable: boolean;
      scriptHasDynamicImport: boolean;
      scriptVueReferences: string[];
      scriptVueGlobPatterns: string[];
      scriptVueGlobUnverifiable: boolean;
      /// Static dynamic-class prefixes from template expressions and
      /// script blocks, for canonical spelling reservations.
      templatePrefixes: string[];
      /// True when a retained unsupported block keeps style content outside
      /// the analyzable block arrays, hiding possible global registrations.
      hasOpaqueStyleBlocks: boolean;
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
  moduleClosureBroken: boolean;
  referencesUseCssModule: boolean;
  /// Identifier names referenced from template expressions, matched after
  /// script analysis against helper aliases the script provides.
  expressionReferences: Set<string>;
  /// Static prefixes of dynamic class construction in template
  /// expressions, for canonical spelling reservations.
  templatePrefixes: Set<string>;
  /// Synthetic path for template expression parsing, carrying the SFC's
  /// script language so TypeScript assertions parse in TypeScript SFCs.
  expressionPath: string;
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
  const styleRanges = descriptor.styles.map((style) => ({
    start: style.loc.start.offset,
    end: style.loc.end.offset,
    lang: style.lang ?? "css",
    ...(style.src !== undefined ? { src: style.src } : {}),
  }));
  const byteRanges = (offset: (value: number) => number) => ({
    warnings: warnings.map((warning) => ({
      ...warning,
      start: offset(warning.start),
      end: offset(warning.end),
    })),
    styleRanges: styleRanges.map((range) => ({
      ...range,
      start: offset(range.start),
      end: offset(range.end),
    })),
  });
  const retained = (): VueAnalysis => {
    const offsets = utf8OffsetMap(
      source,
      [...warnings, ...styleRanges].flatMap((range) => [range.start, range.end]),
    );
    return { retained: true, ...byteRanges(offsetLookup(offsets)) };
  };
  if (errors.length > 0) {
    const offset = errors[0]?.loc?.start?.offset ?? 0;
    warn(
      "unsupported-sfc-block",
      offset,
      offset,
      `The SFC could not be parsed: ${errors[0].message}`,
    );
    if (errors[0]?.loc?.start?.offset === 0) {
      Object.assign(warnings[0], { line: 1, column: 1, endLine: 1, endColumn: 1 });
    }
    return retained();
  }

  const template = descriptor.template;
  if (!template || template.lang || template.src) {
    const reason = !template
      ? "The SFC has no template block, so its class usage cannot be analyzed."
      : "Only the default template language with inline content is analyzed.";
    warn("unsupported-sfc-block", 0, 0, reason);
    return retained();
  }
  if (descriptor.customBlocks.length > 0) {
    const block = descriptor.customBlocks[0];
    warn(
      "unsupported-sfc-block",
      block.loc.start.offset,
      block.loc.end.offset,
      `The custom <${block.type}> block is not analyzed.`,
    );
    return retained();
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
  let opaqueStyleBlocks = false;
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
      opaqueStyleBlocks = true;
      continue;
    }
    if (style.src !== undefined) {
      if (style.module !== undefined) {
        warn("unsupported-sfc-block", start, end, "A <style module src> block is not supported.");
        if (usesDefaultModuleBinding) moduleSiblingUnsupported = true;
        escapeUnverifiable = true;
        opaqueStyleBlocks = true;
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
      opaqueStyleBlocks = true;
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
      opaqueStyleBlocks = true;
      try {
        if (stylesheetAnalysis(`${path}.css`, style.content).selectorsUnverifiable) {
          escapeUnverifiable = true;
        }
      } catch {
        escapeUnverifiable = true;
      }
      continue;
    }
    // Vue exposes content bounds but not the outer tag bounds. Limit the
    // lookup to the compiler-proven block boundary so it only locates bytes.
    const outerStart = source.lastIndexOf("<style", start);
    const closing = source.slice(end).match(/^<\/style\s*>/)?.[0];
    if (outerStart < 0 || !closing) {
      warn("unsupported-sfc-block", start, end, "The style block tags could not be located.");
      escapeUnverifiable = true;
      opaqueStyleBlocks = true;
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
        try {
          if (stylesheetAnalysis(`${path}.${style.lang}`, style.content).selectorsUnverifiable) {
            escapeUnverifiable = true;
          }
        } catch {
          escapeUnverifiable = true;
        }
      } else {
        unscopedShadowCssTexts.push(style.content);
      }
      continue;
    }
    // Scope-escape selectors (`:deep`, `:global`, `:slotted`) reach elements
    // outside this SFC even from a scoped block, so parser-proven inner
    // selectors join the cascade-shadow corpus.
    try {
      const escapes = stylesheetAnalysis(`${path}.${style.lang ?? "css"}`, style.content);
      shadowCssTexts.push(...escapes.scopeEscapes);
      if (escapes.scopeEscapes.length > 0) {
        block.shadowSource = escapes.scopeShadowCss.join("\n") || "/* scope escapes extracted */";
      }
      if (escapes.scopeEscapesUnverifiable || escapes.selectorsUnverifiable) {
        escapeUnverifiable = true;
      }
    } catch {
      escapeUnverifiable = true;
    }
    blocks.push(block);
  }

  const scriptLang = descriptor.scriptSetup?.lang ?? descriptor.script?.lang;
  const state: TemplateState = {
    elements: [],
    components: [],
    dynamic: false,
    vHtml: false,
    hasSlot: false,
    moduleClosureBroken: false,
    referencesUseCssModule: false,
    expressionReferences: new Set(),
    templatePrefixes: new Set(),
    expressionPath: scriptLang === "ts" || scriptLang === "tsx" ? "Component.ts" : "Component.js",
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
  // A script whose imports cannot be read (external src, unsupported
  // language, parse failure) may still load global CSS invisible to the
  // shadow corpus, so its presence alone opens that surface.
  let scriptImportsUnverifiable = false;
  const scriptImports: SourceImportRecord[] = [];
  let setupAnalysis: SourceAnalysis | undefined;
  let scriptUsesCssModule = false;
  let scriptHasDynamicImport = false;
  const scriptVueReferences: string[] = [];
  const scriptVueGlobPatterns: string[] = [];
  const scriptTemplatePrefixes: string[] = [];
  let scriptVueGlobUnverifiable = false;
  const styleImports = [descriptor.script, descriptor.scriptSetup].flatMap((script) => {
    if (!script) return [];
    if (script.src || !SUPPORTED_SCRIPT_LANGUAGES.has(script.lang)) {
      scriptImportsUnverifiable = true;
      return [];
    }
    try {
      const analysis = sourceAnalysis(`${path}.${script.lang ?? "js"}`, script.content);
      if (script === descriptor.scriptSetup) setupAnalysis = analysis;
      if (analysis.usesCssModule || analysis.hasUnboundUseCssModule) scriptUsesCssModule = true;
      if (analysis.hasDynamicImport) scriptHasDynamicImport = true;
      scriptImports.push(...analysis.imports);
      scriptVueReferences.push(
        ...analysis.imports.filter((record) => !record.typeOnly).map((record) => record.specifier),
      );
      scriptVueGlobPatterns.push(...analysis.vueGlobPatterns);
      scriptTemplatePrefixes.push(...analysis.templatePrefixes);
      if (analysis.vueGlobUnverifiable) scriptVueGlobUnverifiable = true;
      return analysis.staticImports;
    } catch {
      scriptImportsUnverifiable = true;
      return [];
    }
  });
  const fallthroughUnverifiable =
    Boolean(descriptor.script) ||
    descriptor.scriptSetup?.src !== undefined ||
    (Boolean(descriptor.scriptSetup) && !setupAnalysis) ||
    setupAnalysis?.hasVueFallthroughMacro === true;
  const componentImports: StaticImportBinding[] = setupAnalysis?.defaultImports ?? [];

  const offsets = utf8OffsetMap(source, [
    ...warnings.flatMap((warning) => [warning.start, warning.end]),
    ...styleRanges.flatMap((range) => [range.start, range.end]),
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
  const offset = offsetLookup(offsets);
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
    ...byteRanges(offset),
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
      scriptUsesCssModule ||
      state.moduleClosureBroken ||
      // A template reference to `useCssModule` resolves to the Vue API
      // unless the setup script provides a local binding with that name;
      // Options API scripts never expose root constants to the template.
      (state.referencesUseCssModule && setupAnalysis?.definesRootUseCssModule !== true) ||
      // A template call through a script-provided helper alias, such as
      // `import { useCssModule as css } from "vue"`, is a Vue API call.
      (setupAnalysis?.useCssModuleLocals ?? []).some((name) =>
        state.expressionReferences.has(name),
      ),
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
    scriptImports,
    scriptImportsUnverifiable,
    scriptHasDynamicImport,
    scriptVueReferences,
    scriptVueGlobPatterns,
    scriptVueGlobUnverifiable,
    templatePrefixes: [...new Set([...state.templatePrefixes, ...scriptTemplatePrefixes])],
    hasOpaqueStyleBlocks: opaqueStyleBlocks,
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
      const expression = prop.exp?.content
        ? prop.name === "on"
          ? templateHandler(prop.exp.content)
          : templateExpression(state.expressionPath, prop.exp.content)
        : undefined;
      if (prop.exp?.content && !expression) state.moduleClosureBroken = true;
      let provenModuleExpression = false;
      if (prop.name === "bind") {
        if (!prop.arg || !prop.arg.isStatic) {
          classOpaque = true;
        } else if (prop.arg.content === "class") {
          const member = node.tagType === TAG_ELEMENT ? expression?.vueModuleMember : undefined;
          if (member && !moduleBinding) {
            // A proven `$style` member yields a hashed class that literal
            // and scoped analysis never see, so it is not an opaque surface.
            moduleBinding = {
              name: member,
              start: attributeRemovalStart(source, node, prop),
              end: prop.loc.end.offset,
            };
            provenModuleExpression = true;
          } else {
            const value = expression?.staticString ?? undefined;
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
      if (!provenModuleExpression && expression?.usesCssModule) {
        state.moduleClosureBroken = true;
      }
      if (expression?.referencesUseCssModule) state.referencesUseCssModule = true;
      for (const name of expression?.references ?? []) state.expressionReferences.add(name);
      for (const prefix of expression?.templatePrefixes ?? []) state.templatePrefixes.add(prefix);
      if (prop.arg && !prop.arg.isStatic && prop.arg.content) {
        const argExpression = templateExpression(state.expressionPath, prop.arg.content);
        state.moduleClosureBroken ||= argExpression?.usesCssModule ?? true;
        state.referencesUseCssModule ||= argExpression?.referencesUseCssModule ?? false;
        for (const name of argExpression?.references ?? []) state.expressionReferences.add(name);
        for (const prefix of argExpression?.templatePrefixes ?? []) {
          state.templatePrefixes.add(prefix);
        }
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
    const interpolation = templateExpression(state.expressionPath, node.content.content);
    state.moduleClosureBroken ||= interpolation?.usesCssModule ?? true;
    state.referencesUseCssModule ||= interpolation?.referencesUseCssModule ?? false;
    for (const name of interpolation?.references ?? []) state.expressionReferences.add(name);
    for (const prefix of interpolation?.templatePrefixes ?? []) state.templatePrefixes.add(prefix);
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

function templateExpression(path: string, source: string): ExpressionAnalysis | undefined {
  try {
    return expressionAnalysis(path, source);
  } catch {
    return undefined;
  }
}

function templateHandler(source: string): ExpressionAnalysis | undefined {
  try {
    const analysis = sourceAnalysis(
      "Component.handler.ts",
      `async function handler($event) { ${source} }`,
    );
    return {
      staticString: null,
      vueModuleMember: null,
      usesCssModule: analysis.usesCssModule,
      // Handler sources carry no bindings of their own, so an unbound
      // reference is a potential Vue API call the script may not shadow.
      referencesUseCssModule: analysis.hasUnboundUseCssModule,
      references: analysis.unboundReferences,
      templatePrefixes: analysis.templatePrefixes,
    };
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
    const insertion = nodeClassInsertionOffset(source, node);
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

function nodeClassInsertionOffset(source: string, node: TemplateNode): number | undefined {
  const propsEnd = Math.max(
    node.loc.start.offset + 1 + node.tag.length,
    ...(node.props ?? []).map((prop) => prop.loc.end.offset),
  );
  const tagEnd = source.indexOf(">", propsEnd);
  if (tagEnd < 0) return undefined;
  return classInsertionOffset(source, node.loc.start.offset, tagEnd);
}
