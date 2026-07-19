export interface MigrateOptions {
  cssFile: string;
  cwd?: string;
  write?: boolean;
  tailwindCss?: string;
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

export interface MigrationReport {
  changedFiles: string[];
  diff: string;
  convertedRules: number;
  retainedRules: number;
  rules: RuleReport[];
  candidates: string[];
  warnings: MigrationWarning[];
}

export function migrate(options: MigrateOptions): Promise<MigrationReport>;
