import { describe, expect, test } from 'bun:test';
import { copy } from './copy';
import { splitInlineCode } from './inlineCode';

describe('splitInlineCode', () => {
  test('marks backtick spans as code', () => {
    expect(splitInlineCode('Run `a b` now')).toEqual([
      { text: 'Run ', code: false },
      { text: 'a b', code: true },
      { text: ' now', code: false },
    ]);
  });
  test('plain text stays one part', () => {
    expect(splitInlineCode('plain')).toEqual([{ text: 'plain', code: false }]);
  });
  test('daemon-down hint has one command and no literal backticks after split', () => {
    const parts = splitInlineCode(copy.daemon.downHint);
    expect(parts.filter((p) => p.code).map((p) => p.text)).toEqual(['git-strata daemon start']);
    expect(parts.some((p) => p.text.includes('`'))).toBe(false);
  });
});
