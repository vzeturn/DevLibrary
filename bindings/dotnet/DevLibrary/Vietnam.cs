using System;

namespace DevLibrary;

/// <summary>Regional word forms and sign handling for <see cref="VnWords"/>.</summary>
[Flags]
public enum VnWordsStyle : uint
{
    /// <summary>Northern standard: "linh", "nghìn", "bốn", "tỷ"; "Âm" for negatives.</summary>
    None = 0,
    /// <summary>"lẻ" instead of "linh" (một trăm lẻ năm).</summary>
    Le = 1,
    /// <summary>"ngàn" instead of "nghìn".</summary>
    Ngan = 1 << 1,
    /// <summary>"tư" instead of "bốn" after "mươi".</summary>
    Tu = 1 << 2,
    /// <summary>"tỉ" instead of "tỷ".</summary>
    Ti = 1 << 3,
    /// <summary>Read a negative amount by its magnitude, without "Âm".</summary>
    Unsigned = 1 << 4,
    /// <summary>Append " chẵn".</summary>
    Chan = 1 << 5,
}

/// <summary>
/// Vietnamese number reading for the "Số tiền bằng chữ" line of invoices and receipts,
/// computed by DevLibrary dl-vn (Rust).
/// </summary>
public static unsafe class VnWords
{
    /// <summary>
    /// Reads an amount, capitalised, followed by <paramref name="unit"/>:
    /// <c>Amount(1050000)</c> is "Một triệu không trăm năm mươi nghìn đồng",
    /// <c>Amount(-500000)</c> is "Âm năm trăm nghìn đồng". Accepts every <see cref="long"/>.
    /// </summary>
    public static string Amount(long amount, string unit = "đồng", VnWordsStyle style = VnWordsStyle.None)
    {
        Native.Ensure();
        int ul = Native.Utf8Length(unit);
        Span<byte> u = ul <= Native.StackInput ? stackalloc byte[Native.StackInput] : new byte[ul];
        Native.Encode(unit, u);
        Span<byte> buf = stackalloc byte[Native.StackOutput];
        nuint len;
        int status;
        fixed (byte* up = u, op = buf)
            status = Native.AmountWords(amount, up, (nuint)ul, (uint)style, op, (nuint)buf.Length, &len);
        if (status == Native.BufferTooSmall)
        {
            byte[] big = new byte[(int)len];
            fixed (byte* up = u, op = big)
                status = Native.AmountWords(amount, up, (nuint)ul, (uint)style, op, (nuint)big.Length, &len);
            return Native.Finish(status, big.AsSpan(0, (int)len));
        }
        return Native.Finish(status, buf[..(int)len]);
    }

    /// <summary>Reads <paramref name="n"/> in lower case: 105 is "một trăm linh năm".</summary>
    public static string Number(ulong n, VnWordsStyle style = VnWordsStyle.None)
    {
        Native.Ensure();
        // The longest reading of a u64 is about 400 bytes.
        Span<byte> buf = stackalloc byte[Native.StackOutput];
        nuint len;
        int status;
        fixed (byte* op = buf)
            status = Native.NumberWords(n, (uint)style, op, (nuint)buf.Length, &len);
        return Native.Finish(status, buf[..(int)Math.Min(len, (nuint)buf.Length)]);
    }
}

/// <summary>Vietnamese text helpers computed by DevLibrary dl-vn (Rust).</summary>
public static unsafe class VnText
{
    /// <summary>Removes Vietnamese diacritics, NFC or NFD input: "Đặng Thị Hương" → "Dang Thi Huong".</summary>
    public static string RemoveDiacritics(string text)
    {
        ArgumentNullException.ThrowIfNull(text);
        Native.Ensure();
        int tl = Native.Utf8Length(text);
        Span<byte> t = tl <= Native.StackInput ? stackalloc byte[Native.StackInput] : new byte[tl];
        Native.Encode(text, t);
        // The output is never longer than the input.
        Span<byte> o = tl <= Native.StackOutput ? stackalloc byte[Native.StackOutput] : new byte[tl];
        nuint len;
        int status;
        fixed (byte* tp = t, op = o)
            status = Native.RemoveDiacritics(tp, (nuint)tl, op, (nuint)o.Length, &len);
        return Native.Finish(status, o[..(int)Math.Min(len, (nuint)o.Length)]);
    }
}

