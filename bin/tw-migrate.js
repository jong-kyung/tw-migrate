#!/usr/bin/env node

import { migrate } from "../index.js";

const usage =
  "Usage: tw-migrate [style-file] [--tailwind-css <entry.css>] [--workspaces] [--force] [--write]";
export function formatDiagnostic(message, ansi) {
  return process.stderr.isTTY && !process.env.NO_COLOR ? `\x1b[${ansi}m${message}\x1b[0m` : message;
}

async function main() {
  const args = process.argv.slice(2);
  let styleFile;
  let tailwindCss;
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
    } else if (argument === "--help" || argument === "-h") {
      console.log(usage);
      return;
    } else if (argument.startsWith("-")) {
      throw new Error(`Unknown option: ${argument}`);
    } else if (!styleFile) styleFile = argument;
    else throw new Error(`Unexpected argument: ${argument}`);
  }

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

if (import.meta.main) {
  main().catch((error) => {
    console.error(formatDiagnostic(`tw-migrate: ${error.message}`, "31"));
    process.exitCode = 1;
  });
}
