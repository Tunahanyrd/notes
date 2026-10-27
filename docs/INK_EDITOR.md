# Kalem verisi yazma araştırması

> Not: Buradaki kanıt kayıtları (arındırılmış ölçümler, ham analiz çıktıları) özel tutulur ve herkese açık depoda yer almaz.

`samsung_notes/ink_editor.py` gerçek stroke kanal ve frame biçimini okur;
PNG üzerine çizerek el yazısı düzenlemesi taklit etmez. İlk belge üreticisi
yalnız mevcut desteklenen bir stroke'un ötelenmiş kopyasını hazırlar. Serbest
kalem girdisi, fırça motoru ve genel mevcut çizim düzenlemesi henüz açılmadı.
Bu modül ağ isteği göndermez.

## Doğrulanan biçim

Cloud parçası `strokeResource`, MIME türü `application/octet-stream`, dosya
adı `UUID_modifiedTime`. XML `object(type=stroke,id,modifiedTime,hash)` ile
aynı kimlik/zamanı taşır. Hash bir içerik özeti değil,
`SHA256(UTF8(UUID + decimal modifiedTime))` değeridir.

Dış zarf `ffffffff`, LE32 header uzunluğu 4, LE32 format alanı, ardından
native frame zinciridir. Android örneklerinde dış format4000, Windows'ta
oluşturulan kontrollü007 örneğinde dış format0 gözlendi. İkisinde de iç
ortak frame format4000. Editör codec'i yalnız bu iki dış profili, iç4000
doğrulamasıyla kabul eder ve özgün zarfı korur. Genel eski `ink.py`
wrapper davranışı değiştirilmedi.

İlk frame type0 ortak nesne; ikinci frame type1 stroke. Her frame uzunluk,
type, flexible-data offset, değişken property/field maskları, sabit veriler
ve mask sırasındaki esnek alanlardan oluşur. Ortak frame UUID, modifiedTime,
çizilmiş sınırlar ve ek nesne metadata'sı içerir.

Stroke kanalları ayrı dizilerdir:

| Kanal | Sıkıştırılmış | Sıkıştırılmamış |
|---|---|---|
| X/Y | İlk iki f64, ardından işaret-büyüklük u16 Q5 delta çiftleri | Her nokta için iki f64 |
| Basınç | İlk f32, ardından işaret-büyüklük u16 Q12 delta | f32 dizisi |
| Zaman | İlk i32, ardından pozitif u16 delta | i32 dizisi |
| Eğim/yönelim | Property bit2 varsa ayrı f32 seed + Q12 dizileri | Bit2 varsa ayrı f32 dizileri |

Property bit0 hem curve hem sıkıştırılmış gösterimle ilişkilidir; sıradan
bir sıkıştırma seçeneği gibi değiştirilmez. Son iki sabit byte raw tool
türüdür. Nokta geri yüklemede her adım float32 hassasiyetinde birikir.
Encoder'ın ilk altkümesi ancak tam temsil edilen Q5/Q12 farklarını kabul
eder; sessiz kuantizasyon yapmaz. Aynı verinin tekrar yazımı özgün baytları
korur. Bu, native kayıplı reducer'ın tüm davranışını uyguladığımız anlamına
gelmez.

Style field1 advanced-setting string ID, field7 pen-name string ID,
field2 ARGB renk, field3 f32 kalınlıktır. ID'ler notun
`attributes/stringIdList/stringId(id,text=Base64UTF8)` tablosunda çözülür.
Stroke parçası tek başına tam bir fırça tanımı değildir. Diğer bilinen
esnek alanlar tipleriyle okunur; bilinmeyen bit, legacy partial-rectangle
verisi veya çözülemeyen string ID kapalı kalır. Esnek baytlar yeniden
kurulmak yerine özgün halleriyle korunur.

## İlk dar belge hazırlama sözleşmesi

```python
prepare_ink_clone(snapshot, source_stroke_id,
                 new_stroke_id=stable_uuid, dx=32, dy=32, now_ms=wall_clock_ms)
```

