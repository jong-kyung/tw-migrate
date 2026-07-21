#!/usr/bin/env node

import { migrate } from '../index.js';

const usage = 'Usage: tw-migrate [css-file] [--tailwind-css <entry.css>] [--workspaces] [--force] [--write]';

async function main() {
  const args = process.argv.slice(2);
  let cssFile;
  let tailwindCss;
  let write = false;
  let force = false;
  let workspaces = false;

  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === '--write') write = true;
    else if (argument === '--force') force = true;
    else if (argument === '--workspaces') workspaces = true;
    else if (argument === '--tailwind-css') {
      tailwindCss = args[++index];
      if (!tailwindCss) throw new Error(`${usage}\n--tailwind-css requires a path.`);
    } else if (argument === '--help' || argument === '-h') {
      console.log(usage);
      return;
    } else if (argument.startsWith('-')) {
      throw new Error(`Unknown option: ${argument}`);
    } else if (!cssFile) cssFile = argument;
    else throw new Error(`Unexpected argument: ${argument}`);
  }

  const report = await migrate({ cssFile, tailwindCss, write, force, workspaces });
  if (report.diff) process.stdout.write(report.diff);
  for (const warning of report.warnings) {
    console.warn(`warning[${warning.code}] ${warning.file}:${warning.start}-${warning.end} ${warning.message}`);
  }
  for (const failure of report.failures) {
    console.warn(`skipped[${failure.package}] ${failure.message}`);
  }
  console.log(`${write ? 'Applied' : 'Previewed'} ${report.changedFiles.length} file(s); ${report.convertedRules} rule(s) converted, ${report.retainedRules} retained.`);
}

main().catch((error) => {
  console.error(`tw-migrate: ${error.message}`);
  process.exitCode = 1;
});
