using System.Net;
using System.Text;
using MM.PdaOrder.Sync.Contracts.Staff;
using MM.PdaOrder.Sync.Contracts.Wire;
using MM.PdaOrder.SyncAgent.Staff;
using MM.PdaOrder.SyncAgent.Transport;

namespace MM.PdaOrder.SyncAgent.Tests.Staff;

/// <summary>
/// The agent side of the PIN lookup v2 (ADR 0101). The golden vector is shared with the cloud's <c>Tokens</c> tests: pepper
/// <c>golden-pepper</c>, tenant <c>11111111-2222-3333-4444-555555555555</c>, PIN <c>482193</c>. Both sides must produce
/// these exact bytes or no POS clerk can sign in.
/// </summary>
public sealed class StaffPinHasherTests
{
    internal static readonly byte[] GoldenKey = Convert.FromHexString("7981d8db9f045a37dd51f14695d4d1897749a3f6726c4f22b5cfe38880fa9915");
    internal const string GoldenKeyId = "e346975cd4c277eb";
    internal const string GoldenPin = "482193";
    internal const string GoldenLookup = "FnBxjjdDkD4NU/2PPVfVQ+rDq2ELINJH2Z63OYZEh64=";

    [Fact]
    public void A_usable_pin_hashes_to_the_golden_lookup_with_the_golden_key_id()
    {
        StaffPinHasher.Hash(GoldenKey, GoldenPin, active: true).Should().Be((GoldenLookup, StaffPinStatus.Set));
        StaffPin.KeyId(GoldenKey).Should().Be(GoldenKeyId);
    }

    [Fact]
    public void Trailing_padding_is_trimmed_as_the_POS_sign_in_did_and_leading_padding_is_not()
    {
        StaffPinHasher.Hash(GoldenKey, GoldenPin + "   ", true).Should().Be((GoldenLookup, StaffPinStatus.Set));
        StaffPinHasher.Hash(GoldenKey, " " + GoldenPin, true).Should().Be(((string?)null, StaffPinStatus.Unusable));
    }

    [Fact]
    public void A_disabled_clerk_carries_no_lookup_even_with_a_pin()
    {
        StaffPinHasher.Hash(GoldenKey, GoldenPin, active: false).Should().Be(((string?)null, StaffPinStatus.None));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("    ")]
    public void No_pin_on_the_POS_is_status_none(string? touchId)
    {
        StaffPinHasher.Hash(GoldenKey, touchId, true).Should().Be(((string?)null, StaffPinStatus.None));
    }

    [Theory]
    [InlineData("1234567890123")]
    [InlineData("12ab")]
    [InlineData("12 34")]
    [InlineData("١٢٣٤")]
    public void A_pin_the_cloud_keypad_cannot_type_is_unusable_and_never_hashed(string touchId)
    {
        StaffPinHasher.Hash(GoldenKey, touchId, true).Should().Be(((string?)null, StaffPinStatus.Unusable));
    }

    [Fact]
    public void One_to_twelve_digits_are_all_usable_since_POS_WIN_takes_short_TouchIDs()
    {
        StaffPinHasher.Hash(GoldenKey, "7", true).Status.Should().Be(StaffPinStatus.Set);
        StaffPinHasher.Hash(GoldenKey, "323", true).Status.Should().Be(StaffPinStatus.Set);
        StaffPinHasher.Hash(GoldenKey, "0000", true).Status.Should().Be(StaffPinStatus.Set);
        StaffPinHasher.Hash(GoldenKey, "123456789012", true).Status.Should().Be(StaffPinStatus.Set);
    }

    [Fact]
    public void The_pin_key_answer_is_used_only_when_it_is_32_bytes_and_matches_its_fingerprint()
    {
        static SyncResponse Answer(string json) => new(HttpStatusCode.OK, Encoding.UTF8.GetBytes(json));
        var b64 = Convert.ToBase64String(GoldenKey);

        var ok = StaffCloudClient.ParseKey(Answer($$$"""{"result":200,"data":{"key":"{{{b64}}}","key_id":"{{{GoldenKeyId}}}"}}"""));
        ok.Status.Should().Be(StaffCloudStatus.Ok);
        ok.PinKey!.Key.Should().Equal(GoldenKey);
        ok.PinKey.KeyId.Should().Be(GoldenKeyId);

        StaffCloudClient.ParseKey(Answer($$$"""{"data":{"key":"{{{b64}}}","key_id":"0000000000000000"}}""")).Status
            .Should().Be(StaffCloudStatus.Failed);
        StaffCloudClient.ParseKey(Answer($$$"""{"data":{"key":"{{{Convert.ToBase64String(new byte[16])}}}","key_id":"x"}}""")).Status
            .Should().Be(StaffCloudStatus.Failed);
        StaffCloudClient.ParseKey(Answer("""{"data":{"key":"not base64!","key_id":"x"}}""")).Status.Should().Be(StaffCloudStatus.Failed);
        StaffCloudClient.ParseKey(Answer("""{"data":{}}""")).Status.Should().Be(StaffCloudStatus.Failed);
        StaffCloudClient.ParseKey(Answer("not json")).Status.Should().Be(StaffCloudStatus.Failed);
    }
}
