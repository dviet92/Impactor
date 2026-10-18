# 03 — Impactor bản Server (thay đổi phía client, Rust)

Làm trên branch `feat/account-server`. Nguyên tắc số một: **giữ diff với `main` càng nhỏ càng tốt**, vì `main` (bản hiện tại) vẫn tiếp tục được sửa và sẽ phải merge sang branch này nhiều lần. Code mới đặt trong file/crate mới; file cũ chỉ đổi ở đúng chỗ gọi.

## Bước 1 — Cho hai bản chạy song song trên cùng một máy

Làm đầu tiên, thành một commit riêng. Nếu bỏ qua bước này, hai bản sẽ phá nhau:

| Thứ | Hiện tại | Bản Server | Hỏng gì nếu để trùng |
|---|---|---|---|
| `APP_NAME` (`apps/plumeimpactor/src/main.rs`) | `"Impactor"` | `"Impactor Server"` | Là khoá single-instance (`relaunch::single_instance_key`) và tên auto-start: mở bản này thì bản kia bị coi là "đang chạy" và tự thoát; bật auto-start bên này ghi đè bên kia. |
| Thư mục dữ liệu (`defaults::get_data_path`) | `%APPDATA%\PlumeImpactor` | `%APPDATA%\PlumeImpactorServer` | Hai bản ghi đè `accounts.json`, log, file cổng relaunch của nhau. |
| `Package::STAGE_PREFIX` (`crates/plume_utils/src/package.rs`) | `"plume_stage_"` | `"plumesrv_stage_"` | **Nặng nhất**: lúc khởi động, `clean_orphan_stages()` xoá mọi thư mục `%TEMP%\plume_stage_*`. Mở bản này lúc bản kia đang cài → xoá file đang ký của bản kia. |
| Tên file exe (`apps/plumeimpactor/Cargo.toml`) | `plumeimpactor.exe` | `[[bin]] name = "plumeimpactor-server"` | Để hai exe nằm chung một thư mục được. |
| Tiêu đề cửa sổ, `notify_rust::set_application` | | thêm chữ "Server" | Phân biệt bằng mắt. |

Gom các giá trị này vào một module `apps/plumeimpactor/src/edition.rs` (ví dụ `pub const DATA_DIR_NAME`, `pub const APP_NAME`…) để khi merge `main` chỉ có một chỗ khác nhau.

## Bước 2 — Crate mới `crates/plume_remote`

Toàn bộ phần nói chuyện với server nằm ở đây.

```
crates/plume_remote/src/
  lib.rs          # pub use
  config.rs       # ServerConfig { url, machine_key, server_cert_pem } — đọc/ghi server.json
  client.rs       # ServerClient: health, snapshot, plans, leases, sign-ins, install-results, import…
  dto.rs          # struct serde (camelCase) khớp với API trong 02-server.md
  lease.rs        # LeaseGuard: heartbeat nền + thư mục cache bí mật + dọn dẹp khi drop
  import.rs       # đọc dữ liệu bản cũ (%APPDATA%\PlumeImpactor) → payload /import
```

### Kết nối HTTPS

- Client riêng cho server, **không bao giờ đi qua proxy Apple**: `reqwest::Client::builder().no_proxy()`. Proxy trong tab Connection chỉ dành cho Apple — cùng bài học với việc ký không đi qua proxy (`developer_client()` trong `crates/plume_core/src/lib.rs`).
- Chỉ tin đúng certificate của server: người dùng bấm "Nhập certificate của server" (file `.cer` do `AccountServer.exe export-cert` xuất), lưu PEM vào `server.json`, rồi dựng client chỉ tin certificate đó làm root (tắt root mặc định). Kiểm tra lại tên hàm TLS trong reqwest **0.13** (đổi tên so với 0.12), và nhớ trên Windows workspace đang bật `native-tls`.
- Timeout: connect 5 giây, request 30 giây. Lỗi mạng → báo rõ "không kết nối được server tài khoản", không thử lại vô tận.
- Gửi `Authorization: Bearer <machine_key>` và `X-Client-Version: <CARGO_PKG_VERSION>`.
- `server.json` chứa khoá máy — nhạy cảm như mật khẩu. Nên mã hoá bằng DPAPI của Windows (`CryptProtectData`, phạm vi user) nếu làm được gọn.

### Bí mật trong lúc có lease

`LeaseGuard` (theo kiểu RAII):

