// Runtime UI language for the frontend.
//
// The source strings are Turkish and are rewritten to t("...") calls at build
// time. This module holds the current language and the Turkish -> English
// dictionary. Only UI strings go through here; note content never does.
import translations from '../i18n/tr-en.json';

export type Language = 'en' | 'tr';

declare const __NOTLAR_DEFAULT_LANGUAGE__: Language;

const STORAGE_KEY = 'samsung-notes.language.v1';
const dictionary = translations as Record<string, string>;

function storedLanguage(): Language {
  try {
    const value = localStorage.getItem(STORAGE_KEY);
    if (value === 'en' || value === 'tr') return value;
  } catch { /* localStorage can be unavailable */ }
  return __NOTLAR_DEFAULT_LANGUAGE__ === 'tr' ? 'tr' : 'en';
}

let language: Language = storedLanguage();

export function getLanguage(): Language {
  return language;
}

export function setLanguage(next: Language): void {
  language = next;
  try { localStorage.setItem(STORAGE_KEY, next); } catch { /* keep the in-memory choice */ }
}

/**
 * Translate a static UI string. Turkish is the source language, so it is
 * returned unchanged; English comes from the dictionary. `{0}`, `{1}`, ...
 * placeholders are filled from the extra arguments.
 */
export function t(key: string, ...values: unknown[]): string {
  const text = language === 'en' ? dictionary[key] ?? key : key;
  if (!values.length) return text;
  return text.replace(/\{(\d+)\}/g, (match, index) => {
    const value = values[Number(index)];
    return value === undefined ? match : String(value);
  });
}
