# DevLibrary for .NET

A thin binding over the DevLibrary C ABI ([`crates/dl-ffi`](../../crates/dl-ffi)). The
rules are computed by the Rust crates; the C# code only converts strings and status codes.
No `PackageReference`, AOT and trimming compatible, `net8.0` and `net10.0`.

```csharp
using DevLibrary;

VnWords.Amount(1_050_000);                         // "Một triệu không trăm năm mươi nghìn đồng"
VnWords.Amount(105, style: VnWordsStyle.Le);       // "Một trăm lẻ năm đồng"
VnWords.Amount(-500_000);                          // "Âm năm trăm nghìn đồng"
VnText.RemoveDiacritics("Bàn 5");                  // "Ban 5"
VietQr.Payload("970436", "1031933430", 150_000,
    VietQrOptions.Dynamic | VietQrOptions.FoldText, purpose: "Bàn 5");   // CRC included
VietQr.TryVerify(payload, out string? error);
```

## Native libraries

The package carries `runtimes/<rid>/native/dl_ffi.*`; .NET loads the right one for the
process RID. A self-contained or RID-specific publish (`-r linux-x64`) copies it next to
the app. Supported RIDs: `win-x64`, `linux-x64` (glibc ≥ 2.36), with `linux-arm64` and
`osx-arm64` built by CI.

Local build of the native libraries and the package:

```powershell
./build-native.ps1          # win-x64 with cargo, linux-x64 with Docker
dotnet pack DevLibrary -c Release -o ../../target/nuget
dotnet run -c Release --project Tests
```

## Errors

- `ArgumentException`: the input was rejected (the message names the VietQR field).
- `DllNotFoundException`: the native library for this RID is missing.
- `InvalidOperationException`: the loaded native library has another ABI version.