`InkAppendEdit` tam beklenen XML, tam noteTree, filtrelenmiş upload XML,
page/layer/new-object kimlik kümesi ve yeni binary parçasını döndürür.
Cloud adaptörü taze commit/snapshot kontrolü, tam typed compare sonucu,
kalıcı tek-gönderim kimliği ve yayın sonrası XML+binary geri okuma kontrolü
uygulamalıdır.

Başlangıç profili format4000, gruplanmamış normal FountainPen/InkPen2,
boş canvas cache, thumbnail=-1, sıfır ek/content-file ve floating object
ister. Eraser, replay/effect, kilit, şifreleme, beklenmeyen metadata/part
envanteri veya sayfa dışına taşan çizilmiş sınırlar reddedilir.

Klon ilk XY seed'i ve ortak çizilmiş sınırları aynı miktarda öteleyip
UUID ve tick'i yeniler. Bütün delta byte'ları, basınç, zaman, eğim,
yönelim ve brush metadata'sı korunur. Float32 birikimi öteleme hedefinden
saparsa işlem reddedilir. Böylece keyfi yeni çizgi için fırça kalınlığına
bağlı çizilmiş sınır hesabı uydurulmaz.

Yeni XML nesnesi ve tree öğesi eklenir; katman ve sayfa hash'leri sıralı
çocuk hash'lerinden yeniden hesaplanır. Önce özgün hash'ler de doğrulanır.
Sayfa drawnRect birleşimi genişler. Root sync/displayModifiedTime ms ile,
içerik tick'i ayrı ham tamsayı alanında ilerler. Diğer sayfalar, gövde,
başlık, string tablosu ve mevcut stroke'lar korunur. Upload yalnız istenen
sayfa/katman/yeni nesne deltasını ve yeni MIME stroke'unu taşır.

**Cache'i bulunan belgeler reddedilir.** Cache silme/yeniden üretme
sözleşmesi kanıtlanmadan eski resmi bırakmak veya tüm cache alanlarını
körlemesine silmek doğru değildir. Kontrollü007 örneğinde iki stroke,
iki sayfa, sıfır canvas-cache ve sıfır content-file görüldü; ilk deneme
bu mevcut dar profile uyuyor.

## Doğrulama ve sınırlar

- 11 sentetik codec/append testi: bağımsız elle kurulmuş golden kanal
  dizisi, UTF olmayan binary çerçeveleme, sınırlar, kuantizasyon reddi,
  metadata/part koruma, timestamp ayrımı, kaynak kimliği ve cache reddi.
- Özel mevcut cache'deki 12 stroke çözüldü; değişmeden byte roundtrip ve
  native UUID/time/hash + XML/string-table bağı 12/12 doğrulandı. Özel
  geometri, kimlik veya metin bu belgeye/repoya alınmadı.
- Kullanıcı APK'sının hashli `libSPenModel.so` dosyasındaki gerçek
  `sm_RestoreStroke(0x2ed234)` talimatları altı sentetik örnekte encoder
  çıktısını bütün beş kanalın float/int byte'larıyla doğruladı.
- Gerçek `NewApplyBinary(0x2ee650)` type1 okuyucusu 12 sentetik durumda
  compressed/raw × stylus açık/kapalı × 1/3/32 nokta için kanal, renk,
  kalınlık, pen/advanced-setting ID ve tool değerlerini doğruladı.
  Bellek önceden ayrılmış, native implementation alanları açıkça sağlanmıştı.
  Nesne constructor'ı, type0 okuyucusu, string manager veya fırça render'ı
  çalıştırılmadı. Bunlar tam uygulama yaşam döngüsü kanıtı değildir.
- Kontrollü007 snapshot üzerinde dx32/dy32 tam offline hazırlama başarılı.
  Bu modülün araştırmacısı Cloud'a yazma yapmadı; cihazda görünür yeni
  çizgi henüz bu checkpoint'in başarı ölçütü değildir.

Native deney yalnız sentetik veriyle, tek CPU, 256MiB adres alanı,
15 CPU saniyesi, çağrı başına100.000 talimat/2s sınırıyla çalıştı. Host
callback'leri sınırlı allocation/delete/memcpy/memset ve buffer-sınırı
kontrolüdür; decoder/property/style işlemleri native talimatlardır.
İlk harness denemesinde stack kopyası reddi ve eksik wrapper bağlamı
giderildi; yalnız tamamlanan kontrollü çağrılar başarı sayıldı.

