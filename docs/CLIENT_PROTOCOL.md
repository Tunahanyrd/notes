# Bağımsız istemci: ilk protokol çekirdeği

> Not: Buradaki kanıt kayıtları (arındırılmış ölçümler, ham analiz çıktıları) özel tutulur ve herkese açık depoda yer almaz.

Hedef Rust çekirdek ve Tauri/React masaüstü uygulaması. Rust kitaplığı XML ve
giriş kriptografisini, Python araştırma araçları sınırlı HTTP akışını yürütüyor.
Linux'ta kullanıcı girişi, şifreli callback, UAT ve Notes'a özel access token
edinme **gerçek sunucuda HTTP 200 ile doğrulandı**. Refresh alanı `-1` sentinel;
ilk aşamada kalıcı yenileme doğrulanmadı. Sonraki UAT yenileme kanıtı aşağıdadır.
Ayrı Cloud tokenı alındı, ilk 51 değişiklik ve
bir notun metadata/karşılaştırma XML'i indirildi. XML/multipart çözümleyici ve
sınırlı düz metin yazıcı eklendi: Linux'tan oluşturulan kontrollü test notu
cihazda görüldü; cihazdaki metin değişikliği aynı not kimliğiyle Linux'a döndü.
Son Linux güncellemesi, sunucu zamanını ilerleten düzeltmenin ardından cihazda
da görüldü; kullanıcı açıkça teyit etti.
Tauri/React metin uygulaması ve kalıcı Python eşitleme servisi artık mevcut.
Genel not/kalem düzenleme ve tam Samsung Notes özellik eşitliği henüz yok.
(Canlı giriş ve gerçek cihazla düz metin aktarım kanıtları özel tutulur.)

## Uygulama katmanı — 25 Ekim 2026

`samsung_notes/` uygulama servisi SQLite kütüphanesi, kalıcı cursor ve değişmez
işlem kimlikli gönderim kuyruğu kullanır. Arayüz gerçek API'den okur; Samsung
tokenı arayüze verilmez. Açma ve sınırlar [README](../README.md), HTTP sözleşmesi
[APP_CONTRACT](APP_CONTRACT.md) içinde.

İlk uygulama aşamasında 78 uygulama/edinme/önizleme testi ve 79 araştırma aracı testi geçti. Ön yüzün sentetik
tarayıcı, gerçek çevrimdışı servis ve native süreç testleri ayrı kayıtta tutulur.
Uygulamanın HTTP API'si ile kontrollü bir not oluşturma ve düzenleme gerçek
Cloud'a gönderildi; her iki sonuçta da içerik geri okumayla eşleşti. Native
uygulama açılışı ve yeniden başlatma sonrası 33/33 notun, hesabın ve son
kontrollü düzenlemenin korunması doğrulandı. Uygulama Wayland penceresi olarak
çalıştı. Bu uygulama katmanı deneyinde ayrıca Android ekran teyidi istenmedi.

Başarılı gönderim teyidinden sonra başarısız okuma, tekrar yazma yerine aynı
işlem kaydından yalnız okumayla uzlaştırılır. Sonucu bilinmeyen gönderim otomatik
tekrarlanmaz. Yerel düzenleme sürüm kontrolü ve çakışmada kopya koruma testlidir;
sunucuda atomik compare-and-swap olduğu iddia edilmez. Geçerli hesap UAT'sinden
yeni Notes yetkilendirme kodu ve Cloud oturumu alma yolu canlı HTTP 200/200/200
ile doğrulandı. Sunucu henüz geçerli aynı Notes tokenını/son tarihini döndürdü;
süresi dolmuş token yenilenmesi ayrıca doğrulanmış değildir.
(Oturum yenileme kanıtı özel tutulur.)

Favoriler, not çöp kutusu/geri alma, klasörler ve not taşıma artık Cloud
işlemleridir. Uygulamanın HTTP API'sinden oluşturulan ayrı test notu/klasöründe
11 işlem, zorunlu sunucu geri okumasıyla geçti. Kullanıcı ayrıca gerçek arayüz
çöp kutusu işleminin buluta ulaştığını teyit etti. Kalıcı silme, dolu klasör
silme ve kaybolan klasör yolunu yeniden oluşturma desteklenmez.
(Protokol kanıtı ve uygulama doğrulaması özel tutulur.)

