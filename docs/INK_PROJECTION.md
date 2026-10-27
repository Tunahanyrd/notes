# Kalem okuma sözleşmesi

`project_ink(snapshot)` ağ veya dosya yazmadan sınırlı kalem geometrisi döndürür:

```text
{
  schemaVersion: 1,
  pages: [{id, index, width, height, strokes: [
    {id, kind: "fountain" | "pen", colorARGB, size,
     points: [{x, y, pressure, timeMs}]}
  ]}],
  capabilities: {appendInk: boolean},
  reason: null | string,
  rendering: {nativeEquivalent: false, mode: "geometry-only"}
}
```

X/Y belge birimindedir; `size` özgün SDK kalem boyutudur, CSS stroke width
olarak eşdeğer değildir. Nokta sırası ve katman sırası korunur; sayfa çizgileri
katmanların belge sırasıyla düzleştirilir. `timeMs` kaydedilmiş native göreli
zamanı aynen taşır; ilk kaydedilen örnek her notta sıfır olmak zorunda değildir.
Ayrı HTTP endpoint'i veya UI entegrasyonu bu modülün kapsamında değildir.

Tek kaynak resource/commit, canonical UUID, binary filename/time, native
UUID/time/hash, string tablosu, layer/page sıralı hash ve noteTree eşleşmesi
kontrol edilir. Kaynak `DocumentSnapshot.resource_xml` ile ham MIME XML'i
ayrılırsa projection reddedilir. Signed URL, medya yolu, native binary veya
advanced-setting ham verisi DTO'ya alınmaz. DTO'nun kendisi özel not verisidir;
loglanmamalı veya paylaşılan test kayıtlarına yazılmamalıdır.

Başlangıç sınırı 32 sayfa, 256 stroke, toplam 200.000 noktadır. Yalnız temel
FountainPen/InkPen2, orientation=0 ve desteklenen normal stroke frame profili
kabul edilir. Bilinmeyen kalem veya karışık desteklenmeyen nesne envanteri
sessizce atılıp eksik sayfa gösterilmez: `pages=[]`, `appendInk=false` olur.
Native boş katmanın tree objectList'inin hiç yazılmaması desteklenir.

Cache içeren, ancak nokta envanteri okunabilen notlarda geometri dönebilir;
`appendInk=false` kalır. `appendInk` kapısı ayrı bir yaklaşık koşul listesine
bağlı değildir: mevcut `prepare_ink_append` üreticisi sentetik bir merkez
noktayla **çevrimdışı** denenir, aday atılır ve hiçbir yere yayınlanmaz.
Bu kontrol kaynak profil, metadata ve hash korumalarını tekrar kullanır.
HTTP entegrasyonu projection sonucunu aynı not revision/commit'i için
önbelleğe almalı; her UI poll'unda tekrar MIME çözmemelidir.

## Görüntü doğruluğunun sınırı

Bu DTO doğrudan native kalem görünümüne eşdeğer SVG/canvas değildir. Mevcut
Samsung cache/PDF preview yolu, özgün sayfayı göstermek için kullanılmaya devam
etmelidir. Merkez çizgisini `lineWidth=size` ile çizmek, kullanıcıya gerçek
kalem görüntüsü diye sunulamaz. Basınç kanalının bulunması tek başına doğru
pressure-brush renderer yapıldığı anlamına gelmez.

Pinned sdocx araştırmasının `stroke-rendering-findings.md` kaynağı, FountainPen
`18;0;100;` ayarı için V16 geometri yolunu daraltıyor: sample-acceptance/tolerance,
üç örneklik yön/geometri geçmişi, basınç, tilt, tool türü, zaman tabanlı width
smoothing, midpoint quadratic eğriler ve mesafe aralıklı dairesel stamp'ler.
Kaydedilmiş stroke redraw, native giriş `0x7851c` üzerinden width toplama ve
smoothing aşamalarını çalıştırıyor. Pinned prior art içinde bağımsız Rust
geometri portu ve native oracle kayıtları var; bunlar bizim yeni Python
projection'ının render ettiği veya her brush sürümünü kapsadığı anlamına gelmez.

Prior art genel fallback'inin basınçla ölçeklenmiş düz SVG segmentleri,
köşelerde ve genişlik geçişlerinde gerçek native geometriden sapabildiği ayrıca
belgelenmiş. Genel midpoint spline da sample filtering/width law/tolerance
olmaksızın tam karşılık değildir. Kenar antialiasing ve compositor eşitliği,
geometri eşitliğinden ayrı doğrulama gerektirir.

Kendi yeni kanıtımız yalnız kanal encoder/decoder ve basic-pen bbox ile
sınırlıdır: `ink-freehand-v1` native reducer8/bbox48 eşleşmesi. Projection için
6 sentetik test ve kontrollü007 özel snapshot'ın 2 sayfa/2 stroke/append-profile
okuması başarılıdır. Kişisel geometri, kimlik ve başlık araştırma kayıtlarına
alınmamıştır. Renderer veya canlı kalem yazma başarı iddiası yoktur.
