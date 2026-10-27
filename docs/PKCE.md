# Çevrimdışı PKCE S256 parçası

> Not: Buradaki kanıt kayıtları (arındırılmış ölçümler, ham analiz çıktıları) özel tutulur ve herkese açık depoda yer almaz.

`notes_protocol::pkce`, bağımsız istemci için standart bir hazırlık parçasıdır.
Samsung'a bağlanmaz; giriş URL'si, callback, token isteği veya kullanıcı arayüzü
oluşturmaz. Windows Account sabitlerinde `code_verifier`, `code_challenge` ve
`code_challenge_method` bulunması araştırma gerekçesidir. Bu kod, Samsung'un
S256'yı veya bağımsız Linux istemcisini kabul ettiğini kanıtlamaz.

`CodeVerifier::generate()` işletim sisteminden 32 rastgele bayt alır ve
padding içermeyen base64url ile 43 karaktere dönüştürür. RNG hatası doğrudan
`RandomUnavailable` olur; zayıf kaynak veya `plain` fallback yoktur.
`from_secret()` yalnız 43–128 ASCII unreserved karakter kabul eder; boşlukları
silmez ve Unicode normalleştirmez. Bu kontrol entropiyi ölçmez. Yeni işlem
için `generate()` kullanılmalıdır. `s256_challenge()` SHA-256 sonucunu
padding içermeyen base64url olarak döndürür; yöntem sabiti `S256_METHOD`'dır.
[RFC 7636 §4.1–4.2 ve Appendix B](https://www.rfc-editor.org/rfc/rfc7636.txt).

Verifier özel alanda `Zeroizing<String>` içinde tutulur; `Debug` yalnız
`CodeVerifier([REDACTED])` yazar. `Display`, otomatik serialization ve `Clone`
yoktur. `expose_secret()` gelecekteki token isteği için açıkça çağrılması
gereken erişimdir; sonucu loglanmamalıdır. Verifier ve rastgele bayt tamponu
drop sırasında temizlenir. Çağıranın `from_secret()` girdisi, işletim sistemi
kopyaları ve kripto işlem ara durumları için tam bellek temizliği garantisi
verilmez. [Zeroizing belgesi](https://docs.rs/zeroize/1.9.1/zeroize/struct.Zeroizing.html).

Bağımlılıklar Cargo.lock ile sabitlenmiştir:

| Kitaplık | Sürüm | Kullanım |
| --- | --- | --- |
| getrandom | 0.4.3 | İşletim sistemi CSPRNG'si; hata döndüren `fill` |
| sha2 | 0.10.9 | RustCrypto SHA-256 |
| base64 | 0.23.1 | URL-safe, padding'siz kodlama; yalnız alloc özelliği |
| zeroize | 1.9.1 | Sahip olunan gizli tamponları drop sırasında temizleme |

[getrandom](https://docs.rs/getrandom/0.4.3/getrandom/fn.fill.html),
[sha2](https://docs.rs/sha2/0.10.9/sha2/) ve
[base64](https://docs.rs/base64/0.23.1/base64/) API'leri kullanılır; kriptografik
algoritma yeniden yazılmadı. `sha2` 0.11.1'in paket manifesti Rust 1.88
istediğinden projenin 1.85 hedefi için 0.10.9 seçildi. Linux bağımlılıklarının
bildirdiği MSRV'ler 1.85'i aşmıyor; bazı eski geçişli paketler MSRV bildirmiyor.
Derleme/test mevcut Rust **1.99.1** ile yapıldı; **Rust 1.85 üzerinde derleme
henüz doğrulanmadı**. Workspace `rust-version = "1.85"` korunuyordu.
2026-10-27 bağımlılık güncellemesinde (parking_lot 0.13 → Rust 1.95, rusqlite 0.41 →
1.89) workspace `rust-version` 1.95'e çıkarıldı; `sha2` 0.10.9'da kalıyor (ayrı karar).

Doğrulama: altı PKCE testi RFC Appendix B vektörünü, sınır uzunlukları,
unreserved karakterleri, geçersiz girdi reddini, üretilen değerin boyutunu ve
Debug/hata gizliliğini kapsar. Bir compile-fail doctest `Display` bulunmadığını
kontrol eder. Rastgele değer testi yalnız biçim/OS kaynağı erişimini kontrol
eder; istatistiksel entropi ölçümü değildir. Workspace'te **33 test ve 1
doctest geçti**; Clippy `-D warnings` ve format kontrolü temiz.
Kanıt: özel kanıt arşivi.

Güvenlik tasarımında OWASP ASVS 10.1.2, 11.2.1 ve 11.5.1 kapsamı esas alındı:
yeni verifier üretimi, hazır kripto kitaplıkları ve güçlü rastgelelik.
Transaction/state bağlama, callback doğrulama, tek kullanım ve oturum yaşam
döngüsü henüz uygulanmadı; bu parça tek başına güvenli bir OAuth istemcisi
veya ASVS uyumluluk değerlendirmesi değildir.
