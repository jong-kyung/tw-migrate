import type { HtmlElementAttributes } from "./parser/html.ts";
import type { SourceMapping } from "./parser/style-compiler.ts";
import type { VueCompiler, VueStyleBlock, VueTemplateSite } from "./parser/vue.ts";

export interface MigrateOptions {
  styleFile?: string;
  cwd?: string;
  write?: boolean;
  tailwindCss?: string;
  workspaces?: boolean;
  force?: boolean;
  /** Extract unmatched media conditions into named `@custom-variant`
   * definitions in the Tailwind entry and rewrite consumers to stacked
   * variants. Disabled unless explicitly `true`; the CLI flag and
   * default-on behavior land together with packaged snapshot coverage. */
  extractMediaQueries?: boolean;
}

export interface MigrationWarning {
  code: string;
  file: string;
  /** Byte offsets into the authored file, or (0, 0) when no unique mapping exists. */
  start: number;
  end: number;
  /** 1-based UTF-16 positions, omitted when no unique authored mapping exists. */
  line?: number;
  column?: number;
  endLine?: number;
  endColumn?: number;
  message: string;
}

export interface RuleReport {
  selector: string;
  status: "converted" | "retained";
  candidates: string[];
  file: string;
  /** Rule span in the analysis source (compiled CSS for preprocessor stylesheets). */
  ruleId: RuleSpan;
  /** Rule span in the authored file, or (0, 0) when no unique mapping exists. */
  authoredSpan: RuleSpan;
}

export interface MigrationFailure {
  package: string;
  message: string;
}

export interface MigrationReport {
  changedFiles: string[];
  diff: string;
  convertedRules: number;
  retainedRules: number;
  rules: RuleReport[];
  candidates: string[];
  warnings: MigrationWarning[];
  failures: MigrationFailure[];
}

export interface SourceFile {
  path: string;
  source: string;
}

export interface HtmlContext {
  cssPath: string;
  variants: string[];
  direct: boolean;
  analyzable: boolean;
}

export interface VuePlannedElement extends VueTemplateSite {
  cssPaths: string[];
  matchIds?: string[];
  matchTag?: string;
}

export interface PreparedSourceFile extends SourceFile {
  htmlElements?: (HtmlElementAttributes | VuePlannedElement)[];
  htmlStylesheets?: HtmlContext[];
  htmlReferencesSafe?: boolean;
  htmlScriptText?: string;
}

export interface PlannedFile extends PreparedSourceFile {
  writable: boolean;
}

export interface RuleSpan {
  start: number;
  end: number;
}

export interface StylesheetEntry {
  cssPath: string;
  cssSource: string;
  cssModuleId: string;
  cssDependents?: string[];
  syntax?: string;
  isModule: boolean;
  isPartial?: boolean;
  analysisSource?: string;
  sourceMappings?: SourceMapping[];
  vueBlocks?: VueStyleBlock[];
  vueModule?: boolean;
  vueRetention?: string;
  vueUnscoped?: boolean;
  vueShadowCss?: string[];
  vueShadowModuleCss?: string[];
  vueShadowUnverifiable?: boolean;
  blockedRules?: RuleSpan[];
}

export interface PlannerRequest {
  stylesheets: StylesheetEntry[];
  tailwindPath: string;
  tailwindSource: string;
  utilityPrefix: string | null;
  themeTokens: Record<string, string>;
  /** The entry group's fixed media key-to-name map. Omit while extraction
   * is disabled, in which case media handling is unchanged; an explicitly
   * supplied empty map stays authoritative and sends every condition to
   * the arbitrary fallback. */
  mediaNames?: Record<string, string>;
  /** False when the resolved Tailwind entry must not be edited: the planner
   * disables keyframe and global at-rule movement and returns no entry
   * file. Omitted means writable. */
  entryWritable?: boolean;
  files: PlannedFile[];
}

// `stylesheet` is the planner's compile-failure attribution index into the
// request stylesheets; it is stripped from the public RuleReport.
export interface PlanRule extends RuleReport {
  stylesheet: number;
}

export interface Plan {
  files: SourceFile[];
  deletedFiles: string[];
  unlinkedFiles: string[];
  candidates: string[];
  rules: PlanRule[];
  warnings: MigrationWarning[];
  convertedRules: number;
  retainedRules: number;
}

export interface PlanResult {
  failure?: MigrationFailure;
  plan?: Plan;
}

export interface Scope {
  cwd: string;
  workspaceRoot: string;
  scannedPaths: string[];
  targetable: Set<string>;
  explicitStyle?: string;
  configuredEntry?: string;
  pathOwners: Map<string, string | undefined>;
  selectedPackages: string[];
}

export interface MigrationContext extends Scope {
  options: MigrateOptions;
  snapshots: Map<string, string>;
  styleSources: Map<string, string>;
  sourceFiles: SourceFile[];
  styleDependents: Map<string, string[]>;
  vueStyleRanges: Map<string, RuleSpan[]>;
  /** Tailwind entries per owning package, for ancestor-shared resolution. */
  entryCatalog: Map<string, string[]>;
  /** Scanned paths the utility scanner's ignore rules exclude. */
  ignoredPaths: Set<string>;
}

export interface RemovableLink {
  filePath: string;
  cssPath: string;
  href: string;
  media: string;
}

export interface PreparedHtml {
  files: PreparedSourceFile[];
  stylePaths: Set<string>;
  generatedPaths: Set<string>;
  removableLinks: RemovableLink[];
  warnings: MigrationWarning[];
}

export interface PreparedVue {
  files: Map<string, PreparedSourceFile>;
  stylesheets: StylesheetEntry[];
  styleRanges: Map<string, RuleSpan[]>;
  stylePaths: Set<string>;
  unscopedPaths: Set<string>;
  warnings: MigrationWarning[];
  compiler?: VueCompiler;
}

export interface ShadowCssEntry {
  path: string;
  source: string;
  scoped?: boolean;
  migratingUnscoped?: boolean;
}

export interface DesignSystem {
  theme: { prefix: string | null };
  candidatesToCss: (candidates: string[]) => (string | null)[];
}

export interface LoadedTailwind {
  designSystem: DesignSystem;
  css: string;
  path: string;
  themeTokens: Record<string, string>;
  /** Stylesheet sources retained from the entry's import graph, parsed for
   * authored custom-variant reservations and content-identity adoption. */
  graphSources: { path: string; source: string }[];
  /** Load a design system for a replacement entry source, sharing the
   * entry's module and stylesheet resolution; used to validate the
   * augmented entry in memory. */
  loadWith: (css: string) => Promise<DesignSystem>;
}

export interface CssImport {
  href: string;
  media: string;
  start: number;
  end: number;
}

export type StylesheetLoader = (
  id: string,
  base: string,
) => Promise<{ content: string; base: string }>;
