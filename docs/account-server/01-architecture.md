# 01 — Kiến trúc: server tài khoản dùng chung cho nhiều máy

## Mục tiêu

Nhiều máy tính Windows trong cùng mạng LAN dùng **chung một kho Apple ID**. Không phải chia tài khoản cho từng máy, không phải copy `accounts.json` qua lại. Máy nào cũng ký và cài được bằng bất kỳ tài khoản nào trong kho. Mọi thay đổi — đăng nhập mới, đăng ký thiết bị, cài thành công hay lỗi, đánh dấu đầy — đều ghi lên server ngay, nên mọi máy luôn thấy cùng một trạng thái.

Ngoài phạm vi: server **không** tự ký app và **không** nói chuyện với iPhone. Việc ký, cắm USB, cài và refresh vẫn làm trên từng máy y như bây giờ. Server là **sổ cái + két sắt + người điều phối**.

## Ba thành phần

```
┌────────────── LAN ──────────────────────────────────────────────┐
│                                                                 │
│  PC-1 (Impactor bản Server)   PC-2 …          PC-n …            │
│   ├─ ký app (plume_core)       (giống PC-1)                      │
│   ├─ USB → iPhone                                               │
│   └─ RemoteStore ──HTTPS──┐                                      │
│                           ▼                                     │
│                 Account Server (C#, Windows Server 2019)        │
│                  ├─ API  /api/v1/...                            │
│                  ├─ Planner (luật 3 máy/7 ngày, identity 5/24h)  │
│                  ├─ Lease + Reservation (chống hai máy giành nhau)│
│                  └─ DB (EF Core code-first) + secrets mã hoá     │
└─────────────────────────────────────────────────────────────────┘
          Mọi request lên Apple vẫn đi từ PC (qua proxy nếu PC đặt proxy).
```

