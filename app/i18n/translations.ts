import { en } from './en';
import { zh } from './zh';
import { ja } from './ja';
import { ko } from './ko';
import { ru } from './ru';

export type Language = 'en' | 'zh' | 'ja' | 'ko' | 'ru';

export type TranslationKey = keyof typeof en;

export const translations = { en, zh, ja, ko, ru };
