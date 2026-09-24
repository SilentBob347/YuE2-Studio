import { en } from './en';
import { zh } from './zh';
import { ja } from './ja';
import { ko } from './ko';
import { ru } from './ru';
import { yue2 } from './yue2';
import { adapterStrings } from './adapters';
import { processingStrings } from './processing';
import { trainingStrings } from './training';

export type Language = 'en' | 'zh' | 'ja' | 'ko' | 'ru';

const enAll = { ...en, ...yue2.en, ...adapterStrings.en, ...processingStrings.en, ...trainingStrings.en };

export type TranslationKey = keyof typeof enAll;

export const translations: Record<Language, Partial<Record<TranslationKey, string>>> = {
  en: enAll,
  zh: { ...zh, ...yue2.zh, ...adapterStrings.zh, ...processingStrings.zh, ...trainingStrings.zh },
  ja: { ...ja, ...yue2.ja, ...adapterStrings.ja, ...processingStrings.ja, ...trainingStrings.ja },
  ko: { ...ko, ...yue2.ko, ...adapterStrings.ko, ...processingStrings.ko, ...trainingStrings.ko },
  ru: { ...ru, ...yue2.ru, ...adapterStrings.ru, ...processingStrings.ru, ...trainingStrings.ru },
};
