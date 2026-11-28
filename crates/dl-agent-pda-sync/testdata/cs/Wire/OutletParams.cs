namespace MM.PdaOrder.Sync.Contracts.Wire;

/// <summary>
/// <c>dbo.Para</c> names the cloud never writes (ADR 0154): sequence counters <c>GetRVCSeqNumber</c> and the POS bump (<c>Last*</c>),
/// the business date (<c>Cur*</c>), bill numbering, the interface/member/AR database wiring, training mode and the switches the
/// order batches of ADR 0100 read. Shared by the cloud (an early 403) and the .NET agent (the final check); the Rust agent keeps the same list in <c>commands/master</c>.
/// </summary>
public static class OutletParams
{
    private static readonly string[] Prefixes = ["Last", "Cur", "BillNo"];

    private static readonly HashSet<string> Names = new(StringComparer.OrdinalIgnoreCase)
    {
        "InterfaceDB", "MBRDatabase", "ARDatabase", "CityLedgerFolio", "PostToSmileEnabled", "ARNotPostToDB",
        "TrainingMode", "gTrainingMode", "MemberControl",
        "OrderPrintByMenu", "GroupChkTrn", "OrderByCourse", "qrCodeNeedConfirmOrder",
    };

    public static bool IsProtected(string paraName) =>
        Names.Contains(paraName) || Prefixes.Any(p => paraName.StartsWith(p, StringComparison.OrdinalIgnoreCase));
}