Bağımsız başlık yazımı ve desteklenen zengin metin notları da gerçek Cloud
üzerinde doğrulandı. Kontrollü rich notun kalın/italik/kırmızı metni ve sağa
hizalaması Android Samsung Notes ekranında kullanıcı tarafından teyit edildi;
sağlanan ekran görüntüsüyle ayrıca karşılaştırıldı. Bu tek notun doğrulaması,
bütün stillerin veya kalem yazıcısının doğrulandığı anlamına gelmez.
(Editör kanıtı özel tutulur.)

Büyük sabit bir önizleme girdisinde eşlenmiş ölçüm: 31,367 → 1,302 saniye duvar
saati, 29,045 → 1,296 saniye CPU. Çıktı adaylarının kimlik/hash/boyutları aynı
kaldı; bu tek ayrıştırma ölçümüdür, bütün notların açılma süresi değildir.

Büyük çizimli notların sınırlı XML/multipart okuması 16.384 parçaya kadar açılır;
1.024 parçadan büyük belgeler düzenlenmez. Başlık/metin önizlemesine ek olarak
kaydedilmiş canvas cache dosyaları hash doğrulamasıyla indirilip sınırlı yerel
Samsung codec'iyle PNG'ye açılır. Bu, bütün çizim/PDF nesnelerini yeniden çizen
bir belge motoru değildir. Dosya yönlendirmesinde hesap başlıkları taşınmaz;
hesap veya kaynak değişince sonraki indirme/yayımlama adımı durur. Ayrıntı:
[APP_CONTRACT](APP_CONTRACT.md).

Aşağıdaki kontrollü protokol deneyleri uygulama katmanından önceki kanıtlardır.

`tools/notes_payload.py` bounded XML/multipart ayrıştırır ve özel yerel cache
üretir; SDOCX veya çizim renderer'ı değildir. `tools/notes_writer.py` yalnız
yeni, tek sayfalı düz metin not oluşturur. `tools/notes_sync_probe.py` uzaktan
yazmayı bu istemcinin oluşturduğu kontrollü nota sınırlar. İndirilen kişisel
notlar değiştirilmedi. Aynı test notuna kontrollü güncelleme aşağıda doğrulanmıştır.

İlk geri yazma hazırlığı cihazdan gelen ikinci boş sayfayı yakalayıp çevrimdışı
durdu; sunucuya update gönderilmedi. Bu nedenle özgün tek sayfalı taslağı yeniden
kurmak yeterli değil. Cihazın son XML yapısı korunmalı; `noteTree` tam envanter
olarak gönderilirken veri parçaları compareForUpsync sonucuna göre seçilmeli.
Android `k1.a.m/n` bu ayrımı uyguluyor; aynı protokolü kullanmak sayfa kaybını önler.

## Aynı notu güncelleme — canlı doğrulandı

`tools/notes_snapshot_update.py`, bu istemcinin oluşturduğu notun son cihaz
snapshot'ını esas alır. İki boş sayfanın kimlikleri ve içeriği, başlık, metadata
ve mevcut metin işaretlemeleri korunur. Compare sonucu yalnız gövdeyi istiyorsa
tam noteTree ile gövde parçası gönderilir; beklenmeyen kaynak veya farklı commit
güncellemeyi durdurur. İstek sonucu belirsizse otomatik tekrar yoktur.

İlk denemede compare 200, update 403/128409 döndü: sunucu zamanı ile iç nesne
zamanı aynı max hesabına alınmıştı. Gerçek Android snapshot'ında bazı iç
değerler 16 haneli, root syncModifiedTime 13 hanelidir. Yeniden okuma reddin
ardından not XML'inin ve commit'in değişmediğini doğruladı. Bu başarısız deneme
korundu; aynı işlem kaydı tekrar kullanılmadı.

Native `k1.a.k → sync.db.t.j → entity.getServerTimestamp` ve `k1.a.n` zinciri
root syncModifiedTime'ın saklanan sunucu zamanından geldiğini gösteriyor.
İlk düzeltme bu alanı bire bir korudu; body/attributes/tree-body için ham iç
zamanın maksimumunu bir artırdı. Ayrı kayıtla yapılan compare 200 ve update 201
döndü. Sonraki downsync 200 yanıtında:

