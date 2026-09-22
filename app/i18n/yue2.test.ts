import { describe, expect, it } from 'vitest';
import { yue2 } from './yue2';

describe('YuE2 strings', () => {
  it('every language carries every key, none empty', () => {
    const keys = Object.keys(yue2.en).sort();
    for (const [language, strings] of Object.entries(yue2)) {
      expect(Object.keys(strings).sort(), language).toEqual(keys);
      for (const key of keys) {
        expect((strings as Record<string, string>)[key]?.trim().length, `${language}.${key}`).toBeGreaterThan(0);
      }
    }
  });

  it('no string names the model this studio was forked from', () => {
    for (const [language, strings] of Object.entries(yue2)) {
      for (const [key, value] of Object.entries(strings)) {
        expect(/music ?3|minimax/i.test(value), `${language}.${key}: ${value}`).toBe(false);
      }
    }
  });
});
