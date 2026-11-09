import {defineConfig} from 'vite';
import react from '@vitejs/plugin-react';
import {translateUi} from './i18n/plugin.mjs';

// The UI source is Turkish and is translated at runtime by the plugin, so the
// language can be switched inside the app. NOTLAR_LANG=en only changes the
// language used on first launch (before the user picks one in Settings).
export default defineConfig({
  plugins: [react(), translateUi()],
  define: {
    __NOTLAR_DEFAULT_LANGUAGE__: JSON.stringify(process.env.NOTLAR_LANG === 'en' ? 'en' : 'tr'),
  },
  clearScreen: false,
});