- Cihazdan gelen metin aynen korunmuş, Linux satırı eklenmiş.
- İki sayfa/layer kaynağı ve tam sayfa ağacı semantik olarak değişmemiş.
- Yerel not kimliği aynı; commit ve fileRevision yazma sonucuyla eşleşmiş.

(Reddedilen deneme, düzeltme kaynağı ve başarılı geri okuma kayıtları özel tutulur.)
Bu ara aşamada Linux satırının cihaz ekranında görünmesi kullanıcı tarafından
teyit edilmemişti. Kullanıcı bu kontrolde metni yeniden düzenlediğini bildirdi;
sonraki cihaz sürümü ayrıca indirilip korundu. Kullanıcı not listesindeyken
yapılan ayrı denemede içerik tekrar 201/200 ile doğrulandı, fakat hemen alınan
GetNoteInfoView.syncModifiedTime ilerlememişti. Android `c4.f` eşit sunucu
zamanında yalnız commit'i günceller; içeriği indirmez. Bu nedenle içerik geri
okuma başarısı tek başına cihazın güncellemeyi alacağını kanıtlamıyor.

Önceki çıkarımın düzeltmesi: serializer saklanan zamanı korusa da normal yerel
kayıt öncesinde `WDocSaveOperation.setDocumentTimeEntity`, keepServerTimestamp
false iken `TimeManager.getCurrentTime()` ile ServerTimestamp'ı ilerletir.
`TimeManager` sunucu farkıyla düzeltilmiş epoch-ms kullanır; SDK zamanını
uygulama zamanına çeviren kod 16 haneli SDK zamanını 1000'e böler. Ürün düzeyi
saat farkı yönetimi henüz yoktur. Bu oturumda sunucu timestamp'ı ile yerel
isteğin saniyeye yuvarlanmış zamanı arasında 514 ms gözlendi; ölçüm ağ gecikmesini
ve yuvarlamayı içerir, kesin saat farkı değildir.

Schema 5 düzeltmesi root syncModifiedTime ve displayModifiedTime için yeni
epoch-ms, iç body/attributes/tree-body için ayrı ham sayaç kullanır. Önceki
başarılı kontrollü revision ile commit, dosya hash'i, başlık/metin ve tüm kimlik
envanteri eşleşirse metne dokunmadan yeniden yayımlama desteklenir. Böylece
Linux satırı iki kez eklenmez. Bu onarım compare 200 → update 201 → info 200 →
downsync 200 ile uygulandı. Bu kez GetNoteInfoView.syncModifiedTime gerçekten
ilerledi ve hazırlanan zamanla eşleşti; metin, iki sayfa ve yeni commit tekrar
doğrulandı. Kullanıcı not listesindeyken eşitleyip notu yeniden açtı ve
**“Evet, artık görünüyor”** diyerek Linux satırının cihazda göründüğünü teyit etti.
Kontrollü düz metin döngüsü böylece gerçek cihazda tamamlandı.
(Düzeltme, onarım/geri okuma ve cihaz teyidi kayıtları özel tutulur.)

Bu, genel amaçlı not düzenleme veya atomik çakışma çözümü
kanıtı değildir: compare ile update arasında yarış penceresi vardır.

Bu aşamadaki doğrulama: Rust çalışma alanında 65 test (doctest dahil), Python
araştırma araçlarında 79 test geçti; Rust format ve Clippy kontrolleri temiz.
Canlı istekler test sayısına dahil edilmedi. O aşamadaki eksikler: tam envanter ve
kalıcı cursor, oturum yenileme, çevrimdışı kuyruk, silmeler/çatışmalar, ekler ve
çizimler, kayıpsız genel editör, Tauri arayüzü.

Eski tek sayfa yeniden kurucusu `tools/notes_update.py` canlı yazma yolu
kapatıldı; yardımcıları ve sentetik tarihsel testleri korunuyor. Canlı doğrulanan
yol `tools/notes_snapshot_update.py` içindedir.

Aşağıdaki bölümler önceki aşamaların kapsamını ve kanıtını korur.

## Kaynak ve kapsam

