# Agent SR: ngân sách độ trễ round-trip SaaS -> dl-agent (RT-1161, tách từ RT-1139)

Ngày: 2026-11-25. Chỉ tài liệu, không sửa mã.

## 1. Trạng thái nói thẳng

**Chưa có số đo p50/p95 thật.** Phiên cloud không có đường mạng tới dl-agent trên SR nên không đo được round-trip thật. Theo chỉ đạo của chủ dự án (25/11): bỏ so sánh với IIS; thay vào đó tự đánh giá độ trễ IIS server -> database server rồi đặt mốc cho đường cloud. Tài liệu này là ngân sách suy ra từ mã và kiến trúc. Các con số "mục tiêu" là đề xuất, chưa phải kết quả đo.

## 2. Đường đi của một lệnh

Đường IIS on-prem: web app -> SQL Server trong cùng LAN (một hop LAN, thường 0.3-2 ms mỗi lần gọi proc). Độ trễ chủ yếu là thời gian chạy proc.

Đường SaaS: SaaS -> kết nối WebSocket do agent giữ sẵn (`crates/dl-agent-pms/src/conn.rs`, heartbeat + `idle_timeout`; không phải poll) -> `SqlExecutor::execute` (`exec/mod.rs`) -> SQL Server cục bộ trên SR -> trả kết quả về cùng kết nối. Vì kết nối đã mở, mỗi lệnh chỉ trả thêm: 1 RTT internet SaaS<->SR, giải mã/ký lệnh, chạy SQL (như IIS), mã hoá kết quả.

Vậy: độ trễ SaaS = độ trễ IIS (thời gian SQL) + RTT internet (cố định, không tối ưu được trong agent) + phần xử lý của agent (tối ưu được).

## 3. Ngân sách đề xuất (chờ chủ dự án chốt)

| Thành phần | Mục tiêu | Ai kiểm soát |
|---|---|---|
| RTT internet SaaS<->SR | đo thực tế; coi là cố định | hạ tầng/ISP |
| Phần agent thêm (giải mã, xác thực, dispatch, đóng gói kết quả) | p95 <= 10 ms cho op đọc nhỏ | dl-agent |
| SQL | ngang IIS (cùng proc/select) | DB |
| Tổng op đọc nhỏ (vd `fo.hotelDate.get`, `pos.menu.items`) | p95 <= RTT + 30 ms | |
| Tổng op ghi (vd `pos.check.pay`, có journal) | p95 <= RTT + 60 ms | |
| Op nặng (`fo.dashboard.*`, tới 10000 dòng) | p95 <= RTT + 500 ms; cần phân trang/nén nếu vượt | |

Các ngưỡng trên là điểm khởi đầu hợp lý, không phải kết quả đo.

## 4. Cách đo (để chạy khi có máy trong mạng SR)

1. Chạy từ một máy có đường tới hub/agent SR (iMac hoặc máy ở SR), ngoài giờ cao điểm và một lần trong giờ cao điểm.
2. Chọn lệnh chính: `fo.hotelDate.get`, `fo.inhouse.list`, `fo.reservation.list`, `pos.menu.items`, `pos.check.list`, `pos.check.pay` (ghi, dùng `request_key` thử), `fo.dashboard.today`.
3. Mỗi lệnh: 5 lần khởi động bỏ đi, rồi >= 200 mẫu tuần tự (và một lượt 10 lệnh song song), ghi thời gian từ lúc gửi tới lúc nhận kết quả, tách riêng thời gian SQL (`pms.op_dry_run`/log agent) khỏi RTT.
4. Đường đối chiếu IIS không còn bắt buộc; chỉ cần đo SQL cục bộ trên SR (cùng câu lệnh) để biết phần SQL.
5. Báo p50, p95, p99, số mẫu, thời điểm đo, và RTT ping tới SR.

## 5. Việc còn lại

- Lần đo đầu đã có (mục 6); còn 3 lệnh và lượt giờ cao điểm.
- Chủ dự án chốt ngưỡng p95 sau khi xem số đo (tiêu chí 3).

## 6. Số đo lần 1 (RT-1232, 2026-11-25 16:52–16:55 UTC, ngoài giờ cao điểm)

Đo từ iMac qua `azolo-demo-pms` (Fly) tới dl-agent 1.3.46 ở SR. Mỗi lệnh: 5 lần khởi động bỏ đi, rồi mẫu tuần tự và một lượt 10 lệnh song song. Đơn vị ms, đầu-cuối (gồm chặng iMac->Fly).

| Lệnh | n | p50 | p95 | p99 | 10 song song p50 / p95 |
|---|---|---|---|---|---|
| `fo.hotelDate.get` | 60* | 250.7 | 327.9 | 476.7 | 480.7 / 529.7 |
| `pos.menu.items` | 200 | 312.3 | 372.5 | 462.5 | 964.9 / 1138.5 |
| `pos.check.list` | 200 | 232.5 | 239.4 | 247.0 | 484.5 / 548.8 |

\* `fo.hotelDate.get` dừng ở 60 mẫu sau 5 lỗi liên tiếp 503 `SITE_OFFLINE`.

- Mốc không qua agent: `/health` p50 159.3, p95 163.0 (n=200). Ping iMac->Fly trung bình 90.4 ms (min 83.5).
- Metrics phía app trước lần đo: phần site (mạng + agent + SQL) trung bình ~107 ms trên 571 op; phần cloud ~4 ms.
- Ước lượng: phần SaaS->SR (trừ chặng iMac->Fly ~160 ms) cho op đọc nhỏ khoảng 75–150 ms p50.
- Bỏ qua `pos.check.pay`: không có chế độ dry-run (`pms.op_dry_run` chưa tồn tại), không ghi thử trên khách sạn thật.
- Không ping được SR (không biết IP, agent không có probe mạng). Không tách được thời gian SQL: `sql.perf` chỉ thấy kết nối tên `dl-agent`, module PMS kết nối bằng `dl-agent-<version>`.
- Còn lại: `fo.inhouse.list`, `fo.reservation.list`, `fo.dashboard.today`; một lượt đo giờ cao điểm; sửa tên kết nối để `sql.perf` tách được SQL.
