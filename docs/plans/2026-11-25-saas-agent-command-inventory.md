# Kiểm kê lệnh SaaS gọi xuống dl-agent và lệnh còn thiếu (RT-1159, tách từ RT-1139)

Ngày: 2026-11-25. Chỉ đọc: không sửa mã agent. Phạm vi: dl-agent trên SR phục vụ PMS và PDAOrder SaaS.

**Giới hạn của kiểm kê (nói thẳng).** Phiên này không đọc được repo SaaS (SMILE-PMS, PDAOrder: GitHub trả 403 "not enabled for this session"). Phía SaaS chỉ được suy ra từ những gì DevLibrary giữ làm bản sao hợp đồng: `ops-manifest.json` (bản sao byte của manifest v2 trong SMILE-PMS), hằng số lệnh trong `contracts/wire`, README module và ma trận parity RT-0260. Test `ops_manifest_drift` (cần `PMS_OPS_MANIFEST`) không chạy ở đây vì không có checkout SMILE-PMS. Mục "cần SaaS xác nhận" bên dưới là chỗ phải đối chiếu lại khi có quyền đọc repo SaaS.

## 1. PMS SaaS: op trong ops-manifest v2 so với handler

Nguồn: `crates/dl-agent-pms/assets/ops-manifest.json` (43 op: 39 có call, 4 không có call). Handler chung: `SqlExecutor::execute` (`crates/dl-agent-pms/src/exec/mod.rs:348`) chạy từng call của manifest (`bind_args` ràng giá trị, `attempt` chạy trong transaction); op ghi có `request_key` đi qua journal (`crates/dl-agent-pms/src/journal/exec.rs:160`). Op không có call bị từ chối `OP_REJECTED/in_process_only` (`exec/mod.rs:352`). `hello` chỉ báo các op có call (`README.md` của dl-agent-pms, mục Routing).

Tổng: 39 đủ, 4 thiếu.

