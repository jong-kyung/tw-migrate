#!/usr/bin/env node

import { migrate } from '../index.js';

const args = process.argv.slice(2);
let cssFile;
let tailwindCss;
let write = false;

for (let index = 0; index < args.length; index += 1) {
  const argument = args[index];
  if (argument === '--write') write = true;
  else if (argument === '--tailwind-css') tailwindCss = args[++index];
  else if (argument === '--help' || argument === '-h') {
    console.log('Usage: tw-migrate <css-file> [--tailwind-css <entry.css>] [--write]');
    process.exit(0);
  } else if (argument.startsWith('-')) {
    throw new Error(`Unknown option: ${argument}`);
  } else if (!cssFile) cssFile = argument;
  else throw new Error(`Unexpected argument: ${argument}`);
}

if (!cssFile) throw new Error('Usage: tw-migrate <css-file> [--tailwind-css <entry.css>] [--write]');

const report = await migrate({ cssFile, tailwindCss, write });
if (report.diff) process.stdout.write(report.diff);
for (const warning of report.warnings) {
  console.warn(`warning[${warning.code}] ${warning.file}:${warning.start}-${warning.end} ${warning.message}`);
}
console.log(`${write ? 'Applied' : 'Previewed'} ${report.changedFiles.length} file(s); ${report.convertedRules} rule(s) converted, ${report.retainedRules} retained.`);
