// Reports source strings that look like they were missed by the English
// translation dictionary. Run with: npm run i18n:check
//
// Exit code 1 means a real Turkish UI string is still present in the source.
// CSS selectors and other non-UI strings are ignored.
import {parse} from '@babel/parser';
import _traverse from '@babel/traverse';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

const traverse = _traverse.default ?? _traverse;
const here = path.dirname(fileURLToPath(import.meta.url));
const srcDir = path.join(here, '..', 'src');
const dict = JSON.parse(fs.readFileSync(path.join(here, 'tr-en.json'), 'utf8'));

const turkishChar = /[\u00e7\u011f\u0131\u015f\u00f6\u00fc\u00c7\u011e\u0130\u015e\u00d6\u00dc]/;
const turkishWord = /\b(için|ile|bir|bu|şu|ve|not|notlar|sayfa|klasör|çizgi|kalem|renk|ayar|kaydet|sil|geri|yinele|kapat|seç|tüm|yeni|ekle|ara|bul|yazdır|paylaş|taşı|kopyala|çöp|favori|bulut|eşitle|hesap|gün|öğe|yok|var|değil|şimdi|önce|sonra|üzerine|hata|uyarı|yükleniyor|düzenle|görünüm|başlık|metin|boyut|kalınlık|varsayılan|tema|dil|menü|aç|kayıt|sürüm|oturum|cihaz|yerel|bağlantı|giriş|çıkış|dosya|indir|yükle|sırala|göster|gizle|izgara|liste|okuma|tam|ekran|sayfalar|renkler|şekil|dolgu|vurgu|yazı|madde|numara|hizala|girinti|onay|kutu|bitti|kelime|resim|nesne|sistem|ortala|hizalama|paragraf|elips)\b/i;

const isSelector = value =>
  /:not\(|\[[a-z-]+[=\]]/.test(value) && !turkishChar.test(value);

const hasTranslation = value =>
  dict[value] !== undefined ||
  dict[value.trim()] !== undefined ||
  dict[` ${value.trim()}`] !== undefined ||
  dict[`${value.trim()} `] !== undefined;

const seen = new Map();
const files = fs.readdirSync(srcDir).filter(f => /\.(tsx|ts)$/.test(f));

for (const f of files) {
  if (f === 'i18n-runtime.ts') continue;
  const src = fs.readFileSync(path.join(srcDir, f), 'utf8');
  const ast = parse(src, {sourceType: 'module', plugins: ['typescript', 'jsx']});
  const check = (value, line, kind) => {
    const v = value.trim();
    if (!v || hasTranslation(value)) return;
    if (v === 'Türkçe' || v === 'English') return; // language names stay as-is
    if (isSelector(v)) return;
    if (!turkishChar.test(v) && !turkishWord.test(v)) return;
    if (/^(http|\/|\.|@|data:)/.test(v)) return;
    if (/[{}<>]/.test(v) || /px|calc\(|url\(|var\(/.test(v)) return;
    if (!seen.has(v)) seen.set(v, `${f}:${line} (${kind})`);
  };
  traverse(ast, {
    JSXText(p) { check(p.node.value.replace(/\s+/g, ' '), p.node.loc.start.line, 'jsx'); },
    StringLiteral(p) { check(p.node.value, p.node.loc.start.line, 'str'); },
    TemplateLiteral(p) {
      let key = '';
      p.node.quasis.forEach((q, i) => {
        key += q.value.cooked ?? q.value.raw;
        if (i < p.node.expressions.length) key += `{${i}}`;
      });
      if (hasTranslation(key)) return;
      for (const q of p.node.quasis) check(q.value.cooked ?? q.value.raw, q.loc.start.line, 'tpl');
    },
  });
}

for (const [value, location] of seen) console.log(location.padEnd(30), JSON.stringify(value));
if (seen.size) {
  console.error(`\n${seen.size} untranslated UI string(s) found.`);
  process.exit(1);
}
console.log('No untranslated Turkish UI strings in src/.');