Mevcut SPI preview provider `libSPenBase` raster codec'idir; stroke writer
yerine kullanılamaz. Tam yerel Spen writer için ayrıca object/context,
string manager, constructor, size/write ve brush cache yaşam döngüsü
gerekir. Bağımsız frame encoder mümkündür; native serialization/render
eşitliği, genel brush sınırları, serbest çizgi kuantizasyon politikası ve
cache güncellemesi ayrı doğrulama bekler.

Kaynak hash'leri/adresleri ve kanıt ayrımı:
özel kanıt arşivi.

## Serbest nokta girdisi: ikinci çevrimdışı aşama

`samsung_notes/ink_capture.py` mevcut strict codec/klon davranışını değiştirmeden
iki yardımcı sağlar: `normalize_capture` native sıkıştırma kaybını açıkça döndürür;
`basic_pen_drawn_bounds` yalnız döndürülmemiş FountainPen/InkPen2 profilinin
native sınırını hesaplar. `prepare_ink_append(snapshot, source_stroke_id,
new_stroke_id=..., samples=StrokeSamples(...), now_ms=...)` aynı cacheless
belge korumalarından geçen tam `InkAppendEdit` adayı üretir. Kaynak stroke
mevcut brush/style/string tablosunun şablonudur; eski stroke değiştirilmez.
Cloud adaptörüne veya uygulamanın düzenleme yeteneğine henüz bağlanmamıştır.

Native `SetPenType(0x2ea3c0)` bu iki kalem adını özel enum dallarında
bulamayınca enum2 seçer. `GetRectByPoint(0x2e9258)` float32 nokta min/max
kutusunu verir. `GetDrawnRect(0x2e9488)` enum2 için her yönde
`f32(f32(size * 0.5) + 4)` genişletir. Diğer kalemlerde farklı çarpanlar ve
paylar vardır; bu formül genel kalem sınırı diye kullanılmaz. Döndürme,
grup/container ve özel efekt profilleri bu aşamanın dışındadır.

Normalleştirme native `sm_ReduceStroke(0x2ec1a8)` ile karşılaştırıldı:

- XY girişleri float32 olur. İlk değerler f64 seed olarak yazılır. Sonraki
  noktalar, önceki **geri oluşturulmuş** float32 noktayla fark alınarak
  1/32 aralığına en yakına yuvarlanır; yarım aralıkta sıfırdan uzaklaşır.
- Basınç/eğim/yönelim float32 olur. Q12 farklar önceki **özgün** kanal
  değerinden hesaplanıp sıfıra doğru kesilir. Decoder bu farkları biriktirir;
  mutlak değerleri tek tek yuvarlamak eşdeğer değildir. Dönen sonuç hem
  canonical örnekleri hem ölçülen XY/basınç kaybını içerir.
- Zamanlar integer ms'dir. Android adapter/recording kaynak izi bunların
  event time eksi down time olduğunu gösterir. Bu istemcinin capture altkümesi
  ilk zamanı sıfır ister; bu, native biçimin genel zorunluluğu değildir.
  Negatif/sırası bozulan veya 65535 ms'yi aşan aralık sessizce clamp edilmez.
- İlk belge üretici stylus-axis kanalları istemez. Tarayıcının tiltX/tiltY
  alanları native tilt/orientation yerine doğrudan yazılamaz. Mevcut şablonun
  diğer property bitleri, tool türü, advanced settings ve fırça baytları korunur;
  axis-presence bit2, yeni iki eksensiz kanal dizisiyle tutarlı kapatılır.
- Yeni native bounds canonical noktalardan hesaplanır. Bu aday native
  uygulamanın ham hareketi filtreleyip/smooth edip kaydettiği bütün akışa
  eşdeğer olduğunu iddia etmez; yalnız eldeki canonical geometriyi yazar.

