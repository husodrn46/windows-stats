# Windows Stats

Windows 10 ve Windows 11 için görev çubuğunda çalışan, hafif ve gizlilik dostu bir sistem izleme
uygulaması. macOS'taki [Stats](https://github.com/exelban/stats) uygulamasından esinlenmiştir ve
[winstats](https://github.com/mgvs/winstats) (Oleksandr Zhabotynskyi, MIT) temel alınarak
geliştirilmiştir.

> Durum: **Faz 1** (çalışan Windows uygulaması). Tasarım, ayarlar penceresi, sensörler ve kurulum
> paketi sonraki fazlarda eklenecek.

## Özellikler (Faz 1)

- Görev çubuğunun içine gömülü şerit (Windows 11 dahil); Explorer yeniden başladığında kendini
  yeniden bağlar.
- CPU (çekirdek başına çubuklar, P/E çekirdek ayrımı, geçmiş grafiği, frekans), RAM, GPU, disk,
  ağ ve pil widget'ları; tıklanınca ayrıntılı paneller (en çok kaynak kullanan işlemler, diskler,
  ağ bağdaştırıcıları, bellek modülleri).
- CPU yükü Görev Yöneticisi ile aynı yöntemle hesaplanır (`% Processor Utility`).
- Sağ tık menüsünden özelleştirme: widget seçimi, renkler, yazı boyutu, grafik genişliği, konum,
  güncelleme aralığı (0,5 / 1 / 2 / 5 sn), tema (sistem / koyu / açık), dil, Windows ile başlatma.
- Widget'lar sürüklenerek sıralanır; tüm ayarlar `%APPDATA%\WindowsStats\config.toml` içinde saklanır.
- Tamamen Türkçe arayüz (Windows dili Türkçe ise otomatik).

## Gizlilik ve güvenlik

- Telemetri, analitik veya otomatik güncelleme kontrolü **yoktur**; uygulama hiçbir ağ bağlantısı
  açmaz ve port dinlemez. (Ağ panelindeki gecikme ölçümü yalnızca kullanıcı `ping_host` ayarına bir
  adres yazarsa çalışır.)
- Yönetici izni istemez (`asInvoker`), sürücü kurmaz, sistem ayarlarını değiştirmez. "Windows ile
  başlat" yalnızca kullanıcıya ait `HKCU\...\Run` anahtarını kullanır.
- Ölçümler ayrı bir iş parçacığında yapılır; görev çubuğuna gömülü pencere hiçbir zaman ölçüm
  beklemez (Explorer'ın donmasını önlemek için).

## Derleme

Gereken: Rust (GNU hedefi) ve kaynak derleyici `windres`. Proje, yönetici izni gerektirmeyen yerel
bir araç zinciriyle derlenir (`.toolchain`, git'e dahil değildir):

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Test
```

Çıktı: `target\release\WindowsStats.exe` ve `dist\WindowsStats-<sürüm>-x64.zip`.

## Komut satırı

```
WindowsStats.exe                  # başlat
WindowsStats.exe --dump 10 1000   # 10 ölçümü JSON satırları olarak yazdır (test için)
WindowsStats.exe --quit           # çalışan uygulamayı temiz biçimde kapat
WindowsStats.exe --version
```

## Testler

```powershell
cargo test --release                                                   # birim testleri
powershell -ExecutionPolicy Bypass -File scripts\compare-taskmgr.ps1   # Görev Yöneticisi sayaçlarıyla karşılaştırma
```

## Lisans

MIT. Ayrıntılar ve kullanılan açık kaynak bileşenler: [LICENSE](LICENSE),
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
