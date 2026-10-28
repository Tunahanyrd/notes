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

## Resim, şekil ve metin kutusu olan notlarda kalem (2026-10-28)

Durum: kaynakta uygulandı, sentetik testler geçti
(`crates/notes-host/tests/ink_objects.rs`). **Cloud'a yazılmadı, cihazda
görülmedi**; kullanıcı turu `tools/live_ink_objects_probe.py`.

- Kural tek yerde: `rs_ink::core::objects::passthrough_object` (rs-ops'ta
  `util::passthrough_object` / `util::ink_objects`). Sayfa katmanının
  `objectList`'inde doğrudan duran `image`, `shape`, `line`, `textBox`
  nesneleri kabul edilir; şu koşullarla: kanonik UUID, `hash ==
  SHA256(id + modifiedTime)`, içinde başka `object` yok (grup/gömülü nesne
  yok), hiçbir yerde `fileId` (önbellek) yok, resimde tam bir
  `fillEffect type="image" imageId=<içerik dosyası id>`, diğer türlerde hiç
  `imageId` yok (resim dolgulu şekil reddedilir). Bunun dışındaki her nesne notu
  eskisi gibi kapalı tutar.
- Bu nesneler hiçbir yazıcıda değişmez: öğe, `objectList` sırası, not ağacı
  girdisi ve resmin içerik dosyası girdisi bayt bayt aynı kalır. Katman/sayfa
  özetleri çocukların kayıtlı özetlerinden yeniden hesaplanır (Samsung kuralı).
  Yükleme gövdesinde yalnız düzenlenen çizgi gider; not ağacı tam kalır.
- Önbellek bırakma (`release_page_cache`): resmin içerik dosyası PDF gibi
  ele alınır: `refCount` resim kullanım sayısına eşit olmalı, önbellek
  eşlemesi olamaz, meta verisi değişmez. Dosya özeti `isFileAttached`
  değerinin kayıtlı yazımıyla (`true`/`True`, ikisi de gözlendi) doğrulanır.
- Yazıcılar: ekleme tarif 2/3 (tarif 1 eski çizgi-yalnız profilini korur),
  ilk çizgi (yalnız nesne taşıyan sayfada nesnelerin sonuna eklenir ve var olan
  `drawnRect` birleşimle büyür; boş sayfada eski davranış), fosforlu kalem,
  silgi ve taşıma. Silgi/taşıma hedefi yalnız çizgi olabilir; resim/şekil/metin
  kutusu kimliği gelirse reddedilir. Sayfadaki son çizgi, sayfada taşınan nesne
  kaldığı sürece silinebilir.
- Tarif kimlikleri değişmedi: daha önce kabul edilen her girdi için çıktı bayt
  bayt aynı (sentetik ve oracle diferansiyelleri). Yalnız önceden reddedilen
  girdiler kabul ediliyor; bu, PDF kitaplarda tarif 2+'nın sayfa sınırının
  genişletilmesiyle aynı yaklaşım. Compare koruması değişmedi: sunucu yalnız
  sayfa + katman + yeni/silinen/taşınan çizgi (+ bırakılan önbellek) adını
  verebilir; taşınan nesneler adlandırılırsa yayın durur.
- Projeksiyon tarifi `ink-projection-v7:carried-objects` (önbelleğe alınmış
  eski kapalı projeksiyonlar yeniden hesaplanır).
- Hâlâ kapalı: kilitli/şifreli not, gruplanmış çizgi (`strokeGroup`), ses/ek
  dosya, taşınamayan serbest yerleşimli nesneler (aşağıya bakın), döndürülmüş
  sayfa, arka plan resmi/şablon URI'si, birden çok katmanda ilk çizgi, kilitli/
  gizli katman, iç içe veya önbelleğe bağlı nesneler.

## Serbest yerleşimli nesneler (2026-10-28)

Durum: kaynakta uygulandı, sentetik testler geçti
(`crates/notes-host/tests/ink_objects.rs`, `floating` bölümü; yeni testler eski
kodda kırmızıydı). Kullanıcının telefonda yaptığı test notunun **özel yerel
kopyasında çevrimdışı** hazırlık da geçti (yalnız sonuç/sayı yazdırıldı).
**Cloud'a yazılmadı, cihazda görülmedi**; kullanıcı turu
`tools/live_ink_objects_probe.py` (LinuxInk-Probe-015).

- Telefonda eklenen resim sayfa katmanına değil, notun `floatingObjectList`'ine
  yazılır: `object type="image"` + `pageIndex`, `layoutType`,
  `ownerPageWidth/Height`, `rect`, `connectionPointList`, `lineColorEffect`,
  `lineStyleEffect`, `originalRect`, `path/segmentList`, `fillEffect
  type="image"` (`stretchOffset`, `tilingOffset`, `ninePatchRect`). Not ağacında
  `noteTree/floatingObjectList/object id t h`, içerik dosyası `refCount` 1.
  Gövde metni resmi `objectSpan` (U+FFFC çevresinde, değer `<id>,…`) ile çapalar.
- Kural tek yerde: `rs_doc::objects` (`passthrough_object`, `floating_object`,
  `floating_inventory`); rs-ink `core::objects::tree_floating`, rs-ops
  `util::floating_objects` ve metin editörü aynı kuralı kullanır. Kabul:
  sayfa nesnesiyle aynı kendine yeterlilik kuralı (resim/şekil/çizgi/metin
  kutusu, `hash == SHA256(id + modifiedTime)`, iç içe nesne yok, `fileId` yok,
  resimde tek `fillEffect imageId`), ek olarak `pageIndex` notun bir sayfası ve
  not ağacı girdileri sıra sıra aynı (`id`, `t`, `h`; yaprak). Başka her şey
  (bilinmeyen tür, sayfa dışı/eksik `pageIndex`, eski ağaç girdisi, grup) notu
  eskisi gibi kapalı tutar.
- Yazıcılar (ekleme tarif 2/3, ilk çizgi, fosforlu, silgi, taşıma, şekil) listeyi,
  ağaç girdisini ve resmin içerik dosyasını bayt bayt korur; liste yüklemeye
  hiç girmez (zaten girmiyordu). Önbellek bırakma resmin dosyasını sayfa
  resimleri gibi sayar (`refCount` = kullanım). Zaman damgası serbest nesnelerin
  `modifiedTime`'ını da aşar. Tarif 1 eskisi gibi kapalı. Daha önce kabul edilen
  her girdinin çıktısı aynı.
- Sayfa işlemleri: serbest nesnenin sayfası ve öncesi değişemez (o sayfaya kadar
  silme/sıralama, önüne sayfa ekleme reddedilir; sonuna ekleme serbest),
  `canReorder` false. Bu yeni bir koruma: önceden sayfa işlemleri serbest
  nesneyi başka sayfaya kaydırabiliyordu.
- Compare koruması değişmedi: sunucu serbest nesneyi adlandırırsa yayın durur.
- Projeksiyon tarifi `ink-projection-v8:floating-objects`.
