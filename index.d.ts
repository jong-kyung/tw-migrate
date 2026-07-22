export interface MigrateOptions {
  styleFile?: string;
  cwd?: string;
  write?: boolean;
  tailwindCss?: string;
  workspaces?: boolean;
  force?: boolean;
}

export interface MigrationWarning {
  code: string;
  file: string;
  /** Byte offsets into the authored file, or (0, 0) when no unique mapping exists. */
  start: number;
  end: number;
  message: string;
}

export interface RuleReport {
  selector: string;
  status: 'converted' | 'retained';
  candidates: string[];
  file: string;
  /** Rule span in the analysis source (compiled CSS for preprocessor stylesheets). */
  ruleId: { start: number; end: number };
  /** Rule span in the authored file, or (0, 0) when no unique mapping exists. */
  authoredSpan: { start: number; end: number };
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

export function migrate(options?: MigrateOptions): Promise<MigrationReport>;
