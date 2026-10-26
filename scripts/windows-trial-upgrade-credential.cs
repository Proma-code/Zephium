// Synthetic installer fixture only: exact generated trial target, no enumeration.
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

public sealed class ZephiumTrialUpgradeCredential
{
    private const uint Generic = 1, Session = 1;
    private const int NotFound = 1168;
    private readonly string target;
    private readonly byte[] payload;
    private bool owned;
    private ZephiumTrialUpgradeCredential(string suffix)
    {
        target = FixtureTarget(suffix);
        payload = Encoding.UTF8.GetBytes("Nonsecret Zephium installer-upgrade fixture " + suffix);
    }
    public static string FixtureTarget(string suffix)
    {
        Guid parsed;
        if (suffix == null || !Guid.TryParseExact(suffix, "N", out parsed) || parsed.ToString("N") != suffix)
            throw new ArgumentException("Fixture suffix must be one generated canonical UUID.");
        return "app.zephium.subscription-trial.upgrade-smoke." + suffix;
    }
    public static ZephiumTrialUpgradeCredential Create()
    {
        if (!RuntimeInformation.IsOSPlatform(OSPlatform.Windows) || IntPtr.Size != 8 ||
            Environment.GetEnvironmentVariable("GITHUB_ACTIONS") != "true" ||
            Environment.GetEnvironmentVariable("GITHUB_REPOSITORY") != "Proma-code/Zephium" ||
            Environment.GetEnvironmentVariable("RUNNER_ENVIRONMENT") != "github-hosted")
            throw new InvalidOperationException("Credential fixture requires the disposable Windows workflow.");
        var fixture = new ZephiumTrialUpgradeCredential(Guid.NewGuid().ToString("N"));
        IntPtr existing;
        if (CredReadW(fixture.target, Generic, 0, out existing))
        {
            CredFree(existing);
            throw new InvalidOperationException("Refusing an existing exact credential fixture target.");
        }
        Require(Marshal.GetLastWin32Error() == NotFound, "establish fixture absence");
        IntPtr name = Marshal.StringToHGlobalUni(fixture.target), blob = Marshal.AllocHGlobal(fixture.payload.Length);
        try
        {
            Marshal.Copy(fixture.payload, 0, blob, fixture.payload.Length);
            var record = new Credential { Type = Generic, TargetName = name, BlobSize = (uint)fixture.payload.Length, Blob = blob, Persist = Session };
            Require(CredWriteW(ref record, 0), "write exact synthetic fixture");
            fixture.owned = true;
        }
        finally { Marshal.FreeHGlobal(blob); Marshal.FreeHGlobal(name); }
        return fixture;
    }
    public bool IsUnchanged()
    {
        if (!owned) throw new InvalidOperationException("Fixture is not owned.");
        IntPtr raw;
        Require(CredReadW(target, Generic, 0, out raw), "read exact owned fixture");
        try
        {
            var record = Marshal.PtrToStructure<Credential>(raw);
            if (record.Flags != 0 || record.Type != Generic || record.Persist != Session || record.AttributeCount != 0 ||
                record.BlobSize != payload.Length || record.Blob == IntPtr.Zero || Marshal.PtrToStringUni(record.TargetName) != target ||
                !String.IsNullOrEmpty(Marshal.PtrToStringUni(record.Comment)) || !String.IsNullOrEmpty(Marshal.PtrToStringUni(record.TargetAlias)) ||
                !String.IsNullOrEmpty(Marshal.PtrToStringUni(record.UserName))) return false;
            var observed = new byte[payload.Length]; Marshal.Copy(record.Blob, observed, 0, observed.Length);
            for (int i = 0; i < observed.Length; i++) if (observed[i] != payload[i]) return false;
            return true;
        }
        finally { CredFree(raw); }
    }
    public void RemoveOwned()
    {
        if (!owned) return;
        if (!IsUnchanged()) throw new InvalidOperationException("Changed credential fixture is left untouched for runner disposal.");
        Require(CredDeleteW(target, Generic, 0), "delete exact owned fixture");
        owned = false;
        IntPtr remaining;
        if (CredReadW(target, Generic, 0, out remaining)) { CredFree(remaining); throw new InvalidOperationException("Credential fixture deletion was not confirmed."); }
        Require(Marshal.GetLastWin32Error() == NotFound, "confirm exact fixture deletion");
    }
    private static void Require(bool condition, string operation)
    {
        if (!condition) throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not " + operation + ".");
    }
    [StructLayout(LayoutKind.Sequential)] private struct Credential
    {
        public uint Flags, Type;
        public IntPtr TargetName, Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint BlobSize;
        public IntPtr Blob;
        public uint Persist, AttributeCount;
        public IntPtr Attributes, TargetAlias, UserName;
    }
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern bool CredReadW(string target, uint type, uint flags, out IntPtr credential);
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern bool CredWriteW(ref Credential credential, uint flags);
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern bool CredDeleteW(string target, uint type, uint flags);
    [DllImport("advapi32.dll")] private static extern void CredFree(IntPtr buffer);
}
