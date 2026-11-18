// Writes the .NET-made test archives in this directory and checks archives written by dl-zip.
// Needs the .NET 10 SDK (file-based app, no project file):
//
//   dotnet run gen_dotnet_zips.cs -- gen <dir>      writes dotnet_*.zip (+ .txt manifests)
//   dotnet run gen_dotnet_zips.cs -- verify <dir>   opens every <dir>/*.zip with
//                                                   System.IO.Compression and checks it
//                                                   against <dir>/<name>.txt
//
// Manifest lines (UTF-8, one per entry in central directory order):
//   <name> TAB dir
//   <name> TAB <size> TAB <sha256 hex> TAB <DOS time as yyyy-MM-ddTHH:mm:ss>
//
// The archives for `verify` come from `cargo test -p dl-zip -- --ignored export_for_dotnet`
// (directory: $DL_ZIP_EXPORT, default <temp>/dl-zip-export).

#:property PublishAot=false

using System.IO.Compression;
using System.Security.Cryptography;
using System.Text;

if (args.Length != 2)
{
    Console.Error.WriteLine("usage: gen|verify <dir>");
    return 2;
}
return args[0] switch
{
    "gen" => Z.Gen(args[1]),
    "verify" => Z.Verify(args[1]),
    _ => 2,
};

static class Z
{
    static readonly DateTime Stamp = new(2024, 5, 6, 7, 8, 10);

    static byte[] Text(string s) => Encoding.UTF8.GetBytes(s);

    public static int Gen(string dir)
    {
        Directory.CreateDirectory(dir);
        var csv = new StringBuilder();
        for (int i = 0; i < 2000; i++) csv.Append($"{i},{i * i},Phòng {100 + i % 50}\n");
        (string, byte[])[] files =
        [
            ("hello.txt", Text("Hello from System.IO.Compression\n")),
            ("data/numbers.csv", Text(csv.ToString())),
            ("data/empty.txt", []),
            ("Tài liệu/hóa đơn tháng 5.json", Text("{\"khách\":\"Nguyễn Văn A\",\"tổng\":\"1.250.000\"}")),
        ];
        Write(dir, "dotnet_deflate", files, CompressionLevel.Optimal, true, null);
        Write(dir, "dotnet_stored", files, CompressionLevel.NoCompression, true, null);
        // An unseekable output makes .NET write data descriptors (flag bit 3).
        Write(dir, "dotnet_unseekable", files, CompressionLevel.Fastest, false, null);
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        Write(dir, "dotnet_cp437", [("café_Ñ.txt", Text("cp437\n")), ("Ä/ö.txt", Text("x"))],
            CompressionLevel.Optimal, true, Encoding.GetEncoding(437));

        // More than 65 535 entries: .NET writes the ZIP64 end of central directory records.
        // Entry i is "f/<i>" containing "x", except every 4096th, which contains "<i>".
        // Stored gzipped to keep the repository small.
        using (var ms = new MemoryStream())
        {
            using (var za = new ZipArchive(ms, ZipArchiveMode.Create, true))
            {
                for (int i = 0; i < 65_600; i++)
                {
                    var e = za.CreateEntry($"f/{i}", CompressionLevel.NoCompression);
                    e.LastWriteTime = Stamp;
                    using var s = e.Open();
                    s.Write(Text(i % 4096 == 0 ? i.ToString() : "x"));
                }
            }
            ms.Position = 0;
            using var gz = new GZipStream(File.Create(Path.Combine(dir, "dotnet_zip64.zip.gz")),
                CompressionLevel.SmallestSize);
            ms.CopyTo(gz);
        }
        Console.WriteLine("GEN OK");
        return 0;
    }

    static void Write(string dir, string stem, (string Name, byte[] Data)[] files,
        CompressionLevel level, bool seekable, Encoding? names)
    {
        var lines = new List<string>();
        using (var fs = File.Create(Path.Combine(dir, stem + ".zip")))
        {
            Stream s = seekable ? fs : new NoSeek(fs);
            using var za = new ZipArchive(s, ZipArchiveMode.Create, true, names);
            foreach (var (name, data) in files)
            {
                var e = za.CreateEntry(name, level);
                e.LastWriteTime = Stamp;
                using (var es = e.Open()) es.Write(data);
                lines.Add($"{name}\t{data.Length}\t{Sha256Hex(data)}\t{Stamp:s}");
            }
            if (names == null)
            {
                var d = za.CreateEntry("empty_dir/");
                d.LastWriteTime = Stamp;
                lines.Add("empty_dir/\tdir");
            }
        }
        File.WriteAllText(Path.Combine(dir, stem + ".txt"), string.Join("\n", lines) + "\n",
            new UTF8Encoding(false));
    }

