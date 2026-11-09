// Runtime UI language for the frontend.
//
// The source strings are Turkish and are rewritten to t("...") calls at build
// time. This module holds the current language and the Turkish -> English
// dictionary. Only UI strings go through here; note content never does.
import translations from '../i18n/tr-en.json';

export type Language = 'en' | 'tr';

declare const __NOTLAR_DEFAULT_LANGUAGE__: Language;

const STORAGE_KEY = 'samsung-notes.language.v1';
const COOKIE = 'notes_lang_v1';
const dictionary = translations as Record<string, string>;

/**
 * The engine's device preferences own the choice (they survive the random
 * per-launch port). Strings are evaluated at module load, before those are
 * fetched, so the last known choice is cached in a cookie (cookies are shared
 * across ports on 127.0.0.1, localStorage is not) with localStorage as a
 * fallback for the browser build.
 */
function cachedLanguage(): Language | null {
  const cookie = document.cookie.match(/(?:^|;\s*)notes_lang_v1=(en|tr)(?:;|$)/);
  if (cookie) return cookie[1] as Language;
  try {
    const value = localStorage.getItem(STORAGE_KEY);
    if (value === 'en' || value === 'tr') return value;
  } catch { /* localStorage can be unavailable */ }
  return null;
}

const language: Language = cachedLanguage() ?? (__NOTLAR_DEFAULT_LANGUAGE__ === 'en' ? 'en' : 'tr');

/** The language this page was rendered in; changing it needs a reload. */
export function getLanguage(): Language {
  return language;
}

/** Remember the language for the next page load. */
export function cacheLanguage(next: Language): void {
  try { localStorage.setItem(STORAGE_KEY, next); } catch { /* the cookie still carries it */ }
  document.cookie = `${COOKIE}=${next}; Path=/; Max-Age=31536000; SameSite=Strict`;
}

const RELOAD_KEY = 'samsung-notes.language-reload';

/**
 * Reload once into `next`. If the cache could not carry the choice the page
 * would come back in the old language; the session flag stops a reload loop.
 */
export function reloadInto(next: Language): void {
  if (next === language) return;
  cacheLanguage(next);
  try {
    if (sessionStorage.getItem(RELOAD_KEY) === next) return;
    sessionStorage.setItem(RELOAD_KEY, next);
  } catch { return; }
  location.reload();
}

try { if (sessionStorage.getItem(RELOAD_KEY) === language) sessionStorage.removeItem(RELOAD_KEY); } catch { /* ignore */ }

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
