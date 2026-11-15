//! Bank and wallet BINs accepted by VietQR, from the VietQR bank list
//! <https://api.vietqr.io/v2/banks>.

/// A bank or e-wallet that can receive VietQR transfers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bank {
    /// 6-digit acquirer ID (BIN) used in field 38.01.00.
    pub bin: &'static str,
    /// VietQR bank code, e.g. `"VCB"`.
    pub code: &'static str,
    /// Short brand name, e.g. `"Vietcombank"`.
    pub short_name: &'static str,
    /// Full Vietnamese name.
    pub name: &'static str,
}

const fn bank(
    bin: &'static str,
    code: &'static str,
    short_name: &'static str,
    name: &'static str,
) -> Bank {
    Bank {
        bin,
        code,
        short_name,
        name,
    }
}

/// Every entry of the VietQR bank list at <https://api.vietqr.io/v2/banks>: 76 entries,
/// checked field by field against the API on 2026-11-15, in the order the API returns them
/// (large banks first). Names are copied as published, with surrounding spaces trimmed.
///
/// The list includes banks, digital banks and e-wallets. A BIN being listed does not mean
/// every bank app can pay it. Banks merge and rename (for example the API now gives
/// Sacombank's full name as "Sài Gòn Tài Lộc"), so treat this as a snapshot and re-check
/// the API when a BIN is missing. [`super::VietQr`] accepts BINs that are not in this table.
#[rustfmt::skip]
pub static BANKS: [Bank; 76] = [
    bank("970415", "ICB", "VietinBank", "Ngân hàng TMCP Công thương Việt Nam"),
    bank("970436", "VCB", "Vietcombank", "Ngân hàng TMCP Ngoại Thương Việt Nam"),
    bank("970418", "BIDV", "BIDV", "Ngân hàng TMCP Đầu tư và Phát triển Việt Nam"),
    bank("970405", "VBA", "Agribank", "Ngân hàng Nông nghiệp và Phát triển Nông thôn Việt Nam"),
    bank("970448", "OCB", "OCB", "Ngân hàng TMCP Phương Đông"),
    bank("970422", "MB", "MBBank", "Ngân hàng TMCP Quân đội"),
    bank("970407", "TCB", "Techcombank", "Ngân hàng TMCP Kỹ thương Việt Nam"),
    bank("970416", "ACB", "ACB", "Ngân hàng TMCP Á Châu"),
    bank("970432", "VPB", "VPBank", "Ngân hàng TMCP Việt Nam Thịnh Vượng"),
    bank("970423", "TPB", "TPBank", "Ngân hàng TMCP Tiên Phong"),
    bank("970403", "STB", "Sacombank", "Ngân hàng TMCP Sài Gòn Tài Lộc"),
    bank("970437", "HDB", "HDBank", "Ngân hàng TMCP Phát triển Thành phố Hồ Chí Minh"),
    bank("970454", "VCCB", "VietCapitalBank", "Ngân hàng TMCP Bản Việt"),
    bank("970429", "SCB", "SCB", "Ngân hàng TMCP Sài Gòn"),
    bank("970441", "VIB", "VIB", "Ngân hàng TMCP Quốc tế Việt Nam"),
    bank("970443", "SHB", "SHB", "Ngân hàng TMCP Sài Gòn - Hà Nội"),
    bank("970431", "EIB", "Eximbank", "Ngân hàng TMCP Xuất Nhập khẩu Việt Nam"),
    bank("970426", "MSB", "MSB", "Ngân hàng TMCP Hàng Hải"),
    bank("546034", "CAKE", "CAKE", "TMCP Việt Nam Thịnh Vượng - Ngân hàng số CAKE by VPBank"),
    bank("546035", "Ubank", "Ubank", "TMCP Việt Nam Thịnh Vượng - Ngân hàng số Ubank by VPBank"),
    bank("963388", "TIMO", "Timo", "Ngân hàng số Timo by Ban Viet Bank (Timo by Ban Viet Bank)"),
    bank("971005", "VTLMONEY", "ViettelMoney", "Tổng Công ty Dịch vụ số Viettel - Chi nhánh tập đoàn công nghiệp viễn thông Quân Đội"),
    bank("971011", "VNPTMONEY", "VNPTMoney", "Trung tâm dịch vụ tài chính số VNPT- Chi nhánh Tổng công ty truyền thông (VNPT Fintech)"),
    bank("970400", "SGICB", "SaigonBank", "Ngân hàng TMCP Sài Gòn Công Thương"),
    bank("970409", "BAB", "BacABank", "Ngân hàng TMCP Bắc Á"),
    bank("971133", "PVDB", "PVcomBank Pay", "Ngân hàng số PVcomBank"),
    bank("971025", "momo", "MoMo", "CTCP Dịch Vụ Di Động Trực Tuyến"),
    bank("970412", "PVCB", "PVcomBank", "Ngân hàng TMCP Đại Chúng Việt Nam"),
    bank("970414", "MBV", "MBV", "Ngân hàng TNHH MTV Việt Nam Hiện Đại"),
    bank("970419", "NCB", "NCB", "Ngân hàng TMCP Quốc Dân"),
    bank("970424", "SHBVN", "ShinhanBank", "Ngân hàng TNHH MTV Shinhan Việt Nam"),
    bank("970425", "ABB", "ABBANK", "Ngân hàng TMCP An Bình"),
    bank("970427", "VAB", "VietABank", "Ngân hàng TMCP Việt Á"),
    bank("970428", "NAB", "NamABank", "Ngân hàng TMCP Nam Á"),
    bank("970430", "PGB", "PGBank", "Ngân hàng TMCP Thịnh vượng và Phát triển"),
    bank("970433", "VIETBANK", "VietBank", "Ngân hàng TMCP Việt Nam Thương Tín"),
    bank("970438", "BVB", "BaoVietBank", "Ngân hàng TMCP Bảo Việt"),
    bank("970440", "SEAB", "SeABank", "Ngân hàng TMCP Đông Nam Á"),
    bank("970446", "COOPBANK", "COOPBANK", "Ngân hàng Hợp tác xã Việt Nam"),
    bank("970449", "LPB", "LPBank", "Ngân hàng TMCP Lộc Phát Việt Nam"),
    bank("970452", "KLB", "KienLongBank", "Ngân hàng TMCP Kiên Long"),
    bank("668888", "KBank", "KBank", "Ngân hàng Đại chúng TNHH Kasikornbank"),
    bank("970466", "KEBHANAHCM", "KEBHanaHCM", "Ngân hàng KEB Hana – Chi nhánh Thành phố Hồ Chí Minh"),
    bank("970467", "KEBHANAHN", "KEBHANAHN", "Ngân hàng KEB Hana – Chi nhánh Hà Nội"),
    bank("977777", "MAFC", "MAFC", "Công ty Tài chính TNHH MTV Mirae Asset (Việt Nam)"),
    bank("999888", "VBSP", "VBSP", "Ngân hàng Chính sách Xã hội"),
    bank("963668", "BNP HN", "BNP HN", "Ngân hàng BNP Paribas – Chi nhánh Hà Nội"),
    bank("555666", "BIDC", "BIDC", "Ngân hàng Đầu Tư Và Phát Triển Campuchia - Chi nhánh Hà Nội"),
    bank("963666", "BNP HCM", "BNP HCM", "Ngân hàng BNP Paribas – Chi nhánh Thành phố Hồ Chí Minh"),
    bank("971101", "ZALOPAY", "ZaloPay", "Ví điện tử ZaloPay"),
    bank("963688", "BOCHK", "BOCHK", "Ngân hàng Bank of China (Hong Kong) Limited – Chi nhánh Hồ Chí Minh"),
    bank("970465", "BSP", "BSP", "Ngân hàng SinoPac - Chi nhánh Thành phố Hồ Chí Minh"),
    bank("168999", "CUBHCM", "CUBHCM", "Ngân hàng Cathay United Bank – Chi nhánh Hồ Chí Minh"),
    bank("963686", "ESUN", "ESUN", "Ngân hàng Thương mại TNHH E.SUN – Chi nhánh Đồng Nai (ESUNBANK)"),
    bank("963669", "ICBC", "ICBC", "Ngân hàng Công thương Trung Quốc – Chi nhánh thành phố Hà Nội"),
    bank("971032", "MVAS", "MVAS", "Trung tâm Dịch vụ số Mobifone – Chi nhánh Tổng Công ty viễn thông Mobifone"),
    bank("963326", "TNEX", "TNEX", "TNEX by MSB"),
    bank("458761", "HSBC", "HSBC", "Ngân hàng TNHH MTV HSBC (Việt Nam)"),
    bank("970442", "HLBVN", "HongLeong", "Ngân hàng TNHH MTV Hong Leong Việt Nam"),
    bank("970455", "IBK - HN", "IBKHN", "Ngân hàng Công nghiệp Hàn Quốc - Chi nhánh Hà Nội"),
    bank("970456", "IBK - HCM", "IBKHCM", "Ngân hàng Công nghiệp Hàn Quốc - Chi nhánh TP. Hồ Chí Minh"),
    bank("970434", "IVB", "IndovinaBank", "Ngân hàng TNHH Indovina"),
    bank("970406", "Vikki", "Vikki", "Ngân hàng TNHH Một thành viên số Vikki"),
    bank("801011", "NHB HN", "Nonghyup", "Ngân hàng Nonghyup - Chi nhánh Hà Nội"),
    bank("796500", "DBS", "DBSBank", "DBS Bank Ltd - Chi nhánh Thành phố Hồ Chí Minh"),
    bank("970439", "PBVN", "PublicBank", "Ngân hàng TNHH MTV Public Việt Nam"),
    bank("970410", "SCVN", "StandardChartered", "Ngân hàng TNHH MTV Standard Chartered Bank Việt Nam"),
    bank("533948", "CITIBANK", "Citibank", "Ngân hàng Citibank, N.A. - Chi nhánh Hà Nội"),
    bank("422589", "CIMB", "CIMB", "Ngân hàng TNHH MTV CIMB Việt Nam"),
    bank("970458", "UOB", "UnitedOverseas", "Ngân hàng United Overseas - Chi nhánh TP. Hồ Chí Minh"),
    bank("970444", "CBB", "CBBank", "Ngân hàng Thương mại trách nhiệm hữu hạn một thành viên Ngoại thương Công nghệ số (Vietcombank Neo Limited)"),
    bank("970408", "GPB", "GPBank", "Ngân hàng Thương mại TNHH MTV Dầu Khí Toàn Cầu"),
    bank("970421", "VRB", "VRB", "Ngân hàng Liên doanh Việt - Nga"),
    bank("970457", "WVN", "Woori", "Ngân hàng TNHH MTV Woori Việt Nam"),
    bank("970462", "KBHN", "KookminHN", "Ngân hàng Kookmin - Chi nhánh Hà Nội"),
    bank("970463", "KBHCM", "KookminHCM", "Ngân hàng Kookmin - Chi nhánh Thành phố Hồ Chí Minh"),
];

/// Finds a bank by its 6-digit BIN.
pub fn bank_by_bin(bin: &str) -> Option<&'static Bank> {
    BANKS.iter().find(|b| b.bin == bin)
}

/// Finds a bank by VietQR code (`"VCB"`) or short name (`"Vietcombank"`), ignoring ASCII case.
pub fn bank_by_name(name: &str) -> Option<&'static Bank> {
    BANKS
        .iter()
        .find(|b| b.code.eq_ignore_ascii_case(name) || b.short_name.eq_ignore_ascii_case(name))
}