Önerilen UI taslak sözleşmesi `schemaVersion:1`, mevcut sayfa/brush-source
kimliği ve `samples:[{x,y,pressure,timeMs}]` dizisidir. X/Y belge koordinatıdır;
CSS pikseli veya ekran pikseli değildir. İlk capture yolu yalnız orientation=0
sayfayı kabul eder; diğer dönüşümler ayrıca doğrulanmalıdır. Basınç 0..1, zaman down başlangıcından
integer ms; ilk aşamada tilt alanları kabul edilmez. Kalıcı iş yeni UUID'yi
bir kere üretmeli, normalize sonucu ve kaynak commit'i dondurmalı; tekrar
çalıştırma yeni Cloud POST üretmemelidir. Henüz bir HTTP edit endpoint'i yoktur.

**Doğrulama:** 8 sentetik native reducer vakasında üretilen kanallar byte-byte
aynı; iki kalem × üç kalınlık dahil 48 native bbox karşılaştırması birebir.
Gerçek `GetRectByPoint`, `GetDrawnRect`, `SetPenType` ve Base `RectF` talimatları
çalıştırıldı. String karşılaştırması, refresh flag ve sıfır rotation bağlamı
harness stub'larıdır; renderer ve constructor yaşam döngüsü değildir. Yeni
capture/helper ve mevcut klon/Cloud-clone testleri birlikte 30/30 geçti.
Kontrollü007 snapshot üzerinde üç sentetik noktalı offline aday hazırlanabildi;
buluta gönderilmedi, kişisel nokta geometrisi repoya alınmadı.

**Cache sınırı değişmedi:** native `WForegroundHandler::ClearCanvasCacheData`
(0xcccd4) yalnız map'i boşaltmaz, media manager sanal metodunu çağırır ve ilgili
native durum bayraklarını günceller. Ayrı `ClearCanvasCacheDataMap(0xcd738)`
daha dar bir işlemdir. `n1.u` map boşsa XML öğesini atlar; bu tek
başına Cloud'daki eski media/contentFile ve thumbnail ilişkilerini güvenle
silme/yenileme sözleşmesini kanıtlamaz. Cached belgeler hâlâ reddedilir.

İkinci aşama kanıtı: özel kanıt arşivi.

## Canlı kalem önizlemesi (2026-10-27)

Çizilen çizgi artık sabit kalınlıklı SVG `polyline` değil: `crates/notes-ink-live`
(GPL-3.0, sabitlenmiş sdocx + resvg 0.47, WebAssembly) yazıcının saklayacağı
kaydı taklit eder (rs-ink Q5/Q12 nicelemesi, şablon kalem + recipe 3, boş
sayfada recipe 4) ve sayfa çizicisinin yoluyla ekran ölçeğinde çizer. Arayüz
modülü Web Worker'da çalıştırır; işlenmemiş son örnekler ve tahmini uç
(`getPredictedEvents`, hiç saklanmaz) o kare düz çizgiyle gösterilir, tam
raster gelince yerine konur. Kalem kalkınca ekrandaki pikseller aynı karede
çizginin kendi tuvaline geçer; taslak ve kuyruktaki çizgi aynı raster'ı
kullanır. Pen kaldırırken gelen basınç 0 örneği son temas basıncını alır ve
aynı konumdaki kalkış örneği saklanmaz (sıfır yarıçaplı damga sdocx Fountain
çizgisini tümden reddettiriyordu).

Şablon kalem `GET /api/notes/:id/ink-pens?revision=N` ile gelir:
`{schemaVersion:1,revision,pens:{<strokeId>:{width,height,rendering}}}`;
`rendering`, sayfa çizicisinin o çizgi için aldığı nesnenin bayt bayt aynısıdır
(notes-preview `pen_templates`, örnek geometrisi içermez). Uç yoksa, kalem
şablonu bilinmiyorsa veya `.wasm` eksikse eski SVG önizleme sessizce kalır.
Fosforlu kalem, silgi ve kement değişmedi. Derleme: `tools/build_ink_wasm.sh`
(ayrıntı: crates/notes-ink-live/README.md). Ölçüm: `node tools/ink_live_fidelity.mjs`.
