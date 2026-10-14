// UI test helper for Windows Stats (not shipped with the app).
//   uitest shot <out.bmp> [x y w h]       screen capture, layered windows included (CAPTUREBLT)
//   uitest click <x> <y> [right]          mouse click at physical pixel coordinates
//   uitest drag <x1> <y1> <x2> <y2>       press, move in steps, release
//   uitest move <x> <y>
//   uitest window <class>                 rect of the first window with this class: "l t r b"
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

static class UiTest {
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr v);
    [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr h);
    [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr h, IntPtr dc);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int i);
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern void mouse_event(uint f, int dx, int dy, uint d, IntPtr e);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindowExW(IntPtr parent, IntPtr after, string cls, string title);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleDC(IntPtr dc);
    [DllImport("gdi32.dll")] static extern IntPtr CreateDIBSection(IntPtr dc, ref BITMAPINFOHEADER bi, uint usage, out IntPtr bits, IntPtr sec, uint off);
    [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr dc, IntPtr o);
    [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr o);
    [DllImport("gdi32.dll")] static extern bool DeleteDC(IntPtr dc);
    [DllImport("gdi32.dll")] static extern bool BitBlt(IntPtr d, int x, int y, int w, int h, IntPtr s, int sx, int sy, uint rop);
    [StructLayout(LayoutKind.Sequential)] struct RECT { public int l, t, r, b; }
    [StructLayout(LayoutKind.Sequential)] struct BITMAPINFOHEADER { public uint biSize; public int biWidth, biHeight; public ushort biPlanes, biBitCount; public uint biCompression, biSizeImage; public int biXPelsPerMeter, biYPelsPerMeter; public uint biClrUsed, biClrImportant; }

    static int Main(string[] a) {
        SetProcessDpiAwarenessContext((IntPtr)(-4));
        switch (a.Length > 0 ? a[0] : "") {
            case "shot": return Shot(a);
            case "click": Click(int.Parse(a[1]), int.Parse(a[2]), a.Length > 3 && a[3] == "right"); return 0;
            case "move": SetCursorPos(int.Parse(a[1]), int.Parse(a[2])); return 0;
            case "drag": Drag(int.Parse(a[1]), int.Parse(a[2]), int.Parse(a[3]), int.Parse(a[4])); return 0;
            case "window": return Window(a[1]);
            case "children": return Children(a.Length > 1 ? a[1] : "Shell_TrayWnd");
            case "printwindow": return PrintWin(a[1], a[2]);
            default: Console.Error.WriteLine("usage: shot|click|move|drag|window|children"); return 2;
        }
    }

    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr h, uint cmd);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern int GetWindowLongW(IntPtr h, int i);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);

    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);

    /// Window content as the window itself renders it (PW_RENDERFULLCONTENT), alpha dropped.
    static int PrintWin(string hex, string outPath) {
        IntPtr h = (IntPtr)Convert.ToInt64(hex, 16);
        if (!GetWindowRect(h, out RECT r)) { Console.WriteLine("no window"); return 1; }
        int w = r.r - r.l, hh = r.b - r.t;
        var bi = new BITMAPINFOHEADER { biSize = 40, biWidth = w, biHeight = hh, biPlanes = 1, biBitCount = 24 };
        IntPtr screen = GetDC(IntPtr.Zero), mem = CreateCompatibleDC(screen);
        IntPtr dib = CreateDIBSection(screen, ref bi, 0, out IntPtr bits, IntPtr.Zero, 0);
        IntPtr old = SelectObject(mem, dib);
        bool ok = PrintWindow(h, mem, 2);
        int stride = (w * 3 + 3) & ~3;
        var px = new byte[stride * hh];
        Marshal.Copy(bits, px, 0, px.Length);
        SelectObject(mem, old); DeleteObject(dib); DeleteDC(mem); ReleaseDC(IntPtr.Zero, screen);
        using (var f = new BinaryWriter(File.Create(outPath))) {
            f.Write((ushort)0x4D42); f.Write(54 + px.Length); f.Write(0); f.Write(54);
            f.Write(40); f.Write(w); f.Write(hh); f.Write((ushort)1); f.Write((ushort)24); f.Write(0); f.Write(px.Length); f.Write(2835); f.Write(2835); f.Write(0); f.Write(0);
            f.Write(px);
        }
        Console.WriteLine($"printwindow ok={ok} {w}x{hh}");
        return ok ? 0 : 1;
    }

    /// Direct children of a top-level window in z-order (top first): class, visibility, styles, rect.
    static int Children(string cls) {
        IntPtr parent = FindWindowExW(IntPtr.Zero, IntPtr.Zero, cls, null);
        if (parent == IntPtr.Zero) { Console.WriteLine("none"); return 1; }
        for (IntPtr c = GetWindow(parent, 5 /* GW_CHILD */); c != IntPtr.Zero; c = GetWindow(c, 2 /* GW_HWNDNEXT */)) {
            var sb = new StringBuilder(128);
            GetClassNameW(c, sb, 128);
            GetWindowRect(c, out RECT r);
            GetWindowThreadProcessId(c, out uint pid);
            Console.WriteLine($"{c.ToInt64():X8} pid={pid} vis={IsWindowVisible(c)} style={GetWindowLongW(c, -16):X8} ex={GetWindowLongW(c, -20):X8} [{r.l},{r.t},{r.r},{r.b}] {sb}");
        }
        return 0;
    }

    static int Window(string cls) {
        IntPtr h = FindWindowExW(IntPtr.Zero, IntPtr.Zero, cls, null);
        if (h == IntPtr.Zero) {
            // child windows of the taskbar (the embedded strip)
            IntPtr tray = FindWindowExW(IntPtr.Zero, IntPtr.Zero, "Shell_TrayWnd", null);
            h = tray == IntPtr.Zero ? IntPtr.Zero : FindWindowExW(tray, IntPtr.Zero, cls, null);
        }
        if (h == IntPtr.Zero || !GetWindowRect(h, out RECT r)) { Console.WriteLine("none"); return 1; }
        Console.WriteLine($"{r.l} {r.t} {r.r} {r.b}");
        return 0;
    }

    static void Click(int x, int y, bool right) {
        SetCursorPos(x, y); Thread.Sleep(80);
        mouse_event(right ? 0x0008u : 0x0002u, 0, 0, 0, IntPtr.Zero); Thread.Sleep(50);
        mouse_event(right ? 0x0010u : 0x0004u, 0, 0, 0, IntPtr.Zero);
    }

    static void Drag(int x1, int y1, int x2, int y2) {
        SetCursorPos(x1, y1); Thread.Sleep(100);
        mouse_event(0x0002, 0, 0, 0, IntPtr.Zero); Thread.Sleep(120);
        for (int i = 1; i <= 24; i++) { SetCursorPos(x1 + (x2 - x1) * i / 24, y1 + (y2 - y1) * i / 24); Thread.Sleep(25); }
        Thread.Sleep(150);
        mouse_event(0x0004, 0, 0, 0, IntPtr.Zero);
    }

    static int Shot(string[] a) {
        int x, y, w, h;
        if (a.Length >= 6) { x = int.Parse(a[2]); y = int.Parse(a[3]); w = int.Parse(a[4]); h = int.Parse(a[5]); }
        else { x = GetSystemMetrics(76); y = GetSystemMetrics(77); w = GetSystemMetrics(78); h = GetSystemMetrics(79); }
        var bi = new BITMAPINFOHEADER { biSize = 40, biWidth = w, biHeight = h, biPlanes = 1, biBitCount = 24 };
        IntPtr screen = GetDC(IntPtr.Zero), mem = CreateCompatibleDC(screen);
        IntPtr dib = CreateDIBSection(screen, ref bi, 0, out IntPtr bits, IntPtr.Zero, 0);
        IntPtr old = SelectObject(mem, dib);
        BitBlt(mem, 0, 0, w, h, screen, x, y, 0x00CC0020 | 0x40000000);
        int stride = (w * 3 + 3) & ~3;
        var px = new byte[stride * h];
        Marshal.Copy(bits, px, 0, px.Length);
        SelectObject(mem, old); DeleteObject(dib); DeleteDC(mem); ReleaseDC(IntPtr.Zero, screen);
        using (var f = new BinaryWriter(File.Create(a[1]))) {
            f.Write((ushort)0x4D42); f.Write(54 + px.Length); f.Write(0); f.Write(54);
            f.Write(40); f.Write(w); f.Write(h); f.Write((ushort)1); f.Write((ushort)24); f.Write(0); f.Write(px.Length); f.Write(2835); f.Write(2835); f.Write(0); f.Write(0);
            f.Write(px);
        }
        Console.WriteLine($"saved {a[1]} ({w}x{h})");
        return 0;
    }
}
