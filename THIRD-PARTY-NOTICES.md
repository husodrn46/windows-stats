# Üçüncü taraf bildirimleri / Third-party notices

Windows Stats aşağıdaki açık kaynak çalışmaları kullanır veya onlardan esinlenir. Her bileşenin
lisans koşulları korunmuştur. / Windows Stats uses or is inspired by the open-source work below;
the licence terms of every component are kept.

## 1. winstats (temel alınan kod / code base)

Windows Stats, **winstats** projesinin bir çatalıdır (fork). Görev çubuğuna gömme, ölçüm
toplayıcıları, açılır paneller, menü ve çeviriler winstats'tan gelir ve değiştirilerek kullanılır.

- Proje: https://github.com/mgvs/winstats
- Yazar: Oleksandr Zhabotynskyi <alexlp@mgvs.org>
- Lisans: MIT

```
MIT License

Copyright (c) 2026 Oleksandr Zhabotynskyi

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## 2. Stats (yalnızca tasarım esini / design inspiration only)

Görsel dil ve modül yapısı macOS uygulaması **Stats**'ten esinlenmiştir. Stats'in kaynak kodu
kullanılmamıştır (Swift/macOS; Windows Stats Rust/Win32).

- Proje: https://github.com/exelban/stats
- Yazar: Serhiy Mytrovtsiy
- Lisans: MIT, Copyright (c) 2019 Serhiy Mytrovtsiy

## 3. Rust kütüphaneleri (çalıştırılabilir dosyaya bağlanan / linked into the executable)

| Crate | Sürüm | Lisans |
|---|---|---|
| serde, serde_core, serde_derive | 1.0.229 | MIT OR Apache-2.0 |
| toml | 0.9.12 | MIT OR Apache-2.0 |
| toml_datetime, toml_parser, toml_writer, serde_spanned | 0.7 / 1.1 | MIT OR Apache-2.0 |
| winnow | 0.7 / 1.0 | MIT |
| windows, windows-core, windows-result, windows-strings, windows-implement, windows-interface | 0.58 / 0.2 / 0.1 | MIT OR Apache-2.0 |
| windows-targets, windows_x86_64_gnu | 0.52.6 | MIT OR Apache-2.0 |
| Rust standart kütüphanesi (std) | 1.99.0 | MIT OR Apache-2.0 |

Yalnızca derleme sırasında kullanılanlar / build time only: winresource (MIT), proc-macro2,
quote, syn (MIT OR Apache-2.0), unicode-ident ((MIT OR Apache-2.0) AND Unicode-3.0).

Çift lisanslı (MIT OR Apache-2.0) kütüphaneler bu dağıtımda MIT koşullarıyla kullanılır. MIT
lisans metni yukarıdadır; her kütüphanenin telif hakkı sahipleri kendi depolarında listelenir.

## 4. MinGW-w64 çalışma zamanı ve libgcc

Windows Stats `x86_64-pc-windows-gnu` hedefiyle derlenir. Çalıştırılabilir dosyaya Rust araç
zincirinin getirdiği MinGW-w64 başlangıç kodu (crt) ve libgcc parçaları statik olarak bağlanır.

- mingw-w64 çalışma zamanı: çoğunlukla kamu malı / ZPL 2.1 / MIT benzeri izin verici lisanslar
  (https://www.mingw-w64.org).
- libgcc: GPL-3.0 + **GCC Runtime Library Exception 3.1**. Bu istisna, libgcc ile bağlanan
  programların herhangi bir lisansla dağıtılmasına izin verir.

## 5. Derleme araçları (dağıtıma dahil değildir / not distributed)

- Rust araç zinciri (rustc, cargo), MIT OR Apache-2.0
- llvm-mingw 20260922 (yalnızca kaynak derleyici `windres` için), Apache-2.0 WITH
  LLVM-exception ve mingw-w64 lisansları, https://github.com/mstorsjo/llvm-mingw

## 6. Windows bileşenleri

Segoe UI yazı tipleri, DirectWrite, PDH, DXGI ve diğer sistem API'leri Windows'un parçasıdır ve
Windows Stats ile birlikte dağıtılmaz.
