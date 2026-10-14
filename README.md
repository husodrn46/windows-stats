# Windows Stats

Windows 10 ve Windows 11 için görev çubuğunda çalışan, hafif ve gizlilik dostu bir sistem izleme
uygulaması. macOS'taki [Stats](https://github.com/exelban/stats) uygulamasından esinlenmiştir ve
[winstats](https://github.com/mgvs/winstats) (Oleksandr Zhabotynskyi, MIT) temel alınarak
geliştirilmiştir.

- **Görev çubuğunun içinde** çalışan şerit: CPU (çekirdek başına çubuklar, P/E çekirdek ayrımı,
  geçmiş grafiği), GPU, RAM, disk, ağ, pil ve sıcaklık widget'ları. Windows görev çubuğuna gömmeye
  izin vermezse aynı yerde **yüzen** şerit olarak çalışır.
- Widget'a tıklayınca **ayrıntılı panel**: geçmiş grafikleri, en çok kaynak kullanan işlemler,
  çekirdekler ve frekanslar, bellek modülleri, fiziksel diskler (SSD/HDD, sıcaklık), sürücüler, ağ
  bağdaştırıcıları (Wi-Fi ağı, IP, MAC, hız), pil, sensörler.
- **Ayarlar penceresi** (sağ tık > Ayarlar…): widget seçimi ve sıralaması, renkler, tema
  (sistem/koyu/açık), yazı ve grafik boyutları, güncelleme aralığı (0,5 / 1 / 2 / 5 sn), konum,
  Windows ile başlatma, eşik bildirimleri, sensörler, gizlilik.
- **Bildirimler**: örneğin "İşlemci sıcaklığı 30 sn boyunca 85 °C üzerinde kalırsa uyar".
  Ölçülemeyen değerler hiçbir zaman bildirim üretmez.
- Tamamen **Türkçe** arayüz (Windows dili Türkçe ise otomatik), Türkçe birimler (KB/sn, 1,5 GB).

## Kurulum

Yönetici izni gerekmez, yalnızca sizin kullanıcı hesabınıza kurulur.

1. `WindowsStats-<sürüm>-x64.zip` dosyasını bir klasöre açın.
2. `Kur.cmd` dosyasına çift tıklayın. Program `%LOCALAPPDATA%\Programs\Windows Stats` klasörüne
   kopyalanır, Başlat menüsüne eklenir ve isterseniz Windows ile birlikte başlar.
3. **Kaldırma:** Ayarlar > Uygulamalar > Windows Stats > Kaldır. Görev çubuğu eski haline döner;
   ayarlarınızın silinip silinmeyeceği sorulur.

Kurmadan denemek için `WindowsStats.exe` doğrudan da çalıştırılabilir.

## Kullanım

- **Sol tık:** widget'ın ayrıntılı paneli. **Sağ tık:** hızlı ayar menüsü (en üstte *Ayarlar…*).
- **Sürükle-bırak:** widget'ları şerit üzerinde sürükleyerek sıralayın.
- **Çift tık:** Görev Yöneticisi.
- Windows 11'de şerit, görev listesinin düğmelerine binmez. Seçilen tarafta yer yoksa diğer
  tarafa geçer, orada da yetmezse sondaki widget'lar yer açılana kadar gizlenir.

## Sensörler

| Değer | Kaynak | Yönetici izni / sürücü |
|---|---|---|
| Disk sıcaklığı, SSD/HDD | Windows depolama sorgusu | Gerekmez |
| GPU sıcaklığı, fan | Ekran kartı sürücüsü (Görev Yöneticisi'nin kaynağı) | Gerekmez (harici GPU'larda) |
| GPU saat hızı, voltaj, ek değerler | LibreHardwareMonitor yardımcısı | Gerekmez |
| İşlemci sıcaklığı, güç, fanlar, anakart voltajları | LibreHardwareMonitor + PawnIO sürücüsü | **Gerekir** |

Gelişmiş sensörler varsayılan olarak kapalıdır (**Ayarlar > Sensörler**). Açıldığında ayrı bir
süreç olan `sensors\WindowsStats.Sensors.exe` çalışır (yaklaşık 80–100 MB bellek). Windows Stats
**hiçbir sürücüyü kendisi kurmaz**. İşlemci sıcaklığı için PawnIO'yu
[resmî sitesinden](https://pawnio.eu) kendiniz kurmanız ve "Yardımcıyı yönetici olarak çalıştır"
seçeneğini açmanız gerekir (her açılışta UAC onayı). Desteklenmeyen sensörler sahte değer yerine
**"Bu cihazda kullanılamıyor"** olarak görünür.

## Gizlilik ve güvenlik

- Telemetri, analitik ve otomatik güncelleme kontrolü **yoktur**. Uygulama ağ portu açmaz ve
  internet bağlantısı gerektirmez.
- Tek isteğe bağlı ağ işlemi ağ panelindeki gecikme ölçümüdür. Varsayılan olarak kapalıdır ve
  açıldığında yalnızca seçtiğiniz adrese ICMP gönderir.
- Yönetici izni istemez (`asInvoker`). Yalnızca kullanıcıya ait anahtarları (`HKCU`) kullanır.
- Ölçümler ayrı bir iş parçacığında yapılır; görev çubuğuna gömülü pencere hiçbir zaman ölçüm
  beklemez. Böylece Explorer'ın donması önlenir. Explorer yeniden başlarsa şerit kendini yeniden
  bağlar.
- Ayarlar `%APPDATA%\WindowsStats\config.toml` dosyasında, günlük ise `WindowsStats.log`
  dosyasında tutulur.

## Derleme

Yönetici izni gerektirmeyen, proje içi bir araç zinciri kullanılır (`.toolchain`, git'e dahil
değildir):

- Rust 1.99, `x86_64-pc-windows-gnu`: `rustup-init.exe -y --no-modify-path --default-host x86_64-pc-windows-gnu`
  ile `RUSTUP_HOME`/`CARGO_HOME` değişkenleri `.toolchain\rustup` ve `.toolchain\cargo` gösterecek
  şekilde kurulur.
- [llvm-mingw](https://github.com/mstorsjo/llvm-mingw): yalnızca simge ve manifest için `windres`.
  `.toolchain\llvm-mingw` klasörüne açılır.
- .NET SDK 8+ (yalnızca isteğe bağlı sensör yardımcısı için).

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Test
```

Çıktı: `target\release\WindowsStats.exe` ve `dist\WindowsStats-<sürüm>-x64.zip` (uygulama,
sensör yardımcısı, lisanslar, `Kur.cmd`).

## Komut satırı

```
WindowsStats.exe                   başlat
WindowsStats.exe --settings        ayarlar penceresini aç
WindowsStats.exe --install         kullanıcıya özel kur (--quiet: soru sormadan)
WindowsStats.exe --uninstall       kaldır (--quiet: soru sormadan, ayarlar korunur)
WindowsStats.exe --quit            çalışan uygulamayı temiz biçimde kapat
WindowsStats.exe --dump 10 1000    10 ölçümü JSON satırları olarak yazdır
WindowsStats.exe --taskbar-items   görev çubuğu öğelerini listele (tanı)
WindowsStats.exe --version
```

## Testler

```powershell
cargo test --release                                                   # birim testleri
powershell -ExecutionPolicy Bypass -File scripts\compare-taskmgr.ps1   # Görev Yöneticisi sayaçlarıyla karşılaştırma
```

`tools\uitest` arayüz testleri için ekran yakalama ve fare yardımcısıdır (dağıtıma dahil değildir).

## Lisans

MIT. Ayrıntılar ve kullanılan açık kaynak bileşenler: [LICENSE](LICENSE),
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md), `licenses\`.
