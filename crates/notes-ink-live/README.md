# notes-ink-live

Canlı kalem önizlemesi, GPL-3.0. Kalemin altındaki çizgi, kayıtlı sayfaların
kullandığı çiziciyle çizilir: sabitlenmiş **twangodev/sdocx** commit
`41e9c1099b29a48dc7320501573d7c4858f09303` (`references/upstream/sdocx`,
`tools/fetch_sdocx.sh`) ve aynı resvg sürümü (`=0.47.0`, crates/rs-render ile
aynı). Algoritma yeniden yazılmadı; burada yalnız yazıcının girdisi taklit edilir:

- `rs-ink` capture nicelemesi (Q5 konum, Q12 basınç, float32 birikim);
  `src/quantize_expected.txt` Python oracle'ından (`normalize_capture`) alındı.
- Şablon kalem: sayfa çizicisinin gördüğü `rendering` nesnesi
  (`GET /api/notes/:id/ink-pens`), stylus kanalları düşürülür (yazıcı da
  düşürür), renk/kalınlık değişince recipe 3 kuralları (fixedWidth, tolerans).
- Boş sayfada ilk çizgi: recipe 4 FountainPen `18;0;100;`, araç 3.
- tools/ink-renderer `calibrated` ayarları (InkPen2 `9;`, profil 1440×2304).

Ayrı Cargo workspace'idir (kök workspace üyesi değil; GPL kaynak ve lockfile
motor derlemesine girmez).

## Derleme

```sh
tools/build_ink_wasm.sh            # desktop/public/ink-live.wasm (Vite → dist/)
cd desktop && npm run build
```

`wasm32-unknown-unknown` std gerekir. Dağıtım Rust paketlerinde çoğu zaman yok:
`rustup target add wasm32-unknown-unknown` ya da `RUSTUP_HOME`/`CARGO_HOME`
ile bu hedefi içeren yalıtılmış bir rustup. Derleme `--locked` ve
`--remap-path-prefix` ile yapılır; aynı kaynak, aynı araç zinciri ve aynı
kayıt kaynağıyla iki temiz derlemede aynı `.wasm` baytları ölçüldü. Çıktı
`.gitignore`'da; depoya önceden derlenmiş `.wasm` konmaz. Dosya yoksa arayüz
sessizce eski SVG çizgisine döner.

Arayüz modülü bir Web Worker'da çalıştırır (`desktop/src/inkLiveWorker.ts`).
CSP'de `script-src 'wasm-unsafe-eval'` gerekir (motorun ve Tauri'nin CSP'sine
eklendi; string `eval` açılmaz).

## C-ABI

`src/wasm.rs`: `ink_alloc/ink_free`, `ink_pen_new(json)`, `ink_render(pen,
samples f64×4, n, scale)`, `ink_out_rect/ptr/len` (düz alfa RGBA), `ink_error`.
wasm-bindgen yok.

## Doğrulama

- `cargo test --release` (12 test): niceleme oracle eşitliği, basınç → genişlik
  (stylus Fountain), ilk çizgi fare profili, recipe 3, desteklenmeyen kalemlerin
  reddi, determinism, renk.
- `node tools/ink_live_fidelity.mjs`: aynı sentetik çizgiler için `.wasm`
  çıktısı ile yerel yol (Python oracle nicelemesi → tools/ink-renderer →
  resvg) aynı ölçekte piksel piksel karşılaştırılır.
- Playwright `desktop/tests/ink-live.spec.ts`.

Sınırlar: çizim yerel çizicinin çıktısıdır, Samsung GPU pikseli değildir.
Yeni InkPen2 çizgisi stylus kanalı taşımadığı için sdocx'in yaklaşık InkPen2
yolu çizer; kayıttan sonra sayfa önizleyicisi de aynı çizgiyi aynı yoldan
çizer (notes-preview `admitted`, stylus kanalsız InkPen2 2026-10-27'den beri
kabul; önceden sayfanın tamamı reddediliyordu).
