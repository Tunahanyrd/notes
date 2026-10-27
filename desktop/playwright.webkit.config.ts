// WebKit run of the same UI suite: the macOS app renders in WKWebView, so
// this approximates it more closely than Chromium. Used by the macOS CI job
// (informational) and by `npx playwright test -c playwright.webkit.config.ts`.
import base from './playwright.config';
import {defineConfig} from '@playwright/test';
export default defineConfig({...base, use:{...base.use, browserName:'webkit', launchOptions:{}}});
