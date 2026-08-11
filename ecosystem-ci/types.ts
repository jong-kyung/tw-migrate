// Shapes of the ecosystem manifest, the staged package provenance, and the
// browser captures the lifecycle compares. `projects.json` is untrusted input
// until `validateManifest` narrows it to `Manifest`.

export const selectorTypes = ["role", "text", "data", "id", "tag", "css"] as const;

export interface ProbeSelector {
  type: (typeof selectorTypes)[number];
  value: string;
  name?: string;
}

export type ProbeAction =
  | { type: "press"; key: string }
  | { type: "hover" | "focus"; selector: ProbeSelector };

export interface Probe {
  route: string;
  viewport: { width: number; height: number };
  readiness: { selector: ProbeSelector; cardinality: number };
  selector: ProbeSelector;
  cardinality: number;
  identity: string[];
  action?: ProbeAction;
  /** False exempts the probe from the causal witness: its element depends
   * on a stylesheet other than the case's witness source, while the
   * baseline-versus-post comparison still covers it. */
  witness?: false;
}

interface ProjectSource {
  path: string;
  before: string;
  after: string;
}

export const controlledRuntimes = ["react-vite", "next", "vite-html", "vue-vite"] as const;
export const controlledStyles = ["css", "scss", "sass", "less"] as const;

export interface ControlledProject {
  id: string;
  kind: "controlled";
  runtime: (typeof controlledRuntimes)[number];
  style: (typeof controlledStyles)[number];
  /** Fixture subpath under fixtures/controlled, when the case does not
   * live in the runtime/style matrix cell. */
  fixture?: string;
  /** How the migration runs: one stylesheet by default, the whole
   * package, or the workspace. */
  scope?: "package" | "workspaces";
  source: ProjectSource;
  probes: Record<string, Probe>;
}

export interface SmokeProject {
  id: string;
  kind: "smoke";
  fixture: string;
}

export interface ExternalProject {
  id: string;
  kind: "external";
  repository: string;
  revision: string;
  packageManager: string;
  lockfile: string;
  packageRoot: string;
  installs: { cwd: string; args: string[] }[];
  runtimeWrites: string[];
  start: string[];
  tailwindCss: string;
  source: ProjectSource;
  probes: Record<string, Probe>;
}

export type Project = ControlledProject | SmokeProject | ExternalProject;

/** Every project kind that carries its own probes and migration source. */
export type ProbedProject = ControlledProject | ExternalProject;

export interface Manifest {
  projects: Project[];
}

export interface PackageEntry {
  name: string;
  version: string;
  tarball: string;
  sha256: string;
}

export interface Provenance {
  commit: string;
  platform: string;
  packages: { root: PackageEntry; native: PackageEntry };
  addon: { file: string; sha256: string };
}

export interface CapturedElement {
  identity: string | undefined;
  styles: Record<string, string>;
}

export interface ProbeCapture {
  elements: CapturedElement[];
  consoleMessages: string[];
  pageErrors: string[];
}

export type CaptureSet = Record<string, ProbeCapture>;

export interface CaptureArtifact {
  screenshot: string;
  writeDiagnostics: (value: unknown) => Promise<void>;
}

export interface PhaseLedger {
  case: string;
  phases: { phase: string; files: string[] }[];
  teardown?: { step: string; at: string }[];
  failure?: string;
  failureFiles?: string[];
}

export interface RunningServer {
  url: string;
  stop: () => Promise<void>;
}

/** The subset of a `MigrationReport` the ecosystem lifecycle asserts on. */
export interface MigrationReport {
  changedFiles: string[];
  diff: string;
  candidates: string[];
}
