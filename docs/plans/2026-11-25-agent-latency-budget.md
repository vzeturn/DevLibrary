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

- Lần đo 1 (mục 6) và lần đo 2 (mục 7) đã có; còn lượt giờ cao điểm (chạy `scripts/latency-probe`, mục 7.3).
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

## 7. Số đo lần 2 (RT-1232, 2026-11-25 18:45–19:07 UTC = 01:45–02:07 GMT+7, ngoài giờ cao điểm)

Đo từ iMac qua `azolo-demo-pms` (Fly) tới dl-agent ở SR bằng `scripts/latency-probe/probe.mjs`. Mỗi route: 5 lần khởi động bỏ đi, mẫu tuần tự cách nhau 150 ms, rồi một lượt 10 lệnh song song. Đơn vị ms, đầu-cuối (gồm chặng iMac->Fly). `fo.dashboard.today` không có route riêng: `GET /dashboard` trả `AGENT_TENANT_UNSUPPORTED` trên tenant agent; op này được `GET /dashboard/overview` gọi cùng các op số liệu RT-1079 (`houseMix`, `rooms`, ... theo azolo-pms master), nên số của `fo.dashboard.today` là dòng `/dashboard/overview`.

| Route (op dl-agent gọi) | Cache API | n | p50 | p95 | p99 | 10 song song p50 / p95 |
|---|---|---|---|---|---|---|
| `/health` (không qua agent) | - | 200 | 155.2 | 161.2 | 170.4 | 269.3 / 345.7 |
| `/workspace/frontdesk/today?blocks=inHouse` (`fo.hotelDate.get`, `fo.inhouse.list`), mẫu cách 5.2 s | trượt | 40 | **233.5** | **297.3** | 302.6 | 591.3 / 656.7 † |
| cùng route, mẫu cách 150 ms | phần lớn trúng (5 s) | 200 | 167.9 | 247.9 | 255.5 | 224.3 / 283.5 |
| `/reservations` (`fo.hotelDate.get`, `fo.reservation.list`, `fo.inhouse.list`) | không | 200 | **647.4** | **770.4** | 849.8 | 1869.3 / 2410.0 |
| `/workspace/frontdesk/today` (hotelDate, arrivals, departures, inhouse) | phần lớn trúng (5 s) | 200 | 163.1 | 255.6 | 277.9 | 186.8 / 235.7 |
| `/dashboard/overview` (`fo.dashboard.today` + hotelDate, arrivals, departures, inhouse, op số liệu RT-1079; cache 10 s phần live) | một phần | 196 ‡ | 284.7 | 387.0 | 416.1 | 250.7 / 322.6 |

† Lượt song song mở 10 kết nối TLS mới tới Fly, nên cộng thêm bắt tay TLS.
‡ 4 lỗi 503 `SITE_OFFLINE` rải rác (giống lần 1), không có 5 lỗi liên tiếp.

### 7.1 Đọc số

- Phần SaaS->SR (trừ mốc `/health` ~155 ms của chặng iMac->Fly): `fo.inhouse.list` (233 phòng đang ở) khoảng **80 ms p50 / 140 ms p95**; `/reservations` (3 op) khoảng **490 ms p50 / 610 ms p95**, là màn nặng nhất đã đo và là ứng viên đầu tiên cho tối ưu (gộp op, phân trang).
- Cache 5 s của workspace và 10 s của overview che phần lớn chi phí agent khi người dùng tải lại; số "trúng cache" không dùng để chốt ngưỡng agent.
- Lần đo cách 5.2 s phải giữ kết nối ấm bằng `/health` không tính giờ: nếu không, keep-alive 4 s của undici đóng kết nối và mỗi mẫu cộng thêm ~180 ms bắt tay TLS (lượt thử đầu: p50 415 ms, đã bỏ).
- `SITE_OFFLINE` lặp lại ở cả hai lần đo (5/65 lần 1, 4/200 lần 2): đáng một yêu cầu riêng để xem heartbeat/kết nối agent có chập chờn không.

### 7.2 Vẫn chưa làm được

- Tách thời gian SQL: `sql.perf` chưa thấy kết nối của module PMS (tên `dl-agent-<version>`).
- Ping trực tiếp tới SR: không có IP/probe mạng.
- `pos.check.pay`: không có dry-run, không ghi thử trên khách sạn thật.

### 7.3 Chạy lại (giờ cao điểm hoặc sau tối ưu)

Công cụ: `scripts/latency-probe/` (README ở đó). Khoá `AZOLO_DEMO_VIRTUAL_CLERK_*`, `AZOLO_DEMO_POS_CASHIER_PIN` đã có trong kit cloud của DevLibrary (RT-1232) và trong `~/.config/rt-kits/smile-pms.env` trên iMac. Đường đo là HTTPS công khai tới `azolo-demo-pms.pms.id.vn`, nên phiên cloud chạy được nếu proxy cho ra host đó; nếu bị chặn thì iMac chạy.
