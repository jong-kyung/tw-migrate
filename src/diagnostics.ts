const ansiEscape = String.fromCharCode(27);

export function formatDiagnostic(message: string, ansi: string): string {
  return process.stderr.isTTY && !process.env.NO_COLOR
    ? `${ansiEscape}[${ansi}m${message}${ansiEscape}[0m`
    : message;
}
