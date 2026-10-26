// Fixed Windows CI custodian. No arbitrary program, arguments, PID or environment API.
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Threading;

public sealed class ZephiumTrialRuntimeHost : IDisposable
{
    private IntPtr job, process;
    private uint processId;
    private FileStream executableLock;
    private const uint KillOnJobClose = 0x2000, ExtendedStartupInfo = 0x80000, CreateSuspended = 4;
    private const uint WaitTimeout = 258, WmClose = 0x10;
    private const int JobExtendedLimits = 9, JobAccounting = 1;
    private static readonly IntPtr JobListAttribute = new IntPtr(0x0002000D);

    private ZephiumTrialRuntimeHost() { }

    public static ZephiumTrialRuntimeHost Start(string expectedSha256)
    {
        if (!RuntimeInformation.IsOSPlatform(OSPlatform.Windows) || IntPtr.Size != 8 ||
            Environment.GetEnvironmentVariable("GITHUB_ACTIONS") != "true" ||
            Environment.GetEnvironmentVariable("GITHUB_REPOSITORY") != "Proma-code/Zephium" ||
            Environment.GetEnvironmentVariable("RUNNER_ENVIRONMENT") != "github-hosted" ||
            Environment.GetEnvironmentVariable("GITHUB_REF") != "refs/heads/windows/subscription-accounts" ||
            Environment.GetEnvironmentVariable("GITHUB_EVENT_NAME") != "push" ||
            Environment.GetEnvironmentVariable("RUNNER_OS") != "Windows" ||
            Environment.GetEnvironmentVariable("RUNNER_ARCH") != "X64")
            throw new InvalidOperationException("Runtime custodian requires the disposable Windows x64 workflow.");
        string workspace = Environment.GetEnvironmentVariable("GITHUB_WORKSPACE");
        if (String.IsNullOrEmpty(workspace)) throw new InvalidOperationException("Missing workflow checkout.");
        string executable = Path.GetFullPath(Path.Combine(workspace, "target", "release", "zephium-subscription-trial.exe"));
        var owner = new ZephiumTrialRuntimeHost();
        IntPtr attributes = IntPtr.Zero, jobList = IntPtr.Zero, thread = IntPtr.Zero;
        bool attributesInitialized = false;
        try
        {
            // Deny write/delete sharing while verifying and throughout execution.
            owner.executableLock = new FileStream(executable, FileMode.Open, FileAccess.Read, FileShare.Read);
            string actual = Convert.ToHexString(SHA256.HashData(owner.executableLock)).ToLowerInvariant();
            if (!String.Equals(actual, expectedSha256, StringComparison.Ordinal))
                throw new InvalidOperationException("Held trial executable hash mismatch.");
            if (FileVersionInfo.GetVersionInfo(executable).ProductName != "Zephium Subscription Trial")
                throw new InvalidOperationException("Held trial executable product mismatch.");
            owner.job = CreateJobObjectW(IntPtr.Zero, null);
            Require(owner.job != IntPtr.Zero, "create owned job");
            var limits = new ExtendedLimits();
            limits.Basic.LimitFlags = KillOnJobClose; // No breakaway permissions.
            Require(SetInformationJobObject(owner.job, JobExtendedLimits, ref limits, (uint)Marshal.SizeOf<ExtendedLimits>()), "seal owned job");
            IntPtr bytes = IntPtr.Zero;
            InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref bytes);
            if (bytes.ToInt64() <= 0 || bytes.ToInt64() > 65536) throw new InvalidOperationException("Invalid process attribute size.");
            attributes = Marshal.AllocHGlobal(bytes);
            Require(InitializeProcThreadAttributeList(attributes, 1, 0, ref bytes), "initialize process attributes");
            attributesInitialized = true;
            jobList = Marshal.AllocHGlobal(IntPtr.Size);
            Marshal.WriteIntPtr(jobList, owner.job);
            Require(UpdateProcThreadAttribute(attributes, 0, JobListAttribute, jobList, new IntPtr(IntPtr.Size), IntPtr.Zero, IntPtr.Zero), "bind creation to owned job");
            var startup = new StartupInfoEx();
            startup.Startup.cb = Marshal.SizeOf<StartupInfoEx>();
            startup.Attributes = attributes;
            ProcessInformation created;
            // Atomic JOB_LIST membership at creation, before any application code.
            Require(CreateProcessW(executable, new StringBuilder("\"" + executable + "\""), IntPtr.Zero, IntPtr.Zero, false,
                ExtendedStartupInfo | CreateSuspended, IntPtr.Zero, Path.GetDirectoryName(executable), ref startup, out created), "create contained trial");
            owner.process = created.Process; owner.processId = created.ProcessId; thread = created.Thread;
            bool inJob;
            Require(IsProcessInJob(owner.process, owner.job, out inJob) && inJob, "confirm created process custody");
            var image = new StringBuilder(32768); uint length = (uint)image.Capacity;
            Require(QueryFullProcessImageNameW(owner.process, 0, image, ref length), "verify created process image");
            if (!String.Equals(Path.GetFullPath(image.ToString()), executable, StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("Created process image is not the fixed trial executable.");
            Require(ResumeThread(thread) != UInt32.MaxValue, "resume contained trial");
            return owner;
        }
        catch (Exception failure)
        {
            // Every successfully created process already belongs to this job.
            // Closing the last Job handle is a final stop request, not proof of
            // settlement. Preserve that distinction even when Start never returns.
            bool createdProcess = owner.process != IntPtr.Zero;
            bool cleanupConfirmed = false;
            try { cleanupConfirmed = owner.StopOwned(10000); }
            catch { cleanupConfirmed = false; }
            finally { owner.Dispose(); }
            var native = failure as Win32Exception;
            throw new LaunchFailureException(cleanupConfirmed, createdProcess,
                native == null ? 0 : native.NativeErrorCode);
        }
        finally
        {
            if (thread != IntPtr.Zero) CloseHandle(thread);
            if (attributesInitialized) DeleteProcThreadAttributeList(attributes);
            if (attributes != IntPtr.Zero) Marshal.FreeHGlobal(attributes);
            if (jobList != IntPtr.Zero) Marshal.FreeHGlobal(jobList);
        }
    }

    public sealed class LaunchFailureException : Exception
    {
        public bool CleanupConfirmed { get; private set; }
        public bool CreatedProcess { get; private set; }
        public int NativeErrorCode { get; private set; }
        internal LaunchFailureException(bool cleanupConfirmed, bool createdProcess, int nativeErrorCode)
            : base(cleanupConfirmed ? "Trial launch failed; owned process is settled or was not created."
                : "Trial launch failed; owned-process cleanup remains unconfirmed.")
        {
            CleanupConfirmed = cleanupConfirmed;
            CreatedProcess = createdProcess;
            NativeErrorCode = nativeErrorCode;
        }
    }

    public uint ProcessId { get { return processId; } }
    public bool HasExited
    {
        get
        {
            if (process == IntPtr.Zero) return true;
            uint result = WaitForSingleObject(process, 0);
            if (result == 0) return true;
            if (result == WaitTimeout) return false;
            throw new InvalidOperationException("Cannot establish owned process exit.");
        }
    }
    public uint ExitCode
    {
        get
        {
            if (!HasExited) throw new InvalidOperationException("Owned process is still running.");
            uint code; Require(GetExitCodeProcess(process, out code), "read owned exit code"); return code;
        }
    }
    public uint ActiveProcesses
    {
        get
        {
            if (job == IntPtr.Zero) return 0;
            BasicAccounting accounting;
            Require(QueryInformationJobObject(job, JobAccounting, out accounting, (uint)Marshal.SizeOf<BasicAccounting>(), IntPtr.Zero), "query owned job");
            return accounting.ActiveProcesses;
        }
    }
    private List<IntPtr> OwnedWindows()
    {
        var windows = new List<IntPtr>();
        if (HasExited) return windows;
        EnumWindows((window, unused) => {
            uint pid; GetWindowThreadProcessId(window, out pid);
            if (pid == processId && windows.Count < 16) windows.Add(window);
            return true;
        }, IntPtr.Zero);
        return windows;
    }
    private bool StillOwnsWindow(IntPtr window)
    {
        if (HasExited) return false;
        uint pid; GetWindowThreadProcessId(window, out pid);
        bool inJob;
        return pid == processId && IsProcessInJob(process, job, out inJob) && inJob;
    }
    public bool HasVisibleResponsiveWindow()
    {
        foreach (IntPtr window in OwnedWindows())
        {
            Rect rect; var name = new StringBuilder(256);
            GetClassNameW(window, name, name.Capacity);
            if (!StillOwnsWindow(window) || !IsWindowVisible(window) || name.ToString() == "#32770" ||
                !GetWindowRect(window, out rect) || rect.Right - rect.Left < 100 || rect.Bottom - rect.Top < 100) continue;
            IntPtr result;
            if (SendMessageTimeoutW(window, 0, IntPtr.Zero, IntPtr.Zero, 2, 250, out result) != IntPtr.Zero) return true;
        }
        return false;
    }
    public bool RequestClose()
    {
        bool sent = false;
        foreach (IntPtr window in OwnedWindows())
            if (StillOwnsWindow(window)) sent |= PostMessageW(window, WmClose, IntPtr.Zero, IntPtr.Zero);
        return sent;
    }
    public bool WaitForEmpty(int milliseconds)
    {
        if (milliseconds < 0 || milliseconds > 30000) throw new ArgumentOutOfRangeException(nameof(milliseconds));
        var timer = Stopwatch.StartNew();
        do
        {
            if (HasExited && ActiveProcesses == 0) return true;
            Thread.Sleep(50);
        } while (timer.ElapsedMilliseconds < milliseconds);
        return HasExited && ActiveProcesses == 0;
    }
    public bool StopOwned(int milliseconds)
    {
        if (job == IntPtr.Zero) return process == IntPtr.Zero;
        if (ActiveProcesses != 0) Require(TerminateJobObject(job, 1), "stop owned job after failed smoke");
        return WaitForEmpty(milliseconds);
    }
    public void Dispose()
    {
        // Last-handle closure is a final custody fence, not graceful-close proof.
        if (job != IntPtr.Zero) { CloseHandle(job); job = IntPtr.Zero; }
        if (process != IntPtr.Zero) { CloseHandle(process); process = IntPtr.Zero; }
        if (executableLock != null) { executableLock.Dispose(); executableLock = null; }
    }
    private static void Require(bool ok, string operation)
    {
        if (!ok) throw new Win32Exception(Marshal.GetLastWin32Error(), "Trial runtime custodian could not " + operation + ".");
    }

    [StructLayout(LayoutKind.Sequential)] private struct BasicLimits
    { public long PerProcessTime, PerJobTime; public uint LimitFlags; public UIntPtr MinWorkingSet, MaxWorkingSet; public uint ActiveProcessLimit; public UIntPtr Affinity; public uint PriorityClass, SchedulingClass; }
    [StructLayout(LayoutKind.Sequential)] private struct IoCounters
    { public ulong ReadOperations, WriteOperations, OtherOperations, ReadBytes, WriteBytes, OtherBytes; }
    [StructLayout(LayoutKind.Sequential)] private struct ExtendedLimits
    { public BasicLimits Basic; public IoCounters Io; public UIntPtr ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory; }
    [StructLayout(LayoutKind.Sequential)] private struct BasicAccounting
    { public long User, Kernel, PeriodUser, PeriodKernel; public uint Faults, TotalProcesses, ActiveProcesses, TerminatedProcesses; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] private struct StartupInfo
    { public int cb; public IntPtr Reserved, Desktop, Title; public uint X, Y, XSize, YSize, XChars, YChars, Fill, Flags; public ushort Show, ReservedSize; public IntPtr ReservedBytes, Input, Output, Error; }
    [StructLayout(LayoutKind.Sequential)] private struct StartupInfoEx { public StartupInfo Startup; public IntPtr Attributes; }
    [StructLayout(LayoutKind.Sequential)] private struct ProcessInformation { public IntPtr Process, Thread; public uint ProcessId, ThreadId; }
    [StructLayout(LayoutKind.Sequential)] private struct Rect { public int Left, Top, Right, Bottom; }
    private delegate bool EnumWindowsCallback(IntPtr window, IntPtr data);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)] private static extern IntPtr CreateJobObjectW(IntPtr attributes, string name);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool SetInformationJobObject(IntPtr job, int kind, ref ExtendedLimits limits, uint size);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool QueryInformationJobObject(IntPtr job, int kind, out BasicAccounting information, uint size, IntPtr returned);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool TerminateJobObject(IntPtr job, uint exitCode);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool InitializeProcThreadAttributeList(IntPtr list, int count, uint flags, ref IntPtr size);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool UpdateProcThreadAttribute(IntPtr list, uint flags, IntPtr attribute, IntPtr value, IntPtr size, IntPtr previous, IntPtr returned);
    [DllImport("kernel32.dll")] private static extern void DeleteProcThreadAttributeList(IntPtr list);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)] private static extern bool CreateProcessW(string application, StringBuilder command, IntPtr processAttributes, IntPtr threadAttributes, bool inheritHandles, uint flags, IntPtr environment, string directory, ref StartupInfoEx startup, out ProcessInformation information);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool IsProcessInJob(IntPtr process, IntPtr job, out bool result);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)] private static extern bool QueryFullProcessImageNameW(IntPtr process, uint flags, StringBuilder name, ref uint size);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool GetExitCodeProcess(IntPtr process, out uint code);
    [DllImport("kernel32.dll")] private static extern bool CloseHandle(IntPtr handle);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr data);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassNameW(IntPtr window, StringBuilder name, int size);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern IntPtr SendMessageTimeoutW(IntPtr window, uint message, IntPtr wParam, IntPtr lParam, uint flags, uint timeout, out IntPtr result);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern bool PostMessageW(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
}
