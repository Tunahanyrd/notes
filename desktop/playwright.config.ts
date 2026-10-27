import {defineConfig} from '@playwright/test';
import {existsSync} from 'node:fs';
// PLAYWRIGHT_CHROMIUM_PATH overrides the browser; otherwise the system /usr/bin/chromium
// is used when present, else Playwright's bundled Chromium (CI: npx playwright install chromium).
const chromium=process.env.PLAYWRIGHT_CHROMIUM_PATH||(existsSync('/usr/bin/chromium')?'/usr/bin/chromium':undefined);
// NOTES_TEST_OUTDIR lets tests run against a separate build while the native app serves dist/.
const outDir=process.env.NOTES_TEST_OUTDIR?` --outDir ${process.env.NOTES_TEST_OUTDIR}`:'';
export default defineConfig({testDir:'./tests',testMatch:'*.spec.ts',fullyParallel:false,workers:1,reporter:'list',use:{baseURL:'http://127.0.0.1:4178',launchOptions:{executablePath:chromium,args:['--no-sandbox']},viewport:{width:1440,height:960}},webServer:{command:`npm run preview -- --port 4178 --strictPort${outDir}`,url:'http://127.0.0.1:4178',reuseExistingServer:false}});
