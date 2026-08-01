#!/usr/bin/env node

import packageJson from "../package.json" with { type: "json" };
import { formatDiagnostic } from "./diagnostics.ts";

const { version } = packageJson;
const usage = "Usage: tw-migrate [style-file] [options]";
const help = `${usage}

Arguments:
  [style-file]               Migrate one stylesheet instead of the current package.
                             Cannot be used with --workspaces.

Options:
  --tailwind-css <entry.css> Use this Tailwind CSS entry when the current package
                             has multiple entries. Cannot be used with --workspaces.
  --workspaces               Migrate every package in the workspace.
  --force                    Skip packages with recoverable discovery or input errors.
  --write                    Apply changes instead of previewing them.
  -h, --help                 Show this help message.
  -v, --version              Show the version number.

Examples:
  tw-migrate
  tw-migrate --write
  tw-migrate path/to/Button.module.scss
  tw-migrate --workspaces --write`;

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.includes("--help") || args.includes("-h")) {
    console.log(help);
    return;
  }
  if (args.includes("--version") || args.includes("-v")) {
    console.log(`tw-migrate ${version}`);
    return;
  }

  let styleFile: string | undefined;
  let tailwindCss: string | undefined;
  let write = false;
  let force = false;
  let workspaces = false;

  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--write") write = true;
    else if (argument === "--force") force = true;
    else if (argument === "--workspaces") workspaces = true;
    else if (argument === "--tailwind-css") {
      tailwindCss = args[++index];
      if (!tailwindCss) throw new Error(`${usage}\n--tailwind-css requires a path.`);
    } else if (argument.startsWith("-")) {
      throw new Error(`Unknown option: ${argument}`);
    } else if (!styleFile) styleFile = argument;
    else throw new Error(`Unexpected argument: ${argument}`);
  }

  const { migrate } = await import("./index.ts");
  const report = await migrate({ styleFile, tailwindCss, write, force, workspaces });
  if (report.diff) process.stdout.write(report.diff);
  for (const warning of report.warnings) {
    console.warn(
      formatDiagnostic(
        `warning[${warning.code}] ${warning.file}:${warning.start}-${warning.end} ${warning.message}`,
        "38;5;208",
      ),
    );
  }
  for (const failure of report.failures) {
    console.warn(formatDiagnostic(`skipped[${failure.package}] ${failure.message}`, "38;5;208"));
  }
  console.log(
    `${write ? "Applied" : "Previewed"} ${report.changedFiles.length} file(s); ${report.convertedRules} rule(s) converted, ${report.retainedRules} retained.`,
  );
}

main().catch((error) => {
  console.error(formatDiagnostic(`tw-migrate: ${error.message}`, "31"));
  process.exitCode = 1;
});