1. Tạo `%LOCALAPPDATA%\PlumeImpactorServer\lease-cache\<leaseId>\`.
2. Ghi `anisette\state.plist` (nếu server có) và `keys\<teamId>\key.pem` (nếu server có) — đúng bố cục mà omnisette và `CertificateIdentity::new_with_session` đang đọc.
3. Ghi nhớ SHA-256 của `state.plist` lúc tải.
4. Chạy heartbeat mỗi 60 giây.
5. Khi xong: nếu `state.plist` đổi hash → `PUT /identity-groups/{n}/anisette`; nếu `CertificateIdentity.new == true` → `PUT /teams/{team}/key`. Rồi `complete` và **xoá thư mục**.
6. Lúc khởi động app: xoá mọi thư mục còn sót trong `lease-cache\` (app chết giữa chừng).

Hai lane cùng identity group trên một máy mỗi cái có bản sao riêng, nên không tranh nhau file; `identity_group_lock` hiện có trong `subscriptions.rs` vẫn giữ lại được, không hại gì.

## Bước 3 — Đổi chỗ gọi

`AccountStore` vẫn dùng, nhưng **chỉ cho phần riêng của máy**: `network`, `refreshes`, `selected_account` (khi tắt Auto-pick), `auto_select_account`, `locale`. Mọi thứ về tài khoản đọc từ `RemoteSnapshot` (lấy từ `GET /snapshot`) và ghi qua `ServerClient`.

| Chỗ hiện tại | Đổi thành |
|---|---|
| `subscriptions.rs` · `run_install_batch_grouped` → `plan_groups(store, …)` | `server.plan(devices, mode, exclude)`. `groups` → lane như cũ; `waiting` → nằm ở mục "Đang chờ Apple ID" trên màn progress, hết lượt thì xin kế hoạch lại sau `retryAfterSeconds`; `unplanned` → `record_failures` + gửi `install-results`. |
| `install_group` → `DeveloperSession::new(account.adsid(), account.xcode_gs_token(), anisette path)` | lấy từ `LeaseGuard` (bí mật + đường dẫn cache). `CertificateIdentity::new_with_session(…, config_path = lease dir, …)`. |
| `install_group` → `store.record_device_account_sync` (sau `sign_bundle`) | `server.signed(lease, udids)` — **một lần**, đúng chỗ đó (chỗ bị tính từ lúc có chữ ký). |
| `record_install_result_sync` (mỗi máy) | `server.install_results([...])` ngay sau từng máy. |
| `AccountProblem::Full / SessionExpired` | `server.refuse(lease, problem, message)` rồi xin kế hoạch lại với `excludeAccountIds`. |
| `remember_for_refresh` | Giữ nguyên ở máy (`refresh_store` + `AccountStore.refreshes`). |
| `run_install_batch_serial` / `run_installation` (1 máy, Mac, Export) | Xin kế hoạch 1 máy (hoặc `POST /plans {purpose:"export", devices:[]}` cho Export: server chọn tài khoản theo luật (2), không giữ chỗ thiết bị). |
| `export_certificate`, `fetch_teams` | `POST /leases {purpose:"export"/"teams"}`. |
| `refresh.rs` | `POST /leases {purpose:"refresh", accountId, udids:[udid]}`. |
| `screen/windows/login_window.rs` (đăng nhập + 2FA) | Trước khi đăng nhập: `server.begin_sign_in(email)` → ghi `state.plist` của group vào `sign-in-cache\<id>\anisette\` → đăng nhập với đường dẫn đó. Thành công: lấy team như `accounts_add_from_session` đang làm, rồi `server.complete_sign_in(...)`; upload `state.plist` nếu vừa provision. Lỗi/huỷ: `server.cancel_sign_in`. |
| Bulk add (`BulkAddQueue` trong `screen/mod.rs`) | Như trên cho từng tài khoản. `sign_in_gap` / `group_provisioning_busy` từ server → đợi `retryAfterSeconds` (server là trọng tài khoảng nghỉ cho cả LAN). |
| `settings.rs` · danh sách tài khoản, `(0/3)`, `(id 1:3/5)`, `(marked)`, màu xanh/đỏ, ngày cài gần nhất | Đọc từ `RemoteSnapshot` (server tính sẵn `slotsLeft`, `restUntil`, `needsSignIn`, `busyByMachine`). Thêm: dòng đỏ "cần đăng nhập lại", chữ "đang dùng bởi PC-2". |
| Remove / Mark full / Unmark / chọn team | `DELETE /accounts/{id}`, `POST`/`DELETE /accounts/{id}/rest`, `PATCH /accounts/{id}`. |
| Ô "Group Apple IDs into shared device identities" | **Bỏ** — bản Server luôn dùng identity group. |
| Ô "Max Apple IDs per identity" | → `PUT /settings` (chỉ bật khi khoá máy là admin). |
| `screen/mod.rs` · Multi Install: màu, "Lỗi lần trước", cột Apple ID dự kiến (`project_device_accounts`) | `RemoteSnapshot.installStatuses` + `POST /plans {dryRun:true}` (debounce ~300 ms khi đổi lựa chọn). |
| `defaults::get_anisette_path(group)` | Không dùng cho tài khoản nữa; đường dẫn đến từ `LeaseGuard` / sign-in cache. |

Snapshot: tải lại mỗi 5 giây khi đang mở Settings hoặc Multi Install (dùng `ETag`, trên LAN gần như không tốn gì), và ngay sau mỗi thao tác ghi.

## Bước 4 — Tab Connection: khối "Account server"

- URL (`https://accountserver:8443`), nút "Nhập certificate của server", ô khoá máy, nút **Test** (gọi `/health` + `/snapshot`, hiện số tài khoản và độ lệch giờ so với server).
- Nút **"Đẩy tài khoản từ bản cũ lên server"**: đọc `%APPDATA%\PlumeImpactor\` (chỉ đọc) — `accounts.json`, `state.plist` ở gốc (identity của máy), `anisette\group-*\state.plist`, `keys\*\key.pem` — gửi `/import` với `dryRun:true`, hiện tóm tắt (bao nhiêu tài khoản mới / bị thay / bỏ qua, bao nhiêu identity, khoá team), người dùng xác nhận thì gửi thật.

## Khi server không với tới được

Chặn ký/cài/đăng nhập, hiện lỗi rõ. Không dùng dữ liệu cũ trong máy (xem `01-architecture.md`).

## Kiểm tra trước khi giao

- `cargo test -p plume_store` vẫn xanh (không phá luật cũ — `main` còn dùng).
- `cargo test -p plume_remote`: DTO khớp JSON mẫu trong `02-server.md`; `LeaseGuard` xoá thư mục khi drop, kể cả khi lỗi; import đọc được một thư mục `%APPDATA%\PlumeImpactor` mẫu (tạo fixture giả, **không** dùng dữ liệu thật).
- `cargo check -p plumeimpactor` sạch.
- Không dòng log nào chứa token, `state.plist`, khoá PEM, khoá máy; email luôn rút gọn.
