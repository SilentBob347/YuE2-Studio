import React, { useEffect, useRef, useState } from 'react';
import abcjs from 'abcjs';

/**
 * Engraves an ABC score as notation.
 *
 * The score is the one the model planned (or the one it was given), so the
 * voices it declares - "Vocal" and "Ins" - are drawn as separate staves the way
 * a musician would read them. Rendering is local; nothing is fetched.
 */
export const ScoreView: React.FC<{ abc: string; className?: string }> = ({ abc, className }) => {
  const host = useRef<HTMLDivElement | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const element = host.current;
    if (!element) return;
    // Typing into the score editor re-engraves on every keystroke; a short
    // pause keeps that from stuttering on a long song.
    const timer = window.setTimeout(() => {
      try {
        const rendered = abcjs.renderAbc(element, abc, {
          responsive: 'resize',
          add_classes: true,
          paddingtop: 4,
          paddingbottom: 4,
          paddingleft: 4,
          paddingright: 4,
          staffwidth: 740,
          wrap: { minSpacing: 1.6, maxSpacing: 2.8, preferredMeasuresPerLine: 4 },
          foregroundColor: 'currentColor',
        });
        setFailed(!rendered?.length || rendered[0].lines.length === 0);
      } catch {
        setFailed(true);
      }
    }, 250);
    return () => window.clearTimeout(timer);
  }, [abc]);

  return (
    <div className={className}>
      <div ref={host} className="score-view text-zinc-900" />
      {failed && <p className="p-2 text-[11px] text-zinc-500">ABC</p>}
    </div>
  );
};