Android Notes 4.4.45.37 APK SHA-256:
`daed1eff8c8ee9dfb8afe2771e39e893a8808f3230d6d522a8aa647db09b8667`.
Android uygulamasındaki değişiklik okuyucu (`b4.a`) `NoteChangesView`, `hasNext`, `lastChangePoint` ve not alanlarını okuyor.
`networkutils.l.d()` HTTP GET seçiyor. İlgili uç
`/samsungnotes/v1/notes/changes` veya `/samsungnotes/v3/notes/changes` ve query
alanı `lastChangePoint`. Kullanılacak sürümün sunucu/hesap pazarlığı henüz
çözülmedi; istemci kendiliğinden birini seçip istek göndermiyor.

`notes-protocol` cursor ve commit değerlerini değiştirmeden korur; eksik
silinme bayrağını false'a çevirmeden `None` tutar. Bilinmeyen elemanların ham
XML parçaları giriş sırasıyla korunur. Bu parçalar bağımsız, yeniden
gönderilebilir XML belgeleri değildir: kökteki namespace bildirimlerine
bağlı olabilirler. Ayrıştırıcı tam XML roundtrip veya kayıpsız not editörü
iddiası taşımaz. Not düzeyindeki `isEncrypted` ile EDP'nin noteTree üzerindeki
`encrypted` niteliği aynı kavram olarak kullanılmayacak.

2 MiB giriş ve 50.000 XML düğümü sınırı, zorunlu pagination/kimlik alanları,
yinelenen bilinen alanların reddi ve açık boolean/numeric kontrolü bizim
istemci doğrulama politikamızdır. Bunlar Samsung sunucusunun eksiksiz şeması
olarak sunulmaz. DTD entity tanımları kabul edilmez, dış entity resolver yoktur.
Bir hata mesajı not kimliğini veya yanıt içeriğini yazdırmaz.

`a1.t.Q/R/S/T` yardımcılarıyla karşılaştırma sonucunda boolean ve sayılarda
whitespace normalizasyonu kaldırıldı. Boolean için yalnız true/false
(büyük-küçük harf duyarsız) kabul edilir; Android diğer metinleri false'a
çevirse de bizim doğrulama politikamız belirsiz girdiyi reddeder. Sayılar
decimal veya küçük `0x` önekli hex olarak okunur; zaman signed 64-bit,
SDK sürümü signed 32-bit. Android'in bozuk SDK değerini sıfıra düşüren
fallback'i burada uygulanmaz, açık hata verilir. Bu farklar önce başarısız
regresyon testleriyle gösterildi, ardından düzeltildi. Bu ilk aşamada 15 test geçti;
Clippy `-D warnings` temiz. Kanıt: özel kanıt arşivi.

Fixture elle oluşturulmuş sentetik veridir. Birden çok `changes` elemanını
koruma davranışı testlidir; gerçek sunucunun bir sayfada birden çok not
gönderdiği henüz gözlenmedi. Android uygulamasının bu alanı nasıl doldurduğu gerçek
yanıtla karşılaştırılmalı. Mevcut testler canlı protokol uyumluluğunu kanıtlamaz.

## Çalıştırma

```sh
cargo test --workspace --locked
cargo run -p notes-cli --locked -- inspect-changes crates/notes-protocol/tests/fixtures/changes.xml
cargo run -p notes-cli --locked -- inspect-tree crates/notes-protocol/tests/fixtures/tree.xml
```

CLI yalnız sayıları ve pagination durumunu gösterir; not ID'si, cursor veya
alan içeriklerini ekrana yazmaz. Girdi dosyasını değiştirmez. `sync` gibi
henüz uygulanmamış bir komut hata verir.

## Not ağacı

`tree::parse_note_tree` artık `noteTree` ve `CompareForUpsyncView` köklerini,
sayfa → katman → nesne hiyerarşisini, içerik dosyalarını, yüzen nesneleri ve
gövde metni kaydını ayrıştırıyor. Kaynak `n1.o`;
bu bir belge renderer'ı veya fark hesaplama motoru değil. Hash değerleri
yorumlanmadan korunur. Ağacın `ver` ve `t` alanları kaynakta decimal okunur;
changes yanıtındaki hex desteği bu alanlara uygulanmaz.

