# Kalem düzenlemesi ve gönderim kuyruğu

`GET /api/notes/:id/ink`, notun yerel sürümüne bağlı `project_ink` sonucuna
`revision` ve `pendingStrokes` ekler. Ham binary, medya URL'si ve hesap bilgisi
arayüze çıkmaz. `pages[].strokes` kaynak kalem stilleridir. Desteklenmeyen veya
eşitleme bekleyen notlarda `capabilities.appendInk=false` olur.

`PUT /api/notes/:id/ink` gövdesi:

```json
{
  "expectedRevision": 2,
  "strokes": [{
    "pageId": "kaynak-sayfa-uuid",
    "sourceStrokeId": "bu-sayfadaki-kaynak-cizgi-uuid",
    "points": [
      {"x": 100, "y": 100, "pressure": 0.5, "timeMs": 0},
      {"x": 110, "y": 110, "pressure": 0.6, "timeMs": 10}
    ]
  }]
}
```

X/Y belge biriminde, süre ilk örnekten itibaren tam sayı milisaniyedir. Kalem
stili kaynak çizgiden gelir. İstek en çok 64 çizgi ve toplam 20.000 nokta içerir.
Kaynak ve sayfa bağı, koordinat/süre/basınç sınırları ve gerçek native belge
üreticisinin kabul koşulları kayıt öncesi doğrulanır. Yanıt `{note}` döndürür;
bu, yerel kalıcı kaydın tamamlandığını gösterir, Cloud onayı değildir.

SQLite schema 6, `ink_documents` ve `ink_jobs` tablolarını ekler. Kayıt tek
transaction içinde değişmez batch modeli, kaynak snapshot, UUID'ler ve sürüm
artışını saklar. Başka bir gövde/başlık kaydı batch bitene kadar kabul edilmez.
Yerel taslak çizgileri uygulama yeniden açıldığında GET yanıtında korunur.

Her çizgi ayrı doğrulanmış Cloud işlemiyle gönderilir. İşlem UUID'si batch UUID'si
ve çizgi sırasından UUIDv5 olarak türetilir. Başarılı geri okuma sonrasında
sonraki sıra ve Cloud snapshot aynı transaction ile checkpoint edilir. İşlem
başarılıyken checkpoint öncesi süreç kapanırsa aynı UUID'nin kayıtlı sonucu
kullanılır; yeni bir yayın kimliği üretilmez. Belirsiz gönderim tüm batch'i
durdurur; otomatik tekrar yoktur. Daha önce gönderilen çizgiler geri alınmaz.

Yeni batch modelleri `recipeVersion:2` ile native sayfa ölçeği ve sınırlı
önbellek bırakma sözleşmesini sabitler. Bu alanı içermeyen önceki modeller
v1 olarak çalıştırılır; eski işlemin anlamı aynı kimlikle değiştirilmez.

Çakışan çizimden düz metin kopyası çıkarıp başarı göstermek engellenmiştir.
Bu sürüm çizimli bir notu yeni nota kopyalamaz; hata durumunda orijinal batch
ve noktalar özel yerel kayıtta korunur.

Bu altyapı tek başına genel kalem desteği veya gerçek cihaz görsel doğrulaması
anlamına gelmez. Native önbellek, sayfa ölçeği ve desteklenen kalem profilleri
üzerindeki kapılar ayrıca sağlanmalıdır. `rendering.nativeEquivalent=false`
geometri DTO'su özgün kalem rasterizer'ıyla eşdeğer değildir.
