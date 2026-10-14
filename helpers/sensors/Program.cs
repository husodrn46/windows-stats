// Windows Stats sensor helper.
//
// Reads hardware sensors with LibreHardwareMonitorLib (MPL-2.0, unmodified) and prints them as
// tab-separated lines, one batch per interval:
//
//   H <version> <elevated 0|1> <pawnio 0|1>          once, first line
//   B                                                  start of a batch
//   S <hw type> <hw name> <sensor type> <name> <value> <identifier>
//   E                                                  end of a batch
//
// Output goes to stdout (Windows Stats starts the helper as its child) or, with --pipe <name>,
// to that local named pipe (the elevated case, which cannot inherit handles).
//
// The helper never installs anything. Sensors that need the PawnIO kernel driver (CPU
// temperature, power and clocks, Super I/O fans and voltages, DIMM temperatures) are only
// reported when PawnIO is installed AND the helper runs elevated; otherwise the library returns
// null or 0 for them, which would be fake values, so they are left out.

using System;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.IO.Pipes;
using System.Linq;
using System.Security.Principal;
using System.Text;
using System.Threading;
using LibreHardwareMonitor.Hardware;

static class Program
{
    const string Version = "0.1.0";

    static int Main(string[] args)
    {
        int interval = 2000;
        string pipe = null;
        int parent = 0;
        for (int i = 0; i < args.Length; i++)
        {
            if (args[i] == "--interval" && i + 1 < args.Length) int.TryParse(args[++i], out interval);
            else if (args[i] == "--pipe" && i + 1 < args.Length) pipe = args[++i];
            else if (args[i] == "--parent" && i + 1 < args.Length) int.TryParse(args[++i], out parent);
        }
        interval = Math.Max(500, Math.Min(60000, interval));

        Stream stream;
        if (pipe != null)
        {
            var client = new NamedPipeClientStream(".", pipe, PipeDirection.Out);
            client.Connect(10000);
            stream = client;
        }
        else
        {
            stream = Console.OpenStandardOutput();
        }
        var output = new StreamWriter(stream, new UTF8Encoding(false)) { AutoFlush = false, NewLine = "\n" };

        bool elevated = new WindowsPrincipal(WindowsIdentity.GetCurrent()).IsInRole(WindowsBuiltInRole.Administrator);
        bool pawnio = PawnIoInstalled();
        bool ring0 = elevated && pawnio;

        var computer = new Computer
        {
            IsCpuEnabled = true,
            IsGpuEnabled = true,
            IsMemoryEnabled = true,
            IsMotherboardEnabled = true,
            IsStorageEnabled = elevated,     // SMART needs the drive opened for read/write
            IsBatteryEnabled = true,
            IsNetworkEnabled = false,        // Windows Stats measures the network itself
            IsControllerEnabled = false,     // probes COM / USB devices: never by default
            IsPsuEnabled = false,
            IsPowerMonitorEnabled = false,
        };
        try
        {
            computer.Open();
            output.WriteLine($"H\t{Version}\t{(elevated ? 1 : 0)}\t{(pawnio ? 1 : 0)}");
            output.Flush();
            var updater = new UpdateVisitor();
            while (true)
            {
                if (parent != 0 && !Alive(parent)) break;
                computer.Accept(updater);
                output.WriteLine("B");
                foreach (var hw in computer.Hardware) Emit(output, hw, ring0);
                output.WriteLine("E");
                output.Flush();
                Thread.Sleep(interval);
            }
        }
        catch (IOException)
        {
            // Windows Stats closed the pipe: done
        }
        finally
        {
            computer.Close();
        }
        return 0;
    }

    static bool Alive(int pid)
    {
        try { return !Process.GetProcessById(pid).HasExited; } catch { return false; }
    }

    /// PawnIO registers an uninstall entry; the library exposes the same check, read here
    /// without depending on its namespace across versions.
    static bool PawnIoInstalled()
    {
        try
        {
            var t = typeof(Computer).Assembly.GetTypes().FirstOrDefault(x => x.Name == "PawnIo");
            var p = t?.GetProperty("IsInstalled");
            if (p != null) return (bool)p.GetValue(null);
        }
        catch { }
        using (var k = Microsoft.Win32.Registry.LocalMachine.OpenSubKey(@"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\PawnIO"))
            return k != null;
    }

    /// Sensors whose values come through the kernel driver.
    static bool NeedsRing0(IHardware hw, ISensor s)
    {
        switch (hw.HardwareType)
        {
            case HardwareType.Cpu:
                // load comes from Windows; temperatures, power, clocks, voltages from MSRs
                return s.SensorType != SensorType.Load;
            case HardwareType.Motherboard:
            case HardwareType.SuperIO:
            case HardwareType.EmbeddedController:
                return true;
            case HardwareType.Memory:
                return s.SensorType == SensorType.Temperature;
            case HardwareType.GpuIntel:
                return s.SensorType == SensorType.Power;
            default:
                return false;
        }
    }

    static void Emit(StreamWriter o, IHardware hw, bool ring0)
    {
        foreach (var s in hw.Sensors)
        {
            s.ValuesTimeWindow = TimeSpan.Zero; // no in-memory history in a long-running helper
            if (!ring0 && NeedsRing0(hw, s)) continue;
            if (!s.Value.HasValue) continue;
            float v = s.Value.Value;
            if (float.IsNaN(v) || float.IsInfinity(v)) continue;
            o.WriteLine(string.Join("\t", "S", hw.HardwareType.ToString(), Clean(hw.Name), s.SensorType.ToString(), Clean(s.Name), v.ToString("R", CultureInfo.InvariantCulture), s.Identifier.ToString()));
        }
        foreach (var sub in hw.SubHardware) Emit(o, sub, ring0);
    }

    static string Clean(string s) => (s ?? "").Replace('\t', ' ').Replace('\n', ' ').Replace('\r', ' ').Trim();
}

sealed class UpdateVisitor : IVisitor
{
    public void VisitComputer(IComputer computer) => computer.Traverse(this);
    public void VisitHardware(IHardware hardware)
    {
        hardware.Update();
        foreach (var sub in hardware.SubHardware) sub.Accept(this);
    }
    public void VisitSensor(ISensor sensor) { }
    public void VisitParameter(IParameter parameter) { }
}
