// Shapes of the ecosystem manifest, the staged package provenance, and the
// browser captures the lifecycle compares. `projects.json` is untrusted input
// until `validateManifest` narrows it to `Manifest`.

export type SelectorType = 'role' | 'name' | 'text' | 'data' | 'id' | 'tag' | 'css';

export interface ProbeSelector {
  type: SelectorType;
  value: string;
  name?: string;
}

export interface Expectation {
  selector: ProbeSelector;
  cardinality: number;
}

export interface Viewport {
  width: number;
  height: number;
}

export type ProbeAction =
  | { type: 'press'; key: string }
  | { type: 'click' | 'hover' | 'focus'; selector: ProbeSelector };

export interface Probe {
  route: string;
  viewport: Viewport;
  readiness: Expectation;
  selector: ProbeSelector;
  cardinality: number;
  identity: string[];
  action?: ProbeAction;
}

export interface ProjectSource {
  path: string;
  before: string;
  after: string;
}

export interface ControlledProject {
  id: string;
  kind: 'controlled';
  runtime: 'react-vite' | 'next' | 'vite-html';
  style: 'css' | 'scss' | 'sass' | 'less';
  source: ProjectSource;
  probes: Record<string, Probe>;
}

export interface SmokeProject {
  id: string;
  kind: 'smoke';
  fixture: string;
}

export interface ExternalInstall {
  cwd: string;
  args: string[];
}

export interface ExternalProject {
  id: string;
  kind: 'external';
  repository: string;
  revision: string;
  packageManager: string;
  lockfile: string;
  packageRoot: string;
  installs: ExternalInstall[];
  runtimeWrites: string[];
  start: string[];
  server: 'vite' | 'next';
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

export interface InstalledLayout {
  root: string;
  native: string;
  addon: string;
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
