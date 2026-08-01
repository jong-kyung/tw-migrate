export function unifiedDiff(path: string, before: string, after: string): string {
  const oldLines = before.split("\n");
  const newLines = after.split("\n");
  return [
    `--- a/${path}\n`,
    `+++ b/${path}\n`,
    `@@ -1,${oldLines.length} +1,${newLines.length} @@\n`,
    ...oldLines.map(
      (line, index) => `${index === oldLines.length - 1 && line === "" ? " " : "-"}${line}\n`,
    ),
    ...newLines.map(
      (line, index) => `${index === newLines.length - 1 && line === "" ? " " : "+"}${line}\n`,
    ),
  ].join("");
}
