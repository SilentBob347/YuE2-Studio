import { describe, expect, it } from 'vitest';
import { adapterStrings } from './adapters';

describe('LoRA strings', () => {
  it('every language carries every key, none empty', () => {
    const keys = Object.keys(adapterStrings.en).sort();
    for (const [language, strings] of Object.entries(adapterStrings)) {
      expect(Object.keys(strings).sort(), language).toEqual(keys);
      for (const key of keys) {
        expect((strings as Record<string, string>)[key]?.trim().length, `${language}.${key}`).toBeGreaterThan(0);
      }
    }
  });

  it('names no model, so the page serves any engine', () => {
    for (const [language, strings] of Object.entries(adapterStrings)) {
      for (const [key, value] of Object.entries(strings)) {
        expect(/yue|music ?3|minimax|ace-?step/i.test(value), `${language}.${key}: ${value}`).toBe(false);
      }
    }
  });
});
