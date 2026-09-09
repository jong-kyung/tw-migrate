import { createTwoFilesPatch, FILE_HEADERS_ONLY } from "diff";

export function unifiedDiff(path: string, before: string, after: string): string {
  if (before === after) return "";
  return createTwoFilesPatch(`a/${path}`, `b/${path}`, before, after, undefined, undefined, {
    context: 3,
    headerOptions: FILE_HEADERS_ONLY,
  });
}