| Op | Loại | DB | Call (proc / select) | Handler | Trạng thái |
|---|---|---|---|---|---|
| `fo.arrivals.expected@1` | read | SMILE_FO | `HSKPArrivalExpected` | `SqlExecutor::execute` | đủ |
| `fo.availability.chart@1` | read | SMILE_FO | `AvailabilityChartNew` | `SqlExecutor::execute` | đủ |
| `fo.availability.contractStatus@1` | read | SMILE_FO |  | `SqlExecutor::execute` | đủ |
| `fo.availability.source@1` | read | SMILE_FO |  | `SqlExecutor::execute` | đủ |
| `fo.availability.sourceV2@1` | read | SMILE_FO |  | `SqlExecutor::execute` | đủ |
| `fo.clerk.login@1` | write | SMILE_FO | `LogIn` | `SqlExecutor::execute` + journal | đủ |
| `fo.clerk.rights.get@1` | read | SMILE_FO | `UGetUserPermission` | `SqlExecutor::execute` | đủ |
| `fo.dashboard.forecast@1` | read | SMILE_FO |  | `SqlExecutor::execute` | đủ |
| `fo.dashboard.houseMix@1` | read | SMILE_FO |  | `SqlExecutor::execute` | đủ |
| `fo.dashboard.revenue@1` | read | SMILE_FO |  | `SqlExecutor::execute` | đủ |
| `fo.dashboard.rooms@1` | read | SMILE_FO |  | `SqlExecutor::execute` | đủ |
| `fo.dashboard.segments@1` | read | SMILE_FO |  | `SqlExecutor::execute` | đủ |
| `fo.dashboard.today@1` | read | SMILE_FO |  | `SqlExecutor::execute` | đủ |
| `fo.departures.expected@1` | read | SMILE_FO | `HSKPDepartureExpected` | `SqlExecutor::execute` | đủ |
| `fo.departures.onDate@1` | read | SMILE_FO |  | `SqlExecutor::execute` | đủ |
| `fo.folioLogDef.list@1` | read | SMILE_FO | — (không có call) | không (`in_process_only`) | **thiếu** |
| `fo.folioLogDef.sync@1` | write | SMILE_FO | — (không có call) | không (`in_process_only`) | **thiếu** |
| `fo.folioLogDef.update@1` | write | SMILE_FO | — (không có call) | không (`in_process_only`) | **thiếu** |
| `fo.hotelDate.get@1` | read | SMILE_FO | `GetHotelDate1` | `SqlExecutor::execute` | đủ |
| `fo.inhouse.list@1` | read | SMILE_FO | `HSKPCurrentOccupied` | `SqlExecutor::execute` | đủ |
| `fo.pos.post@1` | write | SMILE_FO | `SMILE_Fo_PostPosBatch` | `SqlExecutor::execute` + journal | đủ |
| `fo.reservation.list@1` | read | SMILE_FO | `rptReservMasterList` | `SqlExecutor::execute` | đủ |
| `fo.reservation.listFigures@1` | read | SMILE_FO |  | `SqlExecutor::execute` | đủ |
| `pos.cashier.byInitial@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |
| `pos.cashier.login@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |
| `pos.check.close@1` | write | SMILE_POS | `SMILE_Pos_CloseBill` | `SqlExecutor::execute` + journal | đủ |
| `pos.check.detail@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |
| `pos.check.list@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |
| `pos.check.open@1` | write | SMILE_POS | `SMILE_Pos_OpenCheck` | `SqlExecutor::execute` + journal | đủ |
| `pos.check.pay@1` | write | SMILE_POS | `SMILE_Pos_AddPayment` | `SqlExecutor::execute` + journal | đủ |
| `pos.check.postItem@1` | write | SMILE_POS | `SMILE_Pos_PostItem` | `SqlExecutor::execute` + journal | đủ |
| `pos.check.send@1` | write | SMILE_POS | `SMILE_Pos_SendOrder` | `SqlExecutor::execute` + journal | đủ |
| `pos.checkVersion.get@1` | read | — | — (không có call) | không (`in_process_only`) | **thiếu** |
| `pos.clerk.permissions@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |
| `pos.dashboard.revenue@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |
| `pos.menu.items@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |
| `pos.menu.tree@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |
| `pos.outbox.claim@1` | write | SMILE_POS | `SMILE_Pos_ClaimPost` | `SqlExecutor::execute` + journal | đủ |
| `pos.outbox.complete@1` | write | SMILE_POS | `SMILE_Pos_CompletePost` | `SqlExecutor::execute` + journal | đủ |
| `pos.outbox.source@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |
| `pos.outlets.list@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |
| `pos.shift.payments@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |
| `pos.tender.modes@1` | read | SMILE_POS |  | `SqlExecutor::execute` | đủ |

Ghi chú:

- `fo.availability.chart` (proc `AvailabilityChartNew`) lỗi SQL 8152 trên SR; `fo.availability.source`/`sourceV2` thay thế bằng select (RT-0474, RT-0614). SaaS phải dùng `source*` cho SR.
- Các op `fo.dashboard.*` tính ở cloud từ select thô (tối đa 10000 dòng mỗi call); dòng chạm `max_rows` bị cloud coi là có thể bị cắt.

## 2. PMS SaaS: lệnh hub cho module pms

`crates/dl-agent-pms/src/module/commands.rs` (`answer`): `pms.status`, `pms.reconnect`, `pms.selftest`, `pms.op_dry_run`. Đủ cho vận hành; SaaS không cần thêm.

## 3. PMS thiết bị (lock, POS device) qua `dl-agent-pms-device`

Cloud đẩy lệnh qua hàng đợi claim/result (`contracts/api.rs`). Loại lệnh có handler: `lock.issue`, `lock.duplicate`, `lock.read`, `lock.extend`, `lock.erase`, `pos.print`, `pos.kick`, `pos.display`, `pos.read`, `pos.speak`; lệnh hub: `pms-device.status`, `pms-device.flush_outbox`. Còn mở theo ma trận parity (mục 5): enrolment thay cho token đặt tay (gap 12), OPOS nay mở két trong khi .NET từ chối (cần chủ dự án quyết), chưa có bench phần cứng.

## 4. PDAOrder SaaS: lệnh site gọi xuống `dl-agent-pda-sync`

Điều phối ở `crates/dl-agent-pda-sync/src/commands/executor.rs:244` (`dispatch`); lệnh lạ trả `unknown_type`.

| Lệnh | Handler | Trạng thái |
|---|---|---|
| `kds.line_state` | `kds_line_state` | đủ |
| `reservation.save`, `reservation.cancel` | `reservation_save`, `reservation_cancel` | đủ |
| `sync.resnapshot` | `resnapshot` (nếu bật) | đủ |
| `sync.verify` | `verify` (nếu bật) | đủ |
| `legacy.export` | `legacy_export` (nếu bật) | đủ |
| `order.check_open`, `order.item_post`, `order.send`, `order.item_void`, `order.covers`, `order.combo.post` | `check_open`, `item_post`, `send`, `item_void`, `covers`, `combo_post` | đủ (chỉ chạy được trên proc giả lập, chưa bench SMILE_POS thật: gap 6) |
| `order.table_change`, `order.table_transfer`, `order.check_merge`, `order.items_move`, `order.check_cancel_empty` | `table_change`, `table_transfer`, `check_merge`, `items_move`, `check_cancel_empty` | đủ (cùng hạn chế gap 6) |
| In bếp từ `/hubs/order` qua TCP thô | `printrelay` | đủ |
| Loa nhân viên (`dl-agent-speaker`) | module speaker | đủ khi máy đã `pda-enroll`; máy chỉ có loa chưa chạy được (gap 7) |

Lệnh tắt/chưa nối: `legacy-archive` và `legacy-upload` nằm ngoài phạm vi theo kế hoạch (chủ dự án: out), không tính thiếu.

## 5. Lệnh còn thiếu: đầu vào, đầu ra, ưu tiên

Mã MS-n để theo dõi. Đầu vào/đầu ra của MS-1..4 chưa có trong manifest (call rỗng); phần dưới là đề xuất, phải khớp mã SaaS trước khi làm (RT con tiếp theo).

| Mã | Lệnh thiếu | Đầu vào | Đầu ra | Ưu tiên | Lý do ưu tiên |
|---|---|---|---|---|---|
| MS-1 | `fo.folioLogDef.list@1` (read, SMILE_FO) | `{}` (đề xuất: không tham số) | một set các dòng định nghĩa log folio của SMILE_FO (cột do SaaS chốt) | trung | Agent từ chối `in_process_only`. Nếu SaaS chạy trong cloud thì không cần làm; nếu SaaS gửi xuống agent SR thì mọi lần gọi hỏng. Cần SaaS xác nhận |
| MS-2 | `fo.folioLogDef.sync@1` (write, idempotent, SMILE_FO) | `request_key` + danh sách định nghĩa cần đồng bộ (đề xuất) | số dòng thêm/sửa, qua journal | trung | Như MS-1, thêm là op ghi nên phải có `request_key` và journal |
| MS-3 | `fo.folioLogDef.update@1` (write, idempotent, SMILE_FO) | `request_key` + khoá định nghĩa + giá trị mới (đề xuất) | dòng sau khi sửa / số dòng đổi | trung | Như MS-2 |
| MS-4 | `pos.checkVersion.get@1` (read, target PMS, không DB) | `{}` | phiên bản hợp đồng POS mà agent chạy (đề xuất: `{contract_max, ops}` lấy từ `hello`) | thấp | Selftest dùng op này và coi `rejected` cũng là đạt (`module/selftest.rs`); SaaS có thể đọc `hello` thay thế |

Các khoảng trống vận hành không phải op nhưng chặn "100% yêu cầu từ SaaS" trên SR (từ parity matrix RT-0260, trạng thái 2026-11-20, chưa kiểm lại sau đó):

| Mã | Khoảng trống | Ưu tiên |
|---|---|---|
| MG-1 | pda-sync: không đọc `appsettings.site.json` (`SiteTimeZone`, `PrintRelay`) khi chuyển → giờ POS lệch múi giờ (gap 5) | cao (dữ liệu) |
| MG-2 | pda-sync: lệnh tiền (order/combo/table) chưa chạy trên SMILE_POS thật (gap 6) | cao (tiền), cần bench SR |
| MG-3 | speaker: PDA cloud từ chối SyncAgent ở `JoinRvc`/`sound-mode`; máy chỉ có loa chưa enrol được (gap 7) | cao (vận hành), cần sửa phía PDA cloud |
| MG-4 | pms-site: chưa bench với cloud thật trên Windows/SQL 2008 và op ghi đầu tiên (gap 11) | trung |
| MG-5 | pms-device: enrolment, quyết định OPOS mở két (gap 12) | trung |
| MG-6 | Hiệu năng ngang IIS on-prem: chưa có số đo cho các op đọc nhiều select (`fo.dashboard.*`, `fo.availability.source*`) trên SR | trung, cần đo trước khi kết luận |

## 6. Việc tiếp theo đề xuất (không làm trong RT này)

1. Với quyền đọc SMILE-PMS: đối chiếu danh sách op SaaS thực gọi với 43 op trên; chốt MS-1..4 là "SaaS tự chạy" hay "xuống agent".
2. Chạy `PMS_OPS_MANIFEST=<file> cargo test -p dl-agent-pms --test ops_manifest_drift` để chắc manifest không lệch.
3. Với PDAOrder: đối chiếu `site_commands`/`order` với danh sách lệnh trong repo PDAOrder, và chạy MG-1 trước.

## Kiểm chứng

Đọc: `ops-manifest.json` (đếm 43 op bằng script, 39 có call), `exec/mod.rs:348-355`, `module/commands.rs`, `pda-sync/commands/executor.rs:244-275`, `pms-device/contracts/api.rs`, parity matrix. Không chạy test (RT chỉ thêm tài liệu).
