import { commands } from 'vitest/browser';
import { expect, inject, test } from 'vitest';

test('preserves the controlled fixture through the installed migration lifecycle', async () => {
  const project = inject('ecosystemProject');
  const result = await commands.runEcosystemCase(project.id);
  expect(result.phases.at(-1)).toBe('complete');
  expect(result.report.candidates).toContain(project.source.after);
});
