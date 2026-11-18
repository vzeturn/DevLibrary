// A minimal hub for cross-checking dl-signalr against ASP.NET Core SignalR.
//
//   dotnet run --project crates/dl-signalr/testdata/hub --urls http://127.0.0.1:5199
//   DL_SIGNALR_TEST_URL=http://127.0.0.1:5199/hub cargo test -p dl-signalr --test aspnet -- --ignored
//
// Access check without JWT: negotiate must carry "Authorization: Bearer <token>", the
// WebSocket request must carry "?access_token=<token>" (as browsers send it).

using System.Runtime.CompilerServices;
using System.Text.Json;
using Microsoft.AspNetCore.SignalR;

var builder = WebApplication.CreateBuilder(args);
builder.Logging.SetMinimumLevel(LogLevel.Warning);
builder.Services.AddSignalR(o =>
{
    // Short intervals so the keepalive test runs in seconds.
    o.KeepAliveInterval = TimeSpan.FromSeconds(1);
    o.ClientTimeoutInterval = TimeSpan.FromSeconds(3);
});
var app = builder.Build();
app.UseWebSockets();

var token = Environment.GetEnvironmentVariable("DL_SIGNALR_TEST_TOKEN") ?? "dl-test token+/=";

app.Use(async (ctx, next) =>
{
    if (ctx.Request.Path.StartsWithSegments("/hub"))
    {
        var isNegotiate = ctx.Request.Path.Value!.EndsWith("/negotiate", StringComparison.Ordinal);
        string? presented;
        if (isNegotiate)
        {
            var auth = ctx.Request.Headers.Authorization.ToString();
            presented = auth.StartsWith("Bearer ", StringComparison.Ordinal) ? auth["Bearer ".Length..] : null;
        }
        else
        {
            presented = ctx.Request.Query["access_token"].ToString();
        }
        if (presented != token)
        {
            ctx.Response.StatusCode = StatusCodes.Status401Unauthorized;
            await ctx.Response.WriteAsync("missing or wrong access token");
            return;
        }
    }
    await next(ctx);
});

app.MapHub<TestHub>("/hub");
app.MapGet("/", () => "dl-signalr test hub");
app.Run();

public class TestHub : Hub
{
    public string Echo(string text) => text;

    public int Add(int a, int b) => a + b;

    // JsonElement keeps the exact JSON text, so numbers round-trip digit for digit.
    public JsonElement EchoJson(JsonElement value) => value;

    public void Nothing() { }

    public void Fail() => throw new HubException("boom");

    public void Crash() => throw new InvalidOperationException("secret detail");

    public Task Notify(string text) => Clients.Caller.SendAsync("Notify", text, 42);

    public async Task<string> Delay(int ms)
    {
        await Task.Delay(ms);
        return "late";
    }

    public async IAsyncEnumerable<int> Counter(int count, [EnumeratorCancellation] CancellationToken ct)
    {
        for (var i = 0; i < count; i++)
        {
            yield return i;
            await Task.Delay(10, ct);
        }
    }

    public void Kick() => Context.Abort();
}
