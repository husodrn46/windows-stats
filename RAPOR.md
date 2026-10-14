# Windows Stats: Geliştirme Raporu (sürüm 0.1.0)

Test ortamı: Windows 11 Home Single Language, **derleme 26300 (Insider)**, Intel Core 7 240H
(16 mantıksal çekirdek, P/E hibrit), Intel Graphics (tümleşik), 16 GB RAM, iki NVMe SSD,
1920×1080 %100 ölçek, tek monitör, pilsiz mini bilgisayar. Antivirüs: Kaspersky Small Office Security.

## 1. Mimari kararı

| Proje | Lisans | Değerlendirme | Kullanım |
|---|---|---|---|
| mgvs/winstats | MIT | Rust, Win32/GDI, görev çubuğuna gömme, paneller ve 29 dil hazır | **Temel alındı** (git geçmişiyle çatallandı, atıflar korundu) |
| exelban/stats | MIT | Swift/macOS, kodu Windows'a taşınamaz | Yalnızca tasarım ve davranış esini |
| LibreHardwareMonitor | MPL-2.0 | C#, sensörler için en kapsamlı kaynak | Ayrı yardımcı süreçte, değiştirilmeden |

winstats'tan kaldırılanlar: otomatik GitHub güncelleme kontrolü, uzak makine ve LAN paylaşımı
(ağ portu açıyordu), varsayılan 1.1.1.1 ping'i. Gizlilik gereksinimi nedeniyle uygulama artık
hiçbir ağ bağlantısı açmıyor.

## 2. Doğrulama sonuçları

| Gereksinim | Sonuç | Nasıl doğrulandı |
|---|---|---|
| Uygulama sorunsuz açılıyor | ✅ | Günlük, ekran görüntüleri, gerçek kullanım |
| Gerçek CPU ve RAM değerleri | ✅ | `--dump` çıktısı |
| Görev Yöneticisi ile tutarlılık | ✅ CPU farkı 1,7 puan, RAM farkı 0,0 puan (20 sn ortalama) | `scripts\compare-taskmgr.ps1` (Windows sayaçlarıyla eşzamanlı) |
| Ağ hızı gerçek trafikte güncelleniyor | ✅ | Karşılaştırma testi (5,2 / 5,2 KB/s) ve canlı şerit |
| Widget'lar sürüklenip sıralanabiliyor | ✅ | Kullanıcı gerçek kullanımda sıraladı (günlükte 4 yeniden sıralama) |
| Ayarlar yeniden açılışta korunuyor | ✅ | Yeniden başlatma sonrası sıra ve ayarlar aynı |
| Explorer yeniden başlayınca toparlanma | ✅ | explorer.exe kapatılıp açıldı; şerit 1 sn içinde yeniden bağlandı |
| Desteklenmeyen sensörde sahte değer yok | ✅ | Intel GPU sıcaklığı, PawnIO'suz CPU sıcaklığı "kullanılamıyor"; sürücü gerektiren sıfırlar filtreleniyor |
| Kaldırınca görev çubuğu normal | ✅ | `--install --quiet` / `--uninstall --quiet` döngüsü; dosya, kısayol, kayıt temizlendi |
| Boşta CPU < %1 | ✅ **Makinenin %0,05'i** (tek çekirdeğin %0,81'i), 304 sn | `Get-Process` örneklemesi |
| Bellek | ✅ ortalama 16,1 MB özel bellek (en çok 20,3 MB), çalışma kümesi yaklaşık 42 MB | Aynı ölçüm |
| Sensör yardımcısı | ✅ PawnIO 2.2 + yönetici: 147 okuma, 36 sıcaklık, CPU 65 °C | Günlük |

Otomatik testler: 22 birim testi (yapılandırma, dil dosyaları, birimler, bildirim kuralları,
görev çubuğu sığdırma, sensör protokolü, DirectWrite yazı, JSON çıktısı) ve
`compare-taskmgr.ps1`.

## 3. Tamamlanan özellikler

- **Görev çubuğu:** gömülü şerit. Windows izin vermezse otomatik yüzen mod ve dakikada bir yeniden
  gömme denemesi. Windows 11 görev listesinin UI Automation ile izlenip üzerine binilmemesi.
  Explorer yeniden başlayınca kendini toparlama.