/// <summary>Options for <see cref="VietQr.Payload"/>.</summary>
[Flags]
public enum VietQrOptions : uint
{
    /// <summary>Static code (01 = "11"), transfer to account.</summary>
    None = 0,
    /// <summary>One-payment code (01 = "12").</summary>
    Dynamic = 1,
    /// <summary>Transfer to a card number (QRIBFTTC).</summary>
    Card = 1 << 1,
    /// <summary>Make message and bill number valid instead of rejecting them: remove diacritics,
    /// keep printable ASCII, collapse spaces, cut to 25 characters.</summary>
    FoldText = 1 << 2,
}

/// <summary>NAPAS VietQR payloads (EMVCo MPM) built and verified by DevLibrary dl-vn (Rust).</summary>
public static unsafe class VietQr
{
    /// <summary>
    /// Builds the payload string to encode as a QR code, CRC included.
    /// <paramref name="amount"/> is in đồng; 0 means no amount (the payer enters it).
    /// </summary>
    /// <exception cref="ArgumentException">A field is invalid; the message names the VietQR field.</exception>
    public static string Payload(string bankBin, string account, ulong amount = 0,
        VietQrOptions options = VietQrOptions.None, string? billNumber = null, string? purpose = null)
    {
        ArgumentNullException.ThrowIfNull(bankBin);
        ArgumentNullException.ThrowIfNull(account);
        Native.Ensure();
        int bl = Native.Utf8Length(bankBin), al = Native.Utf8Length(account);
        int ll = Native.Utf8Length(billNumber), pl = Native.Utf8Length(purpose);
        int total = bl + al + ll + pl;
        Span<byte> input = total <= Native.StackInput ? stackalloc byte[Native.StackInput] : new byte[total];
        Native.Encode(bankBin, input);
        Native.Encode(account, input[bl..]);
        Native.Encode(billNumber, input[(bl + al)..]);
        Native.Encode(purpose, input[(bl + al + ll)..]);
        // A VietQR payload is at most a few hundred bytes; error messages are short.
        Span<byte> o = stackalloc byte[Native.StackOutput];
        nuint len;
        int status;
        fixed (byte* ip = input, op = o)
            status = Native.VietQrPayload(ip, (nuint)bl, ip + bl, (nuint)al, amount, (uint)options,
                ip + bl + al, (nuint)ll, ip + bl + al + ll, (nuint)pl, op, (nuint)o.Length, &len);
        string text = Native.Finish(status, o[..(int)Math.Min(len, (nuint)o.Length)]);
        if (status == Native.InvalidInput) throw new ArgumentException("VietQR: " + text);
        return text;
    }

    /// <summary>Checks every field and the CRC of a payload. Returns false with the reason.</summary>
    public static bool TryVerify(string payload, out string? error)
    {
        ArgumentNullException.ThrowIfNull(payload);
        Native.Ensure();
        int pl = Native.Utf8Length(payload);
        Span<byte> p = pl <= Native.StackOutput ? stackalloc byte[Native.StackOutput] : new byte[pl];
        Native.Encode(payload, p);
        Span<byte> o = stackalloc byte[Native.StackOutput];
        nuint len;
        int status;
        fixed (byte* pp = p, op = o)
            status = Native.VietQrVerify(pp, (nuint)pl, op, (nuint)o.Length, &len);
        string text = Native.Finish(status, o[..(int)Math.Min(len, (nuint)o.Length)]);
        error = status == Native.Ok ? null : text;
        return status == Native.Ok;
    }
}
