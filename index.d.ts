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
  start: number;
  end: number;
  message: string;
}

export interface RuleReport {
  selector: string;
  status: 'converted' | 'retained';
  candidates: string[];
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
