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

## 8. Số đo lần 3 (RT-1161, 2026-11-27 02:43–02:53 UTC = 09:43–09:53 GMT+7, giờ cao điểm sáng)

Cùng cách đo mục 7: `node scripts/latency-probe/probe.mjs --samples 200 --gap 150` từ iMac, qua `azolo-demo-pms` tới dl-agent ở SR; 5 lần khởi động bỏ đi, 200 mẫu tuần tự mỗi route, một lượt 10 song song. Đơn vị ms, đầu-cuối. "Phần SaaS->SR" = trừ mốc `/health` (p50 163.3).

| Route | n | p50 | p95 | p99 | Phần SaaS->SR p50 | 10 song song p50 / p95 |
|---|---|---|---|---|---|---|
| `/health` (không qua agent) | 200 | 163.3 | 206.6 | 222.3 | - | 195.7 / 264.5 |
| `fo.hotelDate.get` | 200 | 224.4 | 259.8 | 290.2 | ~61 | 203.3 / 287.8 |
| `pos.check.list` | 200 | 224.2 | 237.1 | 274.9 | ~61 | 213.5 / 292.7 |
| `fo.inhouse.list` (+hotelDate; cache 5 s trúng) | 200 | 176.0 | 233.3 | 259.7 | ~13 | 214.9 / 276.2 |
| `frontdesk.today` (4 op; cache 5 s trúng) | 200 | 180.8 | 301.9 | 335.6 | ~18 | 192.4 / 257.7 |
| `dashboard.overview` (7 op; cache một phần) | 200 | 233.1 | 327.5 | 359.8 | ~70 | 230.8 / 292.0 |
| `fo.reservation.list` (`/reservations`, 3 op, không cache) | 200 | **604.5** | **721.6** | 884.2 | ~441 | 1681.3 / 2669.7 |

- Không có lỗi 5xx nào trong lượt này (lần 1-2 có `SITE_OFFLINE` rải rác).
- `pos.menu.items` không đo được trong lượt này: route trả `422 VALIDATION` (thiếu `menuNo`). Đã đo bù ở mục 8.2.
- Giờ cao điểm gần như không làm chậm so với lần 2 ngoài giờ: `/reservations` 604/722 so với 647/770, `dashboard.overview` 233/328 so với 285/387. Nghĩa là độ trễ bị chi phối bởi số op tuần tự và RTT, không phải tải SR.
- 10 song song trên `/reservations` p95 2.67 s: hàng đợi tuần tự của agent trên một kết nối; đây là điểm RT-1162 cần giải quyết trước.

### 8.1 Đề xuất ngưỡng p95 (chờ chủ dự án chốt, tiêu chí 3)

Chủ dự án (25/11) không cần so IIS: mục tiêu là cloud sát độ trễ IIS -> DB trong cùng LAN (vài ms mỗi truy vấn), nên phần agent cộng thêm phải nhỏ so với RTT internet. Số đo cho thấy op đọc nhỏ đã cách mốc `/health` chừng 60 ms p50; màn nhiều op nặng hơn nhiều. Đề xuất, tính **trên mốc `/health` đo cùng lúc**:

| Loại lệnh | Ngưỡng p95 đề xuất (cộng vào `/health` p95) | Hiện tại (lần 3) | Đạt? |
|---|---|---|---|
| Op đọc nhỏ (1-2 op: `fo.hotelDate.get`, `pos.check.list`, `fo.inhouse.list`) | +80 ms | +30 đến +55 ms | Đạt |
| Màn tổng hợp 4-7 op (`frontdesk.today`, `dashboard.overview`) | +150 ms | +95 đến +120 ms | Đạt |
| Màn 3 op không cache (`/reservations`) | +350 ms | +515 ms | **Chưa** (ứng viên tối ưu RT-1162) |
| Lệnh ghi (`pos.check.pay`) | +120 ms | chưa đo (không có dry-run) | Chưa đo |
| 10 yêu cầu song song, màn nặng | p95 <= 1.0 s | 2.67 s | **Chưa** |

Đo lại bằng cùng lệnh sau mỗi bước tối ưu; `/health` p95 dao động ~160-210 ms giữa các lần nên luôn so theo mốc cùng lượt.

### 8.2 Đo bù `pos.menu.items` (RT-1161, 2026-11-27 04:05–04:07 UTC = 11:05–11:07 GMT+7)

Probe đã sửa gửi `?menuNo=1` (route bắt buộc `menuNo`). Cùng cách đo: `--samples 200 --gap 150 --only pos.menu.items`, 5 lần khởi động bỏ đi, 200 mẫu tuần tự, một lượt 10 song song, không lỗi.

| Route | n | p50 | p95 | p99 | 10 song song p50 / p95 |
|---|---|---|---|---|---|
| `pos.menu.items` (`/pos/menu/items?menuNo=1`) | 200 | 319.6 | 387.5 | 445.3 | 548.3 / 884.6 |

- So với mốc `/health` (~160 ms p50): phần SaaS->SR khoảng **+160 ms p50 / +180 ms p95**, cao hơn op đọc nhỏ khác (+60 ms) vì op này chạy 2 truy vấn tuần tự (`RVCItemMenu`, `ItemDef`). Với ngưỡng đề xuất "+80 ms cho op đọc nhỏ" thì **chưa đạt**; đề xuất xếp nó vào nhóm "op 2 truy vấn: +200 ms" hoặc gộp 2 truy vấn thành một JOIN (việc cho RT-1162).
- Thực đo trên dữ liệu demo: menu 1-3, 5, 10 đều trả mảng rỗng (2 byte), nên chưa phản ánh thời gian truyền/đọc của thực đơn thật vài trăm món; số trên là cận dưới.
- `pos.check.pay` vẫn không đo: không có dry-run, không ghi thử trên khách sạn thật; lý do bỏ này cần chủ dự án duyệt cùng ngưỡng ở 8.1.
