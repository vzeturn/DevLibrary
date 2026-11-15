using System;
using System.Runtime.InteropServices;
using System.Text;

namespace DevLibrary;

/// <summary>Raw imports of crates/dl-ffi/include/dl_ffi.h (ABI version 1) and the buffer helpers.</summary>
/// <remarks>
/// Inputs are encoded to UTF-8 on the stack (heap only above <see cref="StackInput"/> bytes);
/// outputs are written into a 512-byte stack buffer, retried once with an exact heap buffer
/// when larger. The only allocation of a typical call is the returned string.
/// </remarks>
internal static unsafe partial class Native
{
    private const string Lib = "dl_ffi";

    internal const int Ok = 0;
    internal const int BufferTooSmall = 1;
    internal const int NullPointer = 2;
    internal const int InvalidUtf8 = 3;
    internal const int InvalidInput = 4;
    internal const uint ExpectedAbi = 1;

    /// <summary>Size of the first output buffer; every result of ABI 1 fits in practice.</summary>
    internal const int StackOutput = 512;
    /// <summary>Largest input encoded on the stack.</summary>
    internal const int StackInput = 256;

    [LibraryImport(Lib, EntryPoint = "dl_ffi_abi_version")]
    internal static partial uint AbiVersion();

    [LibraryImport(Lib, EntryPoint = "dl_vn_number_words")]
    internal static partial int NumberWords(ulong n, uint flags, byte* output, nuint cap, nuint* outLen);

    [LibraryImport(Lib, EntryPoint = "dl_vn_amount_words")]
    internal static partial int AmountWords(long amount, byte* unit, nuint unitLen, uint flags, byte* output, nuint cap, nuint* outLen);

    [LibraryImport(Lib, EntryPoint = "dl_vn_remove_diacritics")]
    internal static partial int RemoveDiacritics(byte* text, nuint textLen, byte* output, nuint cap, nuint* outLen);

    [LibraryImport(Lib, EntryPoint = "dl_vietqr_payload")]
    internal static partial int VietQrPayload(
        byte* bin, nuint binLen, byte* account, nuint accountLen, ulong amount, uint flags,
        byte* bill, nuint billLen, byte* purpose, nuint purposeLen,
        byte* output, nuint cap, nuint* outLen);

    [LibraryImport(Lib, EntryPoint = "dl_vietqr_verify")]
    internal static partial int VietQrVerify(byte* payload, nuint payloadLen, byte* output, nuint cap, nuint* outLen);

    private static readonly bool AbiChecked = CheckAbi();

    private static bool CheckAbi()
    {
        uint v = AbiVersion();
        if (v != ExpectedAbi)
            throw new InvalidOperationException($"dl_ffi ABI version {v} is loaded, this binding needs {ExpectedAbi}.");
        return true;
    }

    /// <summary>Loads the native library once and checks its ABI version.</summary>
    internal static void Ensure() => _ = AbiChecked;

    /// <summary>Number of UTF-8 bytes needed for <paramref name="s"/> (exact).</summary>
    internal static int Utf8Length(string? s) => s is null ? 0 : Encoding.UTF8.GetByteCount(s);

    /// <summary>Encodes into <paramref name="dest"/>, which must hold <see cref="Utf8Length"/> bytes.</summary>
    internal static int Encode(string? s, Span<byte> dest) => s is null ? 0 : Encoding.UTF8.GetBytes(s, dest);

    /// <summary>Converts a status and the bytes written into a string, or throws for binding errors.</summary>
    internal static string Finish(int status, ReadOnlySpan<byte> bytes) => status switch
    {
        Ok or InvalidInput => bytes.IsEmpty ? string.Empty : Encoding.UTF8.GetString(bytes),
        NullPointer => throw new InvalidOperationException("dl_ffi: null pointer (binding bug)."),
        InvalidUtf8 => throw new ArgumentException("dl_ffi: input is not valid UTF-8."),
        BufferTooSmall => throw new InvalidOperationException("dl_ffi: result size changed between calls."),
        _ => throw new InvalidOperationException($"dl_ffi: unknown status {status}."),
    };
}