Özgün XML namespace ve bilinmeyen nitelikleriyle saklanır; değiştirilip tekrar
gönderilebilen bir serializer henüz yok. 2 MiB / 50.000 düğüm sınırına ek
64 derinlik, 4.096 varlık ve alan/ek veri sınırları uygulanır. Bunlar istemci
politikasıdır. CLI yalnız yapı sayılarını gösterir, saklanan XML'i yazdırmaz.
11 ağaç testi ve ek CLI testiyle çalışma alanında **27 test geçti**;
Clippy `-D warnings` temiz. Yeni kanıtlar `tests-with-tree.log` ve
`clippy-with-tree.log`; sentetik testler canlı sunucu uyumluluğunu kanıtlamaz.

## Sonraki kanıtlar

1. Samsung Account kullanıcı etkileşimi, yönlendirme, oturum yenileme ve
   bağımsız uygulamanın kabul koşulları.
2. Hesap token'ından ayrı SCSP Cloud token edinme ve ERS adres seçimi.
3. Gerçek, kontrollü `NoteChangesView` yanıtıyla parser karşılaştırması.
4. NoteTree/metadata, binary dosyalar, upload URL ve commit ilişkisi.
5. Test notu indirip açma; kontrollü değişikliği geri gönderme, ardından
   çevrimdışı düzenleme, silme ve çakışma davranışı.

Windows VM kuruldu; resmi Notes hesabı tanıdı ve not indirmeye başladı.
Çalışan resmi Notes ve Android cihazları
referans oturumunda (özel, depoda yok) aynı test notuyla karşılaştırılacak.

## Hesap hazırlığı

[PKCE S256](PKCE.md) için çevrimdışı primitive eklendi. 32 bayt işletim sistemi
rastgeleliği, strict RFC verifier kontrolü ve gizli Debug davranışı testli.
Toplam 33 test + 1 doctest geçti; Clippy/format temiz. Bu modül HTTP veya
Samsung giriş uyumluluğu sağlamaz. Hesap araştırması (özel tutulur, depoda yok)
gerçek GET başlangıç isteğini, yerel paket kontrolünü ve kalan belirsizlikleri
ayrı tutar. Test notu transferini beklemeden protokol çalışması sürer.

Sonraki aşamada `entrypoint` modülü ve `inspect-entrypoint FILE.json` komutu
eklendi. İstek hazırlayıcı GET / countryCode / Basic(appId + boş parola)
sözleşmesini uygular, HTTP göndermez; response parser sınırlı JSON metadata
okur. Python probe ile Linux'tan alınan HTTP 200 yanıtı Rust CLI ile de
ayrıştırıldı. Bu aşamada **42 test + 1 doctest** geçti; Clippy/format temizdi.
Metadata parser tek başına giriş URL'sini veya RSA anahtarını doğrulamaz.

Sonraki `signin::PreparedSignIn` katmanı sabit Samsung giriş URL'si ve DER/RSA
kontrollerini ekledi. 34 alanlı profile yeni PKCE/state yerleştirir; bağımsız
Java fixture'ıyla doğrulanan KDF/AES/RSA zincirinden sonra çift URL kodlaması
uygular. `notes-cli prepare-signin ENTRYPOINT.json PROFILE.json NEW_OUTPUT_DIR`
çevrimdışı olarak özel URL ve V2 transaction dosyalarını oluşturur. Profildeki
kimlikler kendiliğinden düzeltilmez: Notes IPC kimliği/callback'i ile Account
web OAuth girdileri farklıdır; ayrıntılar özel hesap araştırması raporundadır (depoda yok).

Python probe üzerinden gönderilen doğru web profiline sunucu HTTP 307 verdi;
gönderilen state, iki client ID, callback ve response_type karşılaştırması
başarılı. Kullanıcı hesabıyla giriş yapılmadı. `tools/capture_oauth_callback.py`
stdin'den verilen URI'yi işlem hedefine/state'e/süreye bağlar, geçerli dönüşü
bir kez özel dosyaya kaydeder. Masaüstü URI handler kaydı ve token isteği yok.
Güncel toplam **49 Rust testi + 1 doctest ve 6 Python callback testi** geçti.
Sentetik Rust CLI → Python callback capture → replay reddi de çevrimdışı
kontrol edildi. Bu testler gerçek callback teslimini veya token kabulünü
kanıtlamaz. Clippy `-D warnings` temiz.
