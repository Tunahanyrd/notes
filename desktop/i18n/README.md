# UI language (English / Türkçe)

The upstream interface is Turkish. This folder adds a runtime English
translation that can be switched inside the app, without touching note data:

- `tr-en.json` — Turkish → English dictionary (UI strings only). Entries may
  contain `{0}`, `{1}`, ... placeholders for strings built from variables.
- `plugin.mjs` — Babel plugin that rewrites static strings in `src/` (JSX text,
  string literals, and template literals) into calls to `t()` from
  `src/i18n-runtime.ts`.
- `check.mjs` — reports source strings that have no dictionary entry.
- `../src/i18n-runtime.ts` — holds the current language and performs lookups.

Only strings written in the source pass through `t()`. Note text, titles,
folder names, and anything else loaded from the engine at runtime are never
translated.

## Switching

Settings → General → Language has **English** and **Türkçe**. The choice is
a device preference (`language` in `/api/preferences`, stored by the engine
next to the library), because the native window gets a new `127.0.0.1` port
on every launch and anything the page stores is per origin. Strings are
evaluated at module load, before preferences arrive, so the last choice is
also cached in the `notes_lang_v1` cookie (cookies are shared across ports)
and in `localStorage` for the browser build. If the engine's choice differs
from the cached one the page reloads once; the webview also reloads after a
change so menus and shortcut lists pick up the new language.

`NOTLAR_LANG=en` only changes the default for a first launch with no saved
choice; the in-app switch always works.

## Commands

```sh
cd desktop
npm run build        # Turkish by default, switchable in Settings
npm run i18n:check   # verify every Turkish UI string has a translation
npm run build:en     # first launch defaults to English
```

## Adding a translation

1. Add the Turkish string to `tr-en.json`.
2. Add the English text as its value.
3. Run `npm run i18n:check` and rebuild.

Strings built from variables (for example `` `${count} not` ``) are matched as
whole templates. Add an entry such as `"{0} not": "{0} notes"` when the word
order differs; otherwise the static chunks around the placeholders are
translated individually.