1. **Account Server** (mới, C# .NET 10, chạy trên máy Windows Server 2019 trong LAN) — giữ dữ liệu dùng chung, cấp "lease" để một máy được dùng một tài khoản trong lúc ký.
2. **Impactor bản Server** (mới, là branch `feat/account-server` của repo hiện tại) — giống hệt bản đang dùng, chỉ khác chỗ lấy và lưu tài khoản: hỏi server thay vì đọc `accounts.json`.
3. **Impactor bản hiện tại** (giữ nguyên trên `main`) — không đổi gì, chạy song song được trên cùng máy.

## Dữ liệu nào nằm ở đâu

Lấy từ code hiện tại (`crates/plume_store/src/store.rs`, `gsa_account.rs`, `apps/plumeimpactor/src/defaults.rs`, `crates/plume_core/src/utils/certificate.rs`). Hiện mọi thứ dưới đây nằm trong `%APPDATA%\PlumeImpactor\` của từng máy.

### Lên server (dùng chung)

| Dữ liệu | Ở bản hiện tại | Vì sao phải dùng chung |
|---|---|---|
| Tài khoản: `email`, `first_name`, **`adsid`**, **`xcode_gs_token`**, `team_id`, `added_at`, `identity_group`, `last_install_at` | `accounts.json` → `accounts` | Máy nào cũng phải ký được bằng tài khoản đó. **In đậm = bí mật**, mã hoá trên server. |
| Thiết bị đã gán tài khoản: `udid → (email, installed_at)` | `accounts.json` → `device_accounts` | Luật **3 máy / 7 ngày** chỉ đúng khi mọi máy đếm cùng một sổ. |
| Kết quả cài gần nhất: `udid → (ok, at, error)` | `accounts.json` → `install_status` | Màu xanh/đỏ và dòng "Lỗi lần trước" trong Multi Install phải giống nhau trên mọi máy. |
| Đánh dấu đầy (nghỉ 7 ngày) | đang là UDID giả `manual-full:<email>:0..2` trong `device_accounts` | Trên server làm thành trường thật `RestUntil` — gọn hơn. |
| **Trạng thái anisette của từng identity group** (`state.plist`) | `anisette\group-N\state.plist`, và `state.plist` ở gốc cho "identity của máy" | Apple ràng token với dấu vân tay thiết bị trong file này. Máy khác dùng token mà không có đúng file này → Apple từ chối ("needs signing in again"). |
| **Khoá riêng certificate theo team** (`keys\<team_id>\key.pem`) | `keys\<team>\key.pem` | **Bắt buộc dùng chung**, xem mục "Certificate" bên dưới. |
| Luật chung: số máy tối đa/ID (3), cửa sổ (7 ngày), số Apple ID/identity (5), cửa sổ identity (24h) | hằng số + `identity_group_size` | Một luật cho mọi máy. |

### Ở lại từng máy

| Dữ liệu | Lý do |
|---|---|
| `network`: proxy Apple, khoảng nghỉ bulk add, thời gian chờ 429, số luồng cài | Mỗi máy có mạng/proxy riêng. **Proxy này chỉ dành cho Apple, không được áp lên kết nối tới server.** |
| Cấu hình kết nối server: URL, khoá máy (API key), vân tay certificate HTTPS của server | Riêng từng máy. |
| `refresh_store\` (bản copy app để refresh, rất nặng) + lịch refresh | Refresh phải chạy trên máy đang cắm iPhone. |
| Log, `crash.log`, thư mục stage tạm | Chẩn đoán từng máy. |
| Tài khoản "đang tích" khi tắt Auto-pick | Lựa chọn giao diện của người ngồi máy đó. |
| **Bản sao tạm của bí mật** (anisette, key) — chỉ trong lúc có lease | omnisette và `CertificateIdentity` đọc file theo đường dẫn, nên phải ghi ra đĩa; xoá ngay khi xong. |

## Ba quy tắc nền tảng (không được phá)

### 1. Token đi cùng identity, không bao giờ tách

`xcode_gs_token` chỉ dùng được với đúng `state.plist` đã sinh ra nó. Hệ quả:

- Trong bản Server, **mọi tài khoản đều thuộc một identity group trên server**. Không còn "identity của máy" (cái `state.plist` ở gốc thư mục), vì identity của PC-1 không có trên PC-2. Ô "Group Apple IDs into shared device identities" trong Settings bỏ đi — luôn bật.
- Đăng nhập lại **giữ nguyên** identity group (luật hiện tại, xem `claude/anisette-group-identity.md` trong project). Chỉ xoá tài khoản rồi thêm lại mới được xếp group mới.
- Khi import từ máy cũ: token và identity của nó luôn đi thành cặp. Tài khoản cũ dùng "identity của máy" thì identity đó lên server thành một group riêng (đánh dấu nguồn `machine:<tên máy>`).

### 2. Một team chỉ có một khoá certificate dùng chung

Hiện tại `CertificateIdentity::new_with_session` tìm `keys\<team>\key.pem`. Nếu không có, nó xin certificate mới. Tài khoản free chỉ được rất ít certificate, nên khi Apple trả lỗi 7460 ("quá nhiều certificate"), code **revoke** một certificate cũ để xin cái mới.

Nếu mỗi máy giữ khoá riêng: PC-2 ký bằng tài khoản A → không có khoá → revoke certificate mà PC-1 đang dùng → các app PC-1 đã cài bằng certificate đó có thể ngừng mở được, và lần sau PC-1 ký lại revoke của PC-2. Hai máy sẽ thay nhau giết app của nhau.

Vì vậy: **server giữ khoá cho từng team**. Máy nào ký thì tải khoá về; nếu máy đó buộc phải tạo certificate mới (`identity.new == true`) thì đẩy khoá mới lên ngay, thành khoá đang dùng của team.

### 3. Mỗi lúc chỉ một máy dùng một tài khoản

Server cấp **lease** theo tài khoản: trong lúc PC-1 đang ký bằng A, không máy nào khác lấy được lease của A. Lý do:

- Hai máy cùng ký một tài khoản cùng lúc sẽ tranh nhau certificate, App ID, profile.
- Luật 3 máy/7 ngày bị vượt: PC-1 và PC-2 cùng thấy A "còn 1 chỗ", cùng ký → A thành 4 máy.

Lease có thời hạn (mặc định 20 phút, gia hạn bằng heartbeat). Máy chết giữa chừng → lease tự hết hạn, chỗ giữ được trả lại.

## Luồng 1 — Multi Install (luồng chính)

Tương ứng `run_install_batch_grouped` hiện tại (mỗi Apple ID một luồng, ký 1 lần, cài lần lượt các máy của ID đó; các ID khác nhau chạy song song — xem `claude/progress-theo-tung-may.md`).

1. **Xin kế hoạch.** PC gửi danh sách thiết bị đã tích (`udid`, `name`) → `POST /api/v1/plans`. Server chạy planner (chép đúng luật của `plan_device_accounts` + `auto_account_for_device`, xem `02-server.md`), trong một khoá ghi duy nhất của server:
   - tạo một **Lease** cho mỗi Apple ID được chọn;
   - tạo **Reservation** (giữ chỗ, trạng thái `Pending`) cho từng UDID — chỗ giữ được tính vào số máy của tài khoản ngay lúc đó, nên máy khác xin kế hoạch cùng lúc sẽ không thấy chỗ trống ảo;
   - trả về: các nhóm `{leaseId, account, udids}`, các máy **chờ** (tài khoản của chúng đang bị máy khác giữ — kèm `retryAfterSeconds`), và các máy **không xếp được** (kèm lý do).
2. **Lấy bí mật.** Với mỗi lease: `GET /api/v1/leases/{id}/secrets` → `adsid`, `xcodeGsToken`, `teamId`, `state.plist` của identity group (kèm `version`), khoá team (kèm `version`, có thể không có). PC ghi ra thư mục cache của lease rồi chạy **đúng code ký hiện tại**, chỉ đổi đường dẫn anisette/keys sang thư mục cache.
3. **Heartbeat** mỗi 60 giây trong lúc làm: `POST /leases/{id}/heartbeat`.
4. **Ký xong** (trước khi cài): `POST /leases/{id}/signed {udids}` → server đổi các Reservation thành **DeviceAssignment** với `installedAt = giờ server`, cập nhật `lastInstallAt`. Đây đúng là thời điểm bản hiện tại gọi `record_device_account_sync`: chỗ bị tính từ lúc chữ ký tồn tại, dù cài sau đó có lỗi.
   - Nếu `state.plist` trong cache đã đổi (hash khác lúc tải) → `PUT /identity-groups/{n}/anisette` kèm `version` cũ.
   - Nếu phải tạo certificate mới → `PUT /teams/{teamId}/key` kèm `version` cũ.
5. **Từng máy cài xong** (thành công hay lỗi): `POST /api/v1/install-results` — gửi ngay sau mỗi máy, không đợi cuối lượt, như bản hiện tại.
6. **Kết thúc lease**: `POST /leases/{id}/complete`. PC xoá thư mục cache của lease (bí mật không nằm lại trên đĩa).
7. **Tài khoản hỏng giữa chừng** (Apple báo đầy, hoặc session hết hạn — đúng `AccountProblem::Full` / `SessionExpired` hiện tại): `POST /leases/{id}/refuse {problem, message}` → server trả các chỗ giữ, đánh dấu tài khoản (đầy → nghỉ `FullCooldownHours`; hết phiên → `NeedsSignIn`, mọi máy thấy dòng đỏ "cần đăng nhập lại"). PC xin kế hoạch lại cho các máy đó, kèm danh sách loại trừ.

Kế hoạch **xem trước** cho ô Multi Install (cột "máy này sẽ dùng Apple ID nào", thay `project_device_accounts`): cùng endpoint với `dryRun: true` — server tính nhưng không giữ chỗ.

## Luồng 2 — Thêm Apple ID (đăng nhập + OTP trên tool)

Tương ứng `login_window.rs` + bulk add hiện tại.

1. `POST /api/v1/sign-ins {email}` → server:
   - email đã có → trả **identity group cũ** (luật "đăng nhập lại giữ identity");
   - email mới → chọn group thấp nhất còn < N lượt đăng nhập trong 24h (đúng `identity_group_for_sign_in_at`), và **giữ một lượt** trong group đó (SignInReservation, hết hạn 10 phút) để hai máy thêm tài khoản cùng lúc không nhét 6 cái vào group 5;
   - group chưa có `state.plist` → cấp **khoá provisioning** cho máy này; máy khác xin vào cùng group lúc đó nhận `retryAfterSeconds`;
   - áp **khoảng nghỉ tối thiểu giữa hai lần đăng nhập trên toàn LAN** (`MinSignInGapSeconds`, mặc định 45s). Mọi máy trong LAN ra Internet bằng cùng một IP, và Apple trả 429 theo mạng — server là chỗ duy nhất thấy được mọi máy để giãn ra.
2. PC ghi `state.plist` (nếu có) vào cache, chạy login hiện tại (SRP + cửa sổ 2FA). Nếu omnisette vừa provision identity mới → `PUT /identity-groups/{n}/anisette`.
3. `POST /sign-ins/{id}/complete {firstName, adsid, xcodeGsToken, teamId}` → server upsert tài khoản (so email không phân biệt hoa/thường; giữ `lastInstallAt` và các thiết bị đã gán; `addedAt/lastSignInAt = giờ server`; xoá cờ `NeedsSignIn`).
4. Hỏng giữa chừng → `POST /sign-ins/{id}/cancel` (trả lượt; đăng nhập hỏng không tính vào 5/24h — giống bản hiện tại chỉ đếm lượt thành công).

## Luồng 3 — Đánh dấu đầy, xoá, chọn team, chỉnh luật

| Thao tác | Endpoint | Ghi chú |
|---|---|---|
| Đánh dấu đầy | `POST /accounts/{id}/rest` | `RestUntil = now + WindowDays`. Bấm lại thì tính lại từ đầu. |
| Bỏ đánh dấu | `DELETE /accounts/{id}/rest` | Không đụng thiết bị thật. |
| Xoá tài khoản | `DELETE /accounts/{id}` | Xoá luôn các DeviceAssignment của nó (giống `forget_account`). Thêm lại = được xếp identity mới. |
| Đổi team | `PATCH /accounts/{id} {teamId}` | |
| Luật chung | `GET/PUT /settings` | Chỉ máy có quyền admin. |

## Luồng 4 — Refresh, Export P12, Fetch teams

Đều cần bí mật, nên đều đi qua lease:

- **Refresh** (`refresh.rs`): xin lease cho đúng tài khoản ghi trong `RefreshDevice.account`, với chính UDID đó. Nếu UDID đã gán cho tài khoản đó trong 7 ngày thì không tốn thêm chỗ. Bản copy app vẫn nằm ở máy đó.
- **Export P12 / Fetch teams**: lease không kèm thiết bị (`purpose: "export"` / `"teams"`).

## Khi server không với tới được

Không ký, không cài. Hiện thông báo rõ ("Không kết nối được server tài khoản …"). **Không** quay về dùng dữ liệu cũ trong máy: số đếm cũ sẽ làm vượt luật 3 máy, còn bí mật cũ có thể đã bị máy khác thay (certificate mới, identity mới).

## Vì sao không làm "server ký hộ"?

Có một kiểu khác: server tự gọi Apple và tự ký, các PC chỉ nhận file đã ký về cắm USB. Ưu điểm: bí mật không rời server. Nhưng nó gần như phải viết lại toàn bộ phần ký (đang là Rust) sang server, phải chuyển file .ipa qua lại, và việc đăng nhập + nhập OTP sẽ phải nằm trên trang web của server. Kiểu đang chọn (máy tự ký, server giữ sổ + két) dùng lại được gần như toàn bộ code đang chạy tốt. Có thể tính lại sau nếu cần.
