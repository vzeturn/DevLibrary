using System;
using System.Diagnostics;
using System.Threading.Tasks;
using DevLibrary;

int failures = 0, checks = 0;

void Eq(string got, string want, string what)
{
    checks++;
    if (got == want) return;
    failures++;
    Console.WriteLine($"FAIL {what}: got \"{got}\", want \"{want}\"");
}

void True(bool cond, string what)
{
    checks++;
    if (cond) return;
    failures++;
    Console.WriteLine($"FAIL {what}");
}

// Amount in words.
Eq(VnWords.Amount(1_050_000), "Một triệu không trăm năm mươi nghìn đồng", "1050000");
Eq(VnWords.Amount(2_010_000), "Hai triệu không trăm mười nghìn đồng", "2010000");
Eq(VnWords.Amount(1_000_000_000_000), "Một nghìn tỷ đồng", "10^12");
Eq(VnWords.Amount(1_000_000_001_000, style: VnWordsStyle.Le), "Một nghìn tỷ không trăm lẻ một nghìn đồng", "10^12+1000 le");
Eq(VnWords.Amount(-500_000), "Âm năm trăm nghìn đồng", "negative");
Eq(VnWords.Amount(-500_000, style: VnWordsStyle.Unsigned), "Năm trăm nghìn đồng", "unsigned");
Eq(VnWords.Amount(0), "Không đồng", "zero");
Eq(VnWords.Amount(125, "đô la Mỹ"), "Một trăm hai mươi lăm đô la Mỹ", "usd");
Eq(VnWords.Amount(5, ""), "Năm", "no unit");
True(VnWords.Amount(long.MinValue).StartsWith("Âm chín tỷ", StringComparison.Ordinal), "long.MinValue");
Eq(VnWords.Number(24, VnWordsStyle.Tu), "hai mươi tư", "tu");
Eq(VnWords.Amount(1, new string('x', 2000)).Length.ToString(), (4 + 2000).ToString(), "long unit uses heap retry");

// Text.
Eq(VnText.RemoveDiacritics("Đặng Thị Hương"), "Dang Thi Huong", "diacritics");
Eq(VnText.RemoveDiacritics("Nguye\u0302\u0303n"), "Nguyen", "nfd");

// VietQR: NAPAS example 6.1.3.
Eq(VietQr.Payload("970403", "0011012345678", 180_000, VietQrOptions.Dynamic, "NPS6869", "thanh toan don hang"),
    "00020101021238570010A00000072701270006970403011300110123456780208QRIBFTTA" +
    "530370454061800005802VN62340107NPS68690819thanh toan don hang63042E2E", "napas 6.1.3");
string folded = VietQr.Payload("970436", "1031933430", 50_000, VietQrOptions.Dynamic | VietQrOptions.FoldText, purpose: "Bàn 5");
True(folded.Contains("0805Ban 5", StringComparison.Ordinal), "fold memo");
True(VietQr.TryVerify(folded, out _), "verify folded");
string corrupted = folded[..^1] + (folded[^1] == '0' ? "1" : "0");
True(!VietQr.TryVerify(corrupted, out string? why) && why != null, "verify corrupted");
try { VietQr.Payload("970436", "1031933430", 50_000, VietQrOptions.Dynamic, purpose: "Bàn 5"); True(false, "unfolded memo must throw"); }
catch (ArgumentException e) { True(e.Message.Contains("62.08", StringComparison.Ordinal), "error names field: " + e.Message); }
try { VietQr.Payload("97043", "1031933430"); True(false, "bad BIN must throw"); }
catch (ArgumentException) { checks++; }

// Thread safety: the library keeps no state.
Parallel.For(0, 64, i => { if (VnWords.Amount(1_050_000 + i * 1000).Length == 0) throw new Exception("empty"); });

// Speed.
for (int i = 0; i < 20_000; i++) VnWords.Amount(3_047_619);
long before = GC.GetAllocatedBytesForCurrentThread();
var sw = Stopwatch.StartNew();
const int N = 1_000_000;
for (int i = 0; i < N; i++) VnWords.Amount(3_047_619);
sw.Stop();
Console.WriteLine($"VnWords.Amount: {sw.Elapsed.TotalMilliseconds * 1e6 / N:F0} ns/op, {(GC.GetAllocatedBytesForCurrentThread() - before) / N} B/op");

Console.WriteLine($"{checks} checks, {failures} failures");
return failures == 0 ? 0 : 1;
