/**
 * The official YuE2 requests: the repository example, the demo site's songs
 * and its covers, which carry their melody score. The same files the service
 * shows the writing assistant as references.
 */

import type { YueCot } from '../types';

const modules = import.meta.glob('../examples/*.json', { eager: true }) as Record<string, { default?: unknown }>;

export interface YueExample {
  id: string;
  title: string;
  style: string;
  lyrics: string;
  cot: YueCot;
  abc: string;
  /** A cover realises a transcribed melody rather than writing its own. */
  cover: boolean;
}

const COTS: YueCot[] = ['full', 'melody', 'off'];

export const EXAMPLES: YueExample[] = Object.entries(modules)
  .map(([path, module]) => {
    const value = ((module as { default?: unknown }).default ?? module) as Record<string, unknown>;
    const id = path.split('/').pop()?.replace(/\.json$/, '') ?? path;
    const cot = COTS.includes(value.cot as YueCot) ? (value.cot as YueCot) : 'full';
    return {
      id,
      title: typeof value.title === 'string' ? value.title : id,
      style: typeof value.style === 'string' ? value.style.trim() : '',
      lyrics: typeof value.lyrics === 'string' ? value.lyrics : '',
      cot,
      abc: typeof value.abc === 'string' ? value.abc : '',
      cover: id.startsWith('cover-'),
    };
  })
  .filter(example => example.style.length > 0)
  .sort((a, b) => Number(b.cover) - Number(a.cover) || a.title.localeCompare(b.title));

export function randomExample(): YueExample {
  return EXAMPLES[Math.floor(Math.random() * EXAMPLES.length)];
}

/** A style prompt as one line for a list row: descriptors, no measurements. */
export function captionSummary(style: string): string {
  const technical = /(bpm|key|scale|tempo|time signature) is/i;
  return style
    .split(/[\n.]/)
    .map(part => part.trim())
    .filter(part => part.length > 0 && !technical.test(part))
    .join('. ')
    .trim();
}
