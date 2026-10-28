// A command is created atomically in a strict job before any user code runs.
// This process keeps the only job handle. Its death therefore kills the tree.
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Web.Script.Serialization;
using Microsoft.Win32.SafeHandles;
#pragma warning disable 0649 // Native layout fields are populated by Win32.

public static class TestCommandOwner {
  [StructLayout(LayoutKind.Sequential)] struct Security { public int Size; public IntPtr Descriptor; public int Inherit; }
  [StructLayout(LayoutKind.Sequential)] struct BasicLimits {
    public long UserTime, JobTime; public uint Flags; public UIntPtr Min, Max;
    public uint ActiveLimit; public UIntPtr Affinity; public uint Priority, Scheduling;
  }
  [StructLayout(LayoutKind.Sequential)] struct IoCounters { public ulong A, B, C, D, E, F; }
  [StructLayout(LayoutKind.Sequential)] struct Limits {
    public BasicLimits Basic; public IoCounters Io; public UIntPtr ProcessMemory, JobMemory, PeakProcess, PeakJob;
  }
  [StructLayout(LayoutKind.Sequential)] struct Accounting {
    public long A, B, C, D; public uint Faults, Total, Active, Terminated;
  }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct Startup {
    public int Size; public string Reserved, Desktop, Title;
    public int X, Y, XSize, YSize, XChars, YChars, Fill, Flags;
    public short Show, ReservedSize; public IntPtr ReservedBytes, Input, Output, Error;
  }
  [StructLayout(LayoutKind.Sequential)] struct StartupEx { public Startup Info; public IntPtr Attributes; }
  [StructLayout(LayoutKind.Sequential)] struct ProcessInfo { public IntPtr Process, Thread; public uint Pid, Tid; }
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr CreateJobObjectW(IntPtr attributes, IntPtr name);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool SetInformationJobObject(IntPtr job, int kind, ref Limits limits, uint size);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool QueryInformationJobObject(IntPtr job, int kind, out Accounting info, uint size, IntPtr returned);
  [DllImport("kernel32.dll", EntryPoint="QueryInformationJobObject", SetLastError=true)] static extern bool QueryIds(IntPtr job, int kind, IntPtr info, uint size, IntPtr returned);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool InitializeProcThreadAttributeList(IntPtr list, int count, int flags, ref IntPtr size);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool UpdateProcThreadAttribute(IntPtr list, uint flags, IntPtr kind, IntPtr value, IntPtr size, IntPtr previous, IntPtr returned);
  [DllImport("kernel32.dll")] static extern void DeleteProcThreadAttributeList(IntPtr list);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CreateProcessW(string app, StringBuilder line, IntPtr pa, IntPtr ta, bool inherit, uint flags, IntPtr environment, string cwd, ref StartupEx startup, out ProcessInfo info);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr CreateFileW(string name, uint access, uint share, ref Security security, uint disposition, uint flags, IntPtr template);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool MoveFileW(string source, string destination);
  [DllImport("kernel32.dll", SetLastError=true)] static extern uint ResumeThread(IntPtr thread);
  [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(IntPtr handle, uint timeout);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetExitCodeProcess(IntPtr process, out uint code);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool TerminateJobObject(IntPtr job, uint code);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr GetStdHandle(int kind);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool PeekNamedPipe(IntPtr pipe, IntPtr buffer, uint size, IntPtr read, out uint available, IntPtr remaining);

  static void Check(bool value, string operation) { if (!value) throw new Win32Exception(Marshal.GetLastWin32Error(), operation); }
  static string Quote(string value) {
    var output = new StringBuilder("\""); int slashes = 0;
    foreach (char ch in value) {
      if (ch == '\\') { slashes++; continue; }
      output.Append('\\', ch == '"' ? slashes * 2 + 1 : slashes);
      output.Append(ch); slashes = 0;
    }
    return output.Append('\\', slashes * 2).Append('"').ToString();
  }
  static uint Active(IntPtr job) {
    Accounting info;
    Check(QueryInformationJobObject(job, 1, out info, (uint)Marshal.SizeOf(typeof(Accounting)), IntPtr.Zero), "query job accounting");
    return info.Active;
  }
  static bool WaitForCleanup(IntPtr job, IntPtr process, int budget) {
    var wait = Stopwatch.StartNew();
    // Job membership is accounting, not a completion barrier for the command's
    // asynchronous termination. Keep its original handle until it is signaled:
    // in particular, an onStart cancellation can kill a suspended command whose
    // current-directory handle would otherwise outlive a "reaped" result.
    if (process != IntPtr.Zero) {
      uint status = WaitForSingleObject(process, (uint)budget);
      if (status == 258) return false;
      Check(status == 0, "wait command cleanup");
    }
    while (Active(job) != 0 && wait.ElapsedMilliseconds < budget) Thread.Sleep(10);
    return Active(job) == 0;
  }
  static List<long> Members(IntPtr job) {
    const int bytes = 65536;
    IntPtr buffer = Marshal.AllocHGlobal(bytes);
    try {
      Check(QueryIds(job, 3, buffer, bytes, IntPtr.Zero), "query owned process ids");
      int count = Marshal.ReadInt32(buffer, 4);
      if (count < 0 || count > (bytes - 8) / IntPtr.Size) throw new IOException("invalid job process count");
      var ids = new List<long>();
      for (int i = 0; i < count; i++) ids.Add(Marshal.ReadIntPtr(buffer, 8 + i * IntPtr.Size).ToInt64());
      return ids;
    } finally { Marshal.FreeHGlobal(buffer); }
  }
  static StreamWriter protocol;
  static void Emit(object value) { protocol.WriteLine("TERMAL_OWNER " + new JavaScriptSerializer().Serialize(value)); protocol.Flush(); }
  static string ReadEnvironment(Stream input) {
    // Read exactly the environment line: no StreamReader read-ahead may hide
    // a cancellation that is already buffered behind it during compilation.
    var bytes = new List<byte>(); int next;
    while ((next = input.ReadByte()) != -1) {
      if (next == 10) return new UTF8Encoding(false, true).GetString(bytes.ToArray());
      if (bytes.Count >= 16 * 1024 * 1024) throw new IOException("owner environment line exceeds limit");
      bytes.Add((byte)next);
    }
    return null;
  }
  static bool ControlRequested(out string reason) {
    // This is the sole pipe reader after the environment. There is no
    // background task which could consume bytes before publishing its state.
    uint available;
    if (!PeekNamedPipe(GetStdHandle(-10), IntPtr.Zero, 0, IntPtr.Zero, out available, IntPtr.Zero)) {
      int error = Marshal.GetLastWin32Error();
      if (error != 109 && error != 232) throw new Win32Exception(error, "observe launcher control pipe");
      reason = "launcher-exit"; return true;
    }
    reason = available > 0 ? "interrupt" : null;
    return available > 0;
  }
  static void Trace(string path, string phase) {
    File.AppendAllText(path, new JavaScriptSerializer().Serialize(new { phase = phase, pid = Process.GetCurrentProcess().Id }) + "\n", new UTF8Encoding(false));
  }
  public static void Run(string command, string[] args, string cwd, string log, string ledger, string nonce, int budget, string bootstrap) {
    Trace(bootstrap, "native-entered");
    // Raw redirected streams work without a console, including DETACHED_PROCESS.
    var controlInput = Console.OpenStandardInput();
    protocol = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false));
    IntPtr job = IntPtr.Zero, input = IntPtr.Zero, output = IntPtr.Zero, attributes = IntPtr.Zero;
    IntPtr jobList = IntPtr.Zero, handles = IntPtr.Zero, environment = IntPtr.Zero;
    bool attributesReady = false;
    ProcessInfo child = new ProcessInfo();
    var result = new Dictionary<string, object> { { "nonce", nonce }, { "code", 1 }, { "signal", null }, { "pid", null } };
    string trigger = "startup-failure"; bool interrupted = false;
    var pids = new List<long>(); string cleanup = "unresolved", pidObservationError = null;
    try {
      job = CreateJobObjectW(IntPtr.Zero, IntPtr.Zero); Check(job != IntPtr.Zero, "create job");
      var limits = new Limits(); limits.Basic.Flags = 0x2000; // KILL_ON_JOB_CLOSE; no breakaway flags.
      Check(SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf(typeof(Limits))), "configure strict job");
      // EOF identifies this exact parent's pipe without a reusable PID lookup.
      string environmentJson = ReadEnvironment(controlInput);
      if (environmentJson == null) { interrupted = true; trigger = "launcher-exit"; throw new IOException("launcher exited before command admission"); }
      var security = new Security { Size = Marshal.SizeOf(typeof(Security)), Inherit = 1 };
      input = CreateFileW("NUL", 0x80000000, 3, ref security, 3, 0, IntPtr.Zero);
      output = CreateFileW(log, 4, 7, ref security, 3, 0, IntPtr.Zero); // FILE_APPEND_DATA
      Check(input != new IntPtr(-1) && output != new IntPtr(-1), "open command handles");
      IntPtr size = IntPtr.Zero;
      InitializeProcThreadAttributeList(IntPtr.Zero, 2, 0, ref size);
      Check(size != IntPtr.Zero, "size process attributes");
      attributes = Marshal.AllocHGlobal(size);
      Check(InitializeProcThreadAttributeList(attributes, 2, 0, ref size), "initialize attributes"); attributesReady = true;
      jobList = Marshal.AllocHGlobal(IntPtr.Size); Marshal.WriteIntPtr(jobList, job);
      handles = Marshal.AllocHGlobal(IntPtr.Size * 2); Marshal.WriteIntPtr(handles, input); Marshal.WriteIntPtr(handles, IntPtr.Size, output);
      Check(UpdateProcThreadAttribute(attributes, 0, new IntPtr(0x2000d), jobList, new IntPtr(IntPtr.Size), IntPtr.Zero, IntPtr.Zero), "atomic job list");
      Check(UpdateProcThreadAttribute(attributes, 0, new IntPtr(0x20002), handles, new IntPtr(IntPtr.Size * 2), IntPtr.Zero, IntPtr.Zero), "explicit handle list");
      var startup = new StartupEx(); startup.Info.Size = Marshal.SizeOf(typeof(StartupEx));
      startup.Info.Flags = 0x100; startup.Info.Input = input; startup.Info.Output = output; startup.Info.Error = output; startup.Attributes = attributes;
      var variables = new JavaScriptSerializer().Deserialize<Dictionary<string, string>>(environmentJson);
      // Node chooses duplicate-key winners in case-sensitive lexical order.
      // Only the final Win32 environment block uses case-insensitive ordering.
      var keys = new List<string>(variables.Keys); keys.Sort(StringComparer.Ordinal);
      var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
      var winners = new List<string>();
      foreach (string key in keys) if (seen.Add(key)) winners.Add(key);
      winners.Sort(StringComparer.OrdinalIgnoreCase);
      var block = new StringBuilder();
      foreach (string key in winners) block.Append(key).Append('=').Append(variables[key]).Append('\0');
      if (block.Length == 0) block.Append('\0');
      environment = Marshal.StringToHGlobalUni(block.Append('\0').ToString());
      var line = new StringBuilder(Quote(command)); foreach (string arg in args ?? new string[0]) line.Append(' ').Append(Quote(arg));
      string controlReason;
      if (ControlRequested(out controlReason)) { interrupted = true; trigger = controlReason; }
      else {
        Check(CreateProcessW(command, line, IntPtr.Zero, IntPtr.Zero, true, 0x08080404, environment, cwd, ref startup, out child), "create contained command");
        result["pid"] = child.Pid;
        Emit(new { type = "started", nonce = nonce, pid = child.Pid, ownerPid = Process.GetCurrentProcess().Id });
        if (ControlRequested(out controlReason)) { interrupted = true; trigger = controlReason; }
        else {
          Check(ResumeThread(child.Thread) != UInt32.MaxValue, "resume contained command");
          while (true) {
            // Cancellation wins a crossed command exit and can never become PASS.
            if (ControlRequested(out controlReason)) { interrupted = true; trigger = controlReason; break; }
            uint status = WaitForSingleObject(child.Process, 20);
            if (status == 0) {
              if (ControlRequested(out controlReason)) { interrupted = true; trigger = controlReason; }
              else trigger = "command-exit";
              break;
            }
            Check(status == 258, "wait command");
          }
        }
        uint code; Check(GetExitCodeProcess(child.Process, out code), "read command exit");
        result["code"] = interrupted ? 1L : (long)code;
      }
      // PID inventory is best effort; its failure cannot skip termination.
      try { pids = Members(job); } catch (Exception observation) { pidObservationError = observation.Message; }
      Check(TerminateJobObject(job, 1), "terminate owned job");
      cleanup = WaitForCleanup(job, child.Process, budget) ? "reaped" : "unresolved";
      if (cleanup != "reaped") result["error"] = "owned command or job did not finish within cleanup budget";
    } catch (Exception error) {
      result["code"] = 1; result["error"] = error.Message;
      if (job != IntPtr.Zero) {
        try {
          TerminateJobObject(job, 1);
          cleanup = WaitForCleanup(job, child.Process, budget) ? "reaped" : "unresolved";
        } catch { cleanup = "unresolved"; }
      }
    } finally {
      // Close kills remaining members even if observation/persistence failed.
      if (job != IntPtr.Zero) CloseHandle(job);
      if (child.Thread != IntPtr.Zero) CloseHandle(child.Thread);
      if (child.Process != IntPtr.Zero) CloseHandle(child.Process);
      if (input != IntPtr.Zero && input != new IntPtr(-1)) CloseHandle(input);
      if (output != IntPtr.Zero && output != new IntPtr(-1)) CloseHandle(output);
      if (attributesReady) DeleteProcThreadAttributeList(attributes);
      foreach (IntPtr allocation in new[] { attributes, jobList, handles, environment }) if (allocation != IntPtr.Zero) Marshal.FreeHGlobal(allocation);
    }
    result["interrupted"] = interrupted;
    result["cleanup"] = new { state = cleanup, trigger = trigger, pids = pids,
      pidScope = "best-effort job snapshot before termination", pidObservationError = pidObservationError, ownership = "windows-job" };
    if (cleanup != "reaped") result["code"] = 1;
    string json = new JavaScriptSerializer().Serialize(result);
    // One immutable final ledger survives even when the launcher was killed.
    string partial = ledger + ".partial";
    Trace(bootstrap, "ledger-open");
    // The guardian supplies extended-length run paths. Open through the existing
    // Win32 owner boundary so legacy framework path validation cannot reject
    // them; CREATE_NEW and the final move preserve immutable ledger publication.
    var ledgerSecurity = new Security { Size = Marshal.SizeOf(typeof(Security)) };
    using (var handle = new SafeFileHandle(CreateFileW(partial, 0x40000000, 1, ref ledgerSecurity, 1, 0x80, IntPtr.Zero), true)) {
      Check(!handle.IsInvalid, "create owner ledger");
      using (var file = new FileStream(handle, FileAccess.Write)) {
        byte[] bytes = new UTF8Encoding(false).GetBytes(json); file.Write(bytes, 0, bytes.Length); file.Flush(true);
      }
    }
    Check(MoveFileW(partial, ledger), "publish owner ledger");
    Trace(bootstrap, "ledger-committed");
    Emit(new { type = "finished", nonce = nonce, result = result });
  }
}
