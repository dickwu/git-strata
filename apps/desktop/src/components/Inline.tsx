import { splitInlineCode } from '@/lib/inlineCode';

/** Copy with `backtick` spans rendered as inline code. */
export function Inline({ text }: { text: string }) {
  return (
    <>
      {splitInlineCode(text).map((p, i) =>
        p.code ? (
          <code key={i} className="inline-code">
            {p.text}
          </code>
        ) : (
          <span key={i}>{p.text}</span>
        )
      )}
    </>
  );
}
