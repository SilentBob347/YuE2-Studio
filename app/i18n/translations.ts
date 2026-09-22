import { en } from './en';
import { zh } from './zh';
import { ja } from './ja';
import { ko } from './ko';
import { ru } from './ru';
import { yue2 } from './yue2';

export type Language = 'en' | 'zh' | 'ja' | 'ko' | 'ru';

const enAll = { ...en, ...yue2.en };

export type TranslationKey = keyof typeof enAll;

export const translations: Record<Language, Partial<Record<TranslationKey, string>>> = {
  en: enAll,
  zh: { ...zh, ...yue2.zh },
  ja: { ...ja, ...yue2.ja },
  ko: { ...ko, ...yue2.ko },
  ru: { ...ru, ...yue2.ru },
};