    public static int Verify(string dir)
    {
        int bad = 0, archives = 0;
        foreach (var path in Directory.GetFiles(dir, "*.zip").OrderBy(p => p, StringComparer.Ordinal))
        {
            archives++;
            var name = Path.GetFileName(path);
            var expected = File.ReadAllLines(Path.ChangeExtension(path, ".txt"), Encoding.UTF8)
                .Where(l => l.Length > 0).Select(l => l.Split('\t')).ToList();
            try
            {
                using var za = ZipFile.OpenRead(path);
                if (za.Entries.Count != expected.Count)
                {
                    Console.WriteLine($"FAIL {name}: {za.Entries.Count} entries, expected {expected.Count}");
                    bad++;
                    continue;
                }
                for (int i = 0; i < expected.Count; i++)
                {
                    var e = za.Entries[i];
                    var x = expected[i];
                    if (e.FullName != x[0])
                    {
                        Console.WriteLine($"FAIL {name}[{i}]: name {e.FullName} != {x[0]}");
                        bad++;
                        continue;
                    }
                    if (x[1] == "dir")
                    {
                        if (e.Length != 0 || !e.FullName.EndsWith('/'))
                        {
                            Console.WriteLine($"FAIL {name}[{i}]: not a directory");
                            bad++;
                        }
                        continue;
                    }
                    byte[] data;
                    using (var s = e.Open())
                    using (var ms = new MemoryStream())
                    {
                        s.CopyTo(ms);
                        data = ms.ToArray();
                    }
                    var why = new List<string>();
                    if (data.Length.ToString() != x[1]) why.Add($"length {data.Length} != {x[1]}");
                    if (Sha256Hex(data) != x[2]) why.Add("sha256");
                    if (Crc32(data) != e.Crc32) why.Add($"crc {Crc32(data):x8} != {e.Crc32:x8}");
                    if (x.Length > 3 && e.LastWriteTime.DateTime.ToString("s") != x[3])
                        why.Add($"time {e.LastWriteTime.DateTime:s} != {x[3]}");
                    if (why.Count > 0)
                    {
                        Console.WriteLine($"FAIL {name}[{i}] {x[0]}: {string.Join(", ", why)}");
                        bad++;
                    }
                }
                if (za.Entries.Count <= 1000)
                {
                    var tmp = Path.Combine(Path.GetTempPath(), "dl-zip-verify-" + Guid.NewGuid().ToString("N"));
                    ZipFile.ExtractToDirectory(path, tmp);
                    Directory.Delete(tmp, true);
                }
                Console.WriteLine($"ok {name}: {za.Entries.Count} entries");
            }
            catch (Exception ex)
            {
                Console.WriteLine($"FAIL {name}: {ex.GetType().Name}: {ex.Message}");
                bad++;
            }
        }
        Console.WriteLine(bad == 0 && archives > 0
            ? $"VERIFY OK ({archives} archives)"
            : $"VERIFY FAILED ({bad} problems, {archives} archives)");
        return bad == 0 && archives > 0 ? 0 : 1;
    }

    static string Sha256Hex(byte[] data) => Convert.ToHexStringLower(SHA256.HashData(data));

    static uint Crc32(byte[] data)
    {
        uint c = 0xFFFFFFFF;
        foreach (var b in data)
        {
            c ^= b;
            for (int k = 0; k < 8; k++) c = (c & 1) != 0 ? (c >> 1) ^ 0xEDB88320 : c >> 1;
        }
        return ~c;
    }
}

/// A write-only stream that cannot seek, like a network or pipe stream.
sealed class NoSeek(Stream inner) : Stream
{
    public override bool CanRead => false;
    public override bool CanSeek => false;
    public override bool CanWrite => true;
    public override long Length => throw new NotSupportedException();
    public override long Position
    {
        get => throw new NotSupportedException();
        set => throw new NotSupportedException();
    }
    public override void Flush() => inner.Flush();
    public override int Read(byte[] buffer, int offset, int count) => throw new NotSupportedException();
    public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
    public override void SetLength(long value) => throw new NotSupportedException();
    public override void Write(byte[] buffer, int offset, int count) => inner.Write(buffer, offset, count);
}
