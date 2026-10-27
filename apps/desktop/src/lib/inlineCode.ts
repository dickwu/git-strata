/** Split copy on backticks so `commands` can render as <code> instead of literal backticks. */
export function splitInlineCode(s: string): { text: string; code: boolean }[] {
  return s
    .split('`')
    .map((text, i) => ({ text, code: i % 2 === 1 }))
    .filter((p) => p.text !== '');
}