- **Modüller:** CPU (çekirdek çubukları, P/E ayrımı, frekans, Görev Yöneticisi uyumlu yük, işlemler),
  GPU (yük, VRAM, sürücü, çoklu bağdaştırıcı, sıcaklık/fan varsa), RAM (kullanım, ayrılan, döküm,
  SMBIOS modülleri, işlemler), Disk (okuma/yazma, fiziksel diskler, SSD/HDD/NVMe, sıcaklık,
  sürücü doluluğu, G/Ç işlemleri), Ağ (indirme/yükleme, adaptörler, Wi-Fi, IP/MAC, toplamlar,
  adaptör seçimi), Pil (masaüstünde otomatik gizli), Sensörler (sıcaklık widget'ı ve panel).
- **Tasarım:** DirectWrite ile keskin yazılar (Segoe UI Variable, sabit genişlikli rakamlar),
  yumuşatılmış alan grafikleri, Stats tarzı ayna ağ grafiği, Windows 11 tarzı ayarlar penceresi,
  sistem/koyu/açık tema, vurgu rengi paleti, yüksek DPI farkındalığı (Per-Monitor V2).
- **Ayarlar:** modüller ve sıralama, renkler, tema, yazı ve grafik boyutu, aralık (0,5/1/2/5 sn),
  konum, yerleşim modu, başlangıçta çalışma, eşik bildirimleri, sensörler, gizlilik, hakkında ve
  lisanslar.
- **Performans:** ölçüm iş parçacığı ayrı; ayrıntılar yalnızca panel açıkken toplanıyor; sınırlı
  (360 örnek) geçmiş; uyku/uyanma işleniyor; olay tabanlı görev çubuğu izleme.
- **Ürünleştirme:** kullanıcıya özel kurulum ve kaldırma (yönetici izni yok), çökme kaydı ve
  Windows'un otomatik yeniden başlatması, atomik yapılandırma kaydı, derleme betiği, lisans
  bildirimleri.

## 4. Eksikler ve bilinen sınırlamalar

- **Windows 10'da test edilmedi.** Windows 10 kod yolu (görev listesini daraltma) winstats'tan
  geliyor ve korunuyor, ancak bu oturumda Windows 10 makinesi yoktu.
- **Çoklu monitör:** şerit yalnızca ana görev çubuğunda gösteriliyor. Paneller ve ayarlar penceresi
  bulundukları monitörün DPI'sine uyuyor; ancak ikincil görev çubuklarında şerit yok.
- **Dikey görev çubuğu** (winstats'tan gelen düzen) Windows 11'de resmî olarak desteklenmediği için
  test edilmedi.
- **Diğer diller:** yalnızca Türkçe ve İngilizce eksiksiz. winstats'tan gelen diğer 27 dilde yeni
  metinler İngilizce görünüyor.
- **Bildirimler** Windows'un kendi bildirim merkezine değil, Windows Stats'in kendi bildirim
  kartına düşüyor.
- **Sensör yardımcısı** açıkken yaklaşık 80–100 MB ek bellek kullanıyor (.NET +
  LibreHardwareMonitor). Bu yüzden varsayılan olarak kapalı. Yönetici modu her açılışta UAC onayı
  istiyor (zamanlanmış görevle onaysız başlatma eklenmedi, çünkü bu sistem çapında bir değişiklik).
- Bu mini bilgisayarda **fan devri okunamadı.** Donanım okunabilir bir Super I/O yongası sunmuyor;
  değer "kullanılamıyor" olarak kalıyor.
- **İmzasız çalıştırılabilir dosya:** Kaspersky gibi antivirüsler yeni ve imzasız derlemeleri
  sezgisel olarak işaretleyebiliyor (geliştirme sırasında derleme klasörü için istisna gerekti).
  Dağıtım için kod imzalama sertifikası önerilir.
- Geliştirme sırasında Insider derlemesinde görev çubuğunun geçici olarak alt pencere kabul etmediği
  bir durum yaşandı. Otomatik yüzen mod bu durumu karşılıyor; yeniden başlatmadan sonra gömme tekrar
  çalıştı.

## 5. Gizlilik ve güvenlik özeti

Telemetri yok; ağ portu yok; internet gerekmiyor; yönetici izni istenmiyor (yalnızca kullanıcı
isterse sensör yardımcısı için UAC); sürücü kurulmuyor (PawnIO'yu kullanıcı kendisi kurdu); yalnızca
`HKCU` anahtarları ve kullanıcı klasörleri kullanılıyor.
