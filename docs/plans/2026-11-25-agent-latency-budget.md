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

- Chạy phép đo mục 4 ở SR (cần máy trong mạng SR; cloud không làm được).
- Chủ dự án chốt ngưỡng p95 sau khi xem số đo (tiêu chí 3).
