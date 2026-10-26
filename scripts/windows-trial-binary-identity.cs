// Read-only identity derivation for the exact pinned, unsigned Tauri NSIS build.
// Tauri CLI 2.11.4 patches UNK to NSS for packaging, then restores the compiler EXE.
// Source: https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.11.4/crates/tauri-bundler/src/bundle.rs
using System;
using System.IO;
using System.Security.Cryptography;
using System.Text;

public static class ZephiumTrialBinaryIdentity
{
    private const int MaxBytes = 512 * 1024 * 1024;
    private static readonly byte[] Unknown = Encoding.ASCII.GetBytes("__TAURI_BUNDLE_TYPE_VAR_UNK");
    private static readonly byte[] Nsis = Encoding.ASCII.GetBytes("__TAURI_BUNDLE_TYPE_VAR_NSS");

    public sealed class Result
    {
        public string CompilerSha256 { get; private set; }
        public string NsisSha256 { get; private set; }
        public int Length { get; private set; }
        public int MarkerOffset { get; private set; }
        internal Result(string compiler, string nsis, int length, int offset)
        { CompilerSha256 = compiler; NsisSha256 = nsis; Length = length; MarkerOffset = offset; }
    }

    public static Result InspectFile(string path)
    {
        if ((File.GetAttributes(path) & FileAttributes.ReparsePoint) != 0)
            throw new InvalidOperationException("Trial binary identity refuses a reparse target.");
        using (var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read))
        {
            if (file.Length < Unknown.Length || file.Length > MaxBytes)
                throw new InvalidOperationException("Trial executable exceeds the fixed identity bound.");
            var bytes = new byte[(int)file.Length];
            file.ReadExactly(bytes);
            if (file.ReadByte() != -1) throw new InvalidOperationException("Trial executable length changed.");
            return InspectOwned(bytes);
        }
    }

    // Pure fixture entrypoint: caller bytes are never changed.
    public static Result InspectBytes(byte[] source)
    {
        if (source == null || source.Length < Unknown.Length || source.Length > MaxBytes)
            throw new InvalidOperationException("Trial executable exceeds the fixed identity bound.");
        return InspectOwned((byte[])source.Clone());
    }

    private static Result InspectOwned(byte[] bytes)
    {
        int offset = -1;
        for (int index = 0; index <= bytes.Length - Unknown.Length; index++)
        {
            if (bytes[index] != Unknown[0]) continue;
            bool matches = true;
            for (int part = 1; part < Unknown.Length; part++)
                if (bytes[index + part] != Unknown[part]) { matches = false; break; }
            if (!matches) continue;
            if (offset != -1) throw new InvalidOperationException("Trial bundle marker is ambiguous.");
            offset = index;
        }
        if (offset < 0) throw new InvalidOperationException("Trial bundle marker is missing or already patched.");
        string compiler = Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant();
        Buffer.BlockCopy(Nsis, 0, bytes, offset, Nsis.Length);
        string nsis = Convert.ToHexString(SHA256.HashData(bytes)).ToLowerInvariant();
        return new Result(compiler, nsis, bytes.Length, offset);
    }
}
