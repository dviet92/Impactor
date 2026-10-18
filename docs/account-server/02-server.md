# 02 — Account Server (C#)

## Nền tảng

| Hạng mục | Chọn | Ghi chú |
|---|---|---|
| Runtime | **.NET 10 (LTS)** | Hỗ trợ tới **14/11/2028**, chạy được trên Windows Server 2019 x64. **Không bắt đầu bằng .NET 8/9**: cả hai hết hỗ trợ ngày 10/11/2026. Nếu "C# framework" nghĩa là .NET Framework 4.8 thì không dùng được ASP.NET Core / EF Core hiện đại; chọn .NET 10. |
| Web | ASP.NET Core (Kestrel), Minimal API hoặc Controllers | Chạy như **Windows Service** (`Microsoft.Extensions.Hosting.WindowsServices`, `builder.Services.AddWindowsService()`), không cần IIS. |
| Database | **EF Core 10 code-first + migrations**, mặc định **SQLite** (`C:\ProgramData\ImpactorAccountServer\accounts.db`) | Vài máy trong LAN, một tiến trình server → SQLite + khoá ghi trong tiến trình là đủ và đúng. Muốn SSMS/backup kiểu SQL Server thì đổi provider sang SQL Server Express — code giữ nguyên, chỉ đổi `UseSqlite` → `UseSqlServer` và tạo lại migration. |
| Test | xUnit + `WebApplicationFactory` | Phần luật (Core) không phụ thuộc ASP.NET/Windows để chạy test được trên Linux (phiên Code chạy Linux). |
| Tài liệu API | `Microsoft.AspNetCore.OpenApi` (có sẵn từ .NET 9) | Xuất `openapi.json` vào `docs/account-server/openapi.json` để client Rust đối chiếu. |

## Bố cục solution

```
server/
  ImpactorAccountServer.sln
  src/
    AccountServer.Core/      # luật thuần: Planner, IdentityPlacer, Policy, các record kết quả — KHÔNG phụ thuộc EF/ASP.NET
    AccountServer.Data/      # DbContext, entity config, value converter mã hoá, migrations
    AccountServer/           # host: endpoints, auth, Windows service, background sweeper, lệnh quản trị (add-machine, cert-fingerprint…)
  tests/
    AccountServer.Core.Tests/   # port toàn bộ test luật từ crates/plume_store/src/store.rs
    AccountServer.Api.Tests/    # tích hợp: WebApplicationFactory + SQLite file tạm; test tranh chấp
```

`.gitignore` thêm: `server/**/bin/`, `server/**/obj/`, `*.db`, `*.db-wal`, `*.db-shm`, `appsettings.Production.json`, `*.pfx`.

## Thời gian và định dạng

- **Đồng hồ duy nhất là giờ UTC của server.** Mọi mốc `installedAt`, `addedAt`, `restUntil`… do server đóng dấu, không tin giờ của PC (các PC có thể lệch vài phút, mà luật 7 ngày / 24 giờ tính theo mốc thời gian).
- Lưu `DateTime` Kind=Utc (value converter ép Utc khi đọc). **Không dùng `DateTimeOffset` với SQLite**: provider SQLite của EF Core không dịch được `ORDER BY`/so sánh trên `DateTimeOffset`.
- JSON camelCase, thời gian ISO-8601 có `Z`. Lỗi trả `application/problem+json` có thêm trường `code` (ví dụ `account_busy`, `no_capacity`, `lease_expired`, `stale_version`, `sign_in_gap`, `group_provisioning_busy`, `client_too_old`).
- Mọi route dưới `/api/v1`. Client gửi `X-Client-Version`; server có thể từ chối bản quá cũ (`client_too_old`).

## Entity (code-first)

Tên thuộc tính tiếng Anh. `[Secret]` = cột mã hoá bằng Data Protection qua value converter.

```csharp
public class Machine {
    public Guid Id { get; set; }
    public string Name { get; set; } = "";          // "PC-1"
    public string ApiKeySha256 { get; set; } = "";  // chỉ lưu hash
    public bool IsAdmin { get; set; }
    public bool Revoked { get; set; }
    public DateTime CreatedAt { get; set; }
    public DateTime? LastSeenAt { get; set; }
}

public class AppleAccount {
    public Guid Id { get; set; }
    public string Email { get; set; } = "";              // cách viết người dùng nhập
    public string EmailNormalized { get; set; } = "";    // lower-invariant, UNIQUE
    public string FirstName { get; set; } = "";
    [Secret] public string Adsid { get; set; } = "";
    [Secret] public string XcodeGsToken { get; set; } = "";
    public string TeamId { get; set; } = "";
    public DateTime? AddedAt { get; set; }        // = lần đăng nhập thành công gần nhất (đúng nghĩa added_at hiện tại)
    public DateTime? LastInstallAt { get; set; }  // chỉ tiến, không lùi
    public Guid IdentityGroupId { get; set; }     // BẮT BUỘC trong bản Server
    public IdentityGroup IdentityGroup { get; set; } = null!;
    public DateTime? RestUntil { get; set; }      // đánh dấu đầy (tay) hoặc Apple báo đầy
    public string? RestReason { get; set; }       // "manual" | "apple_full: …"
    public bool NeedsSignIn { get; set; }
    public string? NeedsSignInReason { get; set; }
    public DateTime? NeedsSignInAt { get; set; }
    public int Version { get; set; }              // [ConcurrencyCheck] — SQLite không có rowversion
}

public class IdentityGroup {
    public Guid Id { get; set; }
    public int Number { get; set; }               // UNIQUE, 0-based; giao diện hiện Number+1 như "(id 1:3/5)"
    [Secret] public byte[]? AnisetteState { get; set; }   // nội dung state.plist; null = chưa provision
    public string? StateSha256 { get; set; }
    public int StateVersion { get; set; }
    public DateTime? ProvisionedAt { get; set; }
    public string Origin { get; set; } = "server";        // "server" | "machine:PC-1" | "import:PC-1:group-0"
    public Guid? ProvisioningLockMachineId { get; set; }
    public DateTime? ProvisioningLockExpiresAt { get; set; }
}

public class TeamKey {
    public Guid Id { get; set; }
    public string TeamId { get; set; } = "";
    [Secret] public string PrivateKeyPem { get; set; } = "";
    public string PublicKeySha256 { get; set; } = "";     // để dedupe khi import
    public bool IsActive { get; set; }                    // mỗi TeamId đúng 1 bản active
    public int Version { get; set; }                      // tăng theo TeamId
    public DateTime UploadedAt { get; set; }
    public Guid UploadedByMachineId { get; set; }
}

public class DeviceAssignment {          // = device_accounts hiện tại
    public string Udid { get; set; } = "";               // PK
    public Guid AccountId { get; set; }
    public DateTime InstalledAt { get; set; }            // giờ server lúc KÝ
    public Guid MachineId { get; set; }
}

public class Device {                    // danh bạ để hiển thị
    public string Udid { get; set; } = "";               // PK
    public string Name { get; set; } = "";
    public Guid? LastMachineId { get; set; }
    public DateTime LastSeenAt { get; set; }
}

public class InstallStatus {             // = install_status hiện tại
    public string Udid { get; set; } = "";               // PK
    public bool Ok { get; set; }
    public DateTime At { get; set; }
    public string? Error { get; set; }                   // 1 dòng, ≤ 300 ký tự (đúng shorten_install_error)
    public Guid MachineId { get; set; }
}

public class Lease {
    public Guid Id { get; set; }
    public Guid AccountId { get; set; }
    public Guid MachineId { get; set; }
    public string Purpose { get; set; } = "install";     // install | refresh | export | teams
    public LeaseState State { get; set; }                // Active, Signed, Completed, Refused, Expired
    public DateTime CreatedAt { get; set; }
    public DateTime ExpiresAt { get; set; }
}

public class Reservation {
    public Guid Id { get; set; }
    public Guid LeaseId { get; set; }
    public Guid AccountId { get; set; }
    public string Udid { get; set; } = "";
    public ReservationState State { get; set; }          // Pending, Committed, Released
}

public class SignInReservation {
    public Guid Id { get; set; }
    public string EmailNormalized { get; set; } = "";
    public Guid IdentityGroupId { get; set; }
    public Guid MachineId { get; set; }
    public SignInState State { get; set; }               // Pending, Completed, Cancelled, Expired
    public DateTime CreatedAt { get; set; }
    public DateTime ExpiresAt { get; set; }              // 10 phút
}

public class PolicySettings {            // 1 dòng, Id = 1
    public int Id { get; set; } = 1;
    public int MaxDevicesPerAccount { get; set; } = 3;   // AUTO_ACCOUNT_MAX_DEVICES
    public int DeviceWindowDays { get; set; } = 7;       // AUTO_ACCOUNT_WINDOW_DAYS
    public int IdentityGroupSize { get; set; } = 5;      // kẹp 1..100 như identity_group_size()
    public int IdentityWindowHours { get; set; } = 24;
    public int FullCooldownHours { get; set; } = 24;     // Apple báo đầy → nghỉ bao lâu (MỚI)
    public int MinSignInGapSeconds { get; set; } = 45;   // giãn đăng nhập trên toàn LAN (MỚI)
    public int LeaseMinutes { get; set; } = 20;
    public DateTime? LastSignInStartedAt { get; set; }
}

public class AuditEvent {
    public long Id { get; set; }
    public DateTime At { get; set; }
    public Guid? MachineId { get; set; }
    public string Action { get; set; } = "";             // "plan", "signed", "refuse:sessionExpired", "rest", "delete", "import"…
    public string? Subject { get; set; }                 // email ĐÃ RÚT GỌN
    public string? Detail { get; set; }
}
```

## Luật (AccountServer.Core) — chép đúng bản Rust

Nguồn chuẩn là `crates/plume_store/src/store.rs` ở branch này. **Đọc kỹ doc comment trong đó trước khi viết**, vì các comment ấy giải thích lý do của từng luật. Các hàm cần port:

| Rust | C# | Tóm tắt |
|---|---|---|
| `devices_installed_since` / `remaining_device_slots` | `Policy.SlotsUsed(account, now)` | Đếm `DeviceAssignment` của tài khoản có `InstalledAt > now − DeviceWindowDays`, **cộng** các `Reservation` Pending của lease còn hạn (bỏ qua UDID đã được tính). |
| `auto_account_for_device` | `Planner.PickForDevice` | Thứ tự: (1) thiết bị đã gán tài khoản trong cửa sổ → giữ tài khoản đó; (2) ứng viên còn chỗ (`SlotsUsed < Max`), sắp `AddedAt` giảm dần (null xếp cuối), rồi `SlotsUsed` tăng dần, rồi email (ordinal); (3) không còn → không xếp được. Tắt auto-pick → chỉ tài khoản được chọn. |
| `plan_device_accounts` | `Planner.Plan` | Đi lần lượt từng thiết bị theo thứ tự người dùng tích, mỗi lần chọn thì ghi "giả" vào bản mô phỏng để máy sau thấy tài khoản đầy thêm một. Gom nhóm theo tài khoản, giữ thứ tự xuất hiện. |
| `identity_group_for_sign_in_at` | `IdentityPlacer.Place` | Tài khoản đã có group → giữ nguyên, kể cả group đang đủ lượt hôm nay. Chưa có → group có `Number` nhỏ nhất mà (thành viên có `AddedAt > now − 24h`) + (SignInReservation Pending) `< IdentityGroupSize`. Không còn group nào → tạo group mới `Number = max+1`. |
| `insert_account` | `Accounts.Upsert` | So email không phân biệt hoa/thường; giữ `LastInstallAt` lớn hơn; giữ các DeviceAssignment; đóng dấu `AddedAt = now`; xoá `NeedsSignIn`. |
| `forget_account` | `Accounts.Delete` | Xoá tài khoản + DeviceAssignment của nó. Thêm lại = xếp identity mới. |
| `mark_account_full_sync` | `Accounts.Rest` | `RestUntil = now + DeviceWindowDays`, `RestReason = "manual"`. Thay cho cách UDID giả `manual-full:` của bản hiện tại. |
| `shorten_install_error` | `InstallStatuses.Shorten` | Gộp khoảng trắng thành 1 dòng, cắt 300 ký tự + `…`. |

**Thay đổi có chủ đích so với bản hiện tại** (ghi rõ để không ai "sửa lại cho giống"):

1. Tài khoản đang **nghỉ** (`RestUntil > now`) hoặc **cần đăng nhập lại** (`NeedsSignIn`) bị loại ở **cả luật (1)**. Ở bản hiện tại, dấu "đầy" chỉ chiếm chỗ, nên một thiết bị đã gán sẵn vào tài khoản bị đánh dấu vẫn được ký lại bằng nó. Người dùng đánh dấu chính là để tránh tài khoản đó, nên server chặn hẳn. Thiết bị đó sẽ được xếp sang tài khoản khác (tốn một chỗ mới).
2. Tài khoản đang bị máy khác giữ lease: ở luật (2) thì bỏ qua và chọn ứng viên kế tiếp; ở luật (1) thì thiết bị vào danh sách **chờ** (`waiting`, kèm `retryAfterSeconds`) thay vì đổi sang tài khoản khác, để không đốt thêm chỗ.
3. Apple báo đầy (`refuse` với `problem: "full"`) → `RestUntil = now + FullCooldownHours`, `RestReason = "apple_full: <thông báo>"`. Bản hiện tại chỉ bỏ qua tài khoản trong lượt đó, nên lượt sau lại thử lại; có server thì nhớ được cho mọi máy.

## Tranh chấp

- Mọi thao tác **làm thay đổi** kế hoạch, chỗ giữ, lease, xếp identity đều chạy trong **một `SemaphoreSlim(1, 1)` dùng chung toàn tiến trình** ("write gate") + một transaction. Server chỉ có một tiến trình, nên như vậy là đủ và dễ chứng minh đúng. Không chạy nhiều instance.
- Lease: tối đa **1 lease Active/Signed cho mỗi tài khoản**. `ExpiresAt = now + LeaseMinutes`, heartbeat gia hạn.
- `BackgroundService` quét mỗi 30 giây: lease hết hạn → `Expired`, Reservation Pending của nó → `Released`; SignInReservation hết hạn → `Expired`; khoá provisioning hết hạn → gỡ. Planner cũng quét "lười" trước mỗi lần tính.
- Ghi `state.plist` và khoá team dùng **optimistic concurrency**: request phải gửi `expectedVersion`; lệch → `409 stale_version` kèm bản hiện tại. Không bao giờ "ai ghi sau thắng" một cách im lặng.

## API

Header bắt buộc (trừ `/health`): `Authorization: Bearer <machine-key>`, `X-Client-Version`.

### Hệ thống
- `GET /api/v1/health` (không cần khoá) → `{ "status": "ok", "serverTime": "…Z", "version": "1.0.0" }`
- `GET /api/v1/snapshot` → toàn bộ những gì giao diện cần, **không có bí mật**. Hỗ trợ `ETag` / `If-None-Match` → `304`.

```json
{
  "serverTime": "2026-10-18T09:00:00Z",
  "policy": { "maxDevicesPerAccount": 3, "deviceWindowDays": 7, "identityGroupSize": 5, "identityWindowHours": 24, "fullCooldownHours": 24, "minSignInGapSeconds": 45 },
  "accounts": [{
    "id": "…", "email": "abc@x.com", "firstName": "A", "teamId": "XXXX",
    "addedAt": "…Z", "lastInstallAt": "…Z",
    "identityGroup": { "number": 0, "recentSignIns": 3 },
    "slotsUsed": 2, "slotsLeft": 1,
    "restUntil": null, "restReason": null,
    "needsSignIn": false, "needsSignInReason": null,
    "busyByMachine": null
  }],
  "deviceAssignments": [{ "udid": "…", "accountId": "…", "installedAt": "…Z" }],
  "installStatuses":   [{ "udid": "…", "ok": false, "at": "…Z", "error": "Your session has expired…" }]
}
```

### Tài khoản
- `PATCH /api/v1/accounts/{id}` `{ "teamId": "…" }`
- `DELETE /api/v1/accounts/{id}`
- `POST /api/v1/accounts/{id}/rest` / `DELETE /api/v1/accounts/{id}/rest`

### Kế hoạch & lease
- `POST /api/v1/plans`
```json
{ "purpose": "install", "mode": "auto", "manualAccountId": null,
  "devices": [{ "udid": "…", "name": "iPhone 1" }],
  "excludeAccountIds": [], "dryRun": false }
```
→
```json
{ "groups":    [{ "leaseId": "…", "account": { "id": "…", "email": "…", "teamId": "…", "identityGroupNumber": 0 }, "udids": ["…"] }],
  "waiting":   [{ "udid": "…", "accountId": "…", "busyByMachine": "PC-2", "retryAfterSeconds": 60 }],
  "unplanned": [{ "udid": "…", "reason": "every Apple ID already carries 3 devices in the last 7 days" }] }
```
`dryRun: true` → không tạo lease/reservation, `leaseId` = null (dùng cho cột xem trước trong Multi Install).

- `POST /api/v1/leases` `{ "purpose": "export" | "teams" | "refresh", "accountId": "…", "udids": [] }` → `{ "leaseId", "expiresAt" }`
- `GET /api/v1/leases/{id}/secrets` → **bí mật**, chỉ trả cho đúng máy giữ lease:
```json
{ "adsid": "…", "xcodeGsToken": "…", "teamId": "…",
  "identityGroup": { "number": 0, "version": 4, "stateBase64": "…" },
  "teamKey": { "version": 2, "pem": "-----BEGIN PRIVATE KEY-----…" },
  "expiresAt": "…Z" }
```
`identityGroup.stateBase64` có thể null (chưa provision); `teamKey` có thể null (team chưa có khoá).
- `POST /api/v1/leases/{id}/heartbeat` → `{ "expiresAt" }`
- `POST /api/v1/leases/{id}/signed` `{ "udids": ["…"] }` → Reservation → DeviceAssignment, cập nhật `LastInstallAt`.
- `POST /api/v1/leases/{id}/complete`
- `POST /api/v1/leases/{id}/refuse` `{ "problem": "full" | "sessionExpired" | "other", "message": "…" }`

### Identity & khoá team
- `PUT /api/v1/identity-groups/{number}/anisette` `{ "expectedVersion": 4, "stateBase64": "…", "leaseId": "…" }` (hoặc `signInId`) → `{ "version": 5 }` | `409 stale_version`
- `PUT /api/v1/teams/{teamId}/key` `{ "expectedVersion": 2, "pem": "…", "leaseId": "…" }` → `{ "version": 3 }` — bản mới thành `IsActive`, bản cũ giữ lại (không xoá) để tra cứu.

### Kết quả cài
- `POST /api/v1/install-results` `{ "results": [{ "udid": "…", "name": "…", "ok": false, "error": "…", "leaseId": "…" }] }`

### Đăng nhập
- `POST /api/v1/sign-ins` `{ "email": "…" }` →
```json
{ "signInId": "…", "expiresAt": "…Z",
  "identityGroup": { "number": 1, "version": 0, "stateBase64": null, "provisioningLock": true } }
```
hoặc `429 sign_in_gap { retryAfterSeconds }` / `409 group_provisioning_busy { retryAfterSeconds }`.
- `POST /api/v1/sign-ins/{id}/complete` `{ "firstName": "…", "adsid": "…", "xcodeGsToken": "…", "teamId": "…" }`
- `POST /api/v1/sign-ins/{id}/cancel`

### Quản trị (máy admin)
- `GET/PUT /api/v1/settings`
- `GET /api/v1/machines`, `POST /api/v1/machines { name, isAdmin }` → `{ apiKey }` (chỉ hiện **một lần**), `DELETE /api/v1/machines/{id}`

### Import từ bản hiện tại
- `POST /api/v1/import` (admin), luôn chạy `dryRun: true` trước để xem tóm tắt:
```json
{ "dryRun": true, "machineLabel": "PC-1",
  "identities": [{ "ref": "machine", "stateBase64": "…" }, { "ref": "group-0", "stateBase64": "…" }],
  "accounts":   [{ "email": "…", "firstName": "…", "adsid": "…", "xcodeGsToken": "…", "teamId": "…", "addedAt": "…Z", "lastInstallAt": "…Z", "identityRef": "group-0" }],
  "deviceAssignments": [{ "udid": "…", "email": "…", "installedAt": "…Z" }],
  "installStatuses":   [{ "udid": "…", "ok": true, "at": "…Z", "error": null }],
  "teamKeys": [{ "teamId": "…", "pem": "…", "fileTimeUtc": "…Z" }] }
```
Luật gộp:
- Identity: dedupe theo SHA-256 nội dung; khác nội dung → group mới trên server (`group-0` của PC-1 và `group-0` của PC-2 là hai identity **khác nhau**, phải thành hai `Number` khác nhau). `ref: "machine"` → `Origin = "machine:<machineLabel>"`.
- Tài khoản trùng email: giữ bản có `addedAt` mới hơn **cùng identity của nó**; token cũ hơn bỏ.
- DeviceAssignment: theo UDID, `installedAt` mới hơn thắng. Bỏ các UDID `manual-full:*` và đổi thành `RestUntil` cho tài khoản tương ứng (lấy mốc từ entry).
- InstallStatus: `at` mới hơn thắng.
- TeamKey: dedupe theo `PublicKeySha256`; team chưa có → bản đó active; đã có → thêm làm bản không active, trừ khi `fileTimeUtc` mới hơn bản đang active.
- Import chạy lại nhiều lần phải cho cùng kết quả (idempotent).

## Bảo mật (LAN)

- **HTTPS kể cả trong LAN.** Token và khoá certificate đi qua mạng; ai cắm vào LAN hoặc Wi-Fi đều nghe được HTTP thường. Dùng certificate tự ký cho Kestrel:
  ```powershell
  $c = New-SelfSignedCertificate -DnsName "accountserver","<IP-server>" -CertStoreLocation Cert:\LocalMachine\My -NotAfter (Get-Date).AddYears(5)
  Export-PfxCertificate -Cert $c -FilePath C:\ProgramData\ImpactorAccountServer\https.pfx -Password (Read-Host -AsSecureString)
  ```
  Client **ghim vân tay SHA-256** của certificate (lệnh `AccountServer.exe cert-fingerprint` in ra), không cần cài CA vào Windows của từng máy.
- **Khoá theo máy**: 32 byte ngẫu nhiên (base64url), chỉ hiện một lần lúc tạo, server lưu SHA-256, so sánh constant-time. Thu hồi được từng máy. Tạo bằng lệnh `AccountServer.exe add-machine --name PC-1 [--admin]` chạy trên server.
- **Mã hoá dữ liệu nghỉ**: ASP.NET Core Data Protection, `PersistKeysToFileSystem(C:\ProgramData\ImpactorAccountServer\dp-keys)` + `ProtectKeysWithCertificate(<pfx riêng>)`. Value converter mã hoá các cột `[Secret]`. Dùng certificate thay vì DPAPI để còn khôi phục được sang máy khác.
- **Log**: không bao giờ ghi body của `/secrets`, `/sign-ins/*/complete`, `/identity-groups/*`, `/teams/*`, `/import`; email luôn rút gọn (2 ký tự đầu + `…@miền`), như client đang làm.
- **Firewall**: chỉ mở cổng cho mạng nội bộ.
  ```powershell
  New-NetFirewallRule -DisplayName "Impactor Account Server" -Direction Inbound -Protocol TCP -LocalPort 8443 -RemoteAddress LocalSubnet -Action Allow
  ```
- Thư mục `C:\ProgramData\ImpactorAccountServer\` chỉ cấp quyền cho tài khoản chạy service và Administrators.

## Triển khai trên Windows Server 2019

1. `dotnet publish src/AccountServer -c Release -r win-x64 --self-contained true -o C:\Services\ImpactorAccountServer` (tự chứa runtime, không cần cài .NET lên server).
2. Tạo certificate HTTPS + certificate bảo vệ khoá Data Protection (hai pfx khác nhau), sửa `appsettings.Production.json` (đường dẫn pfx; mật khẩu lấy từ biến môi trường của service, không viết vào file).
3. `AccountServer.exe migrate` (áp migration), `AccountServer.exe add-machine --name ADMIN --admin`.
4. Đăng ký service:
   ```powershell
   sc.exe create ImpactorAccountServer binPath= "C:\Services\ImpactorAccountServer\AccountServer.exe" start= auto
   sc.exe start ImpactorAccountServer
   ```
5. Firewall như trên. Kiểm tra từ một PC: `https://<IP-server>:8443/api/v1/health`.

## Sao lưu — đọc kỹ

Ba thứ phải sao lưu **cùng nhau**: file DB, thư mục `dp-keys\`, và pfx bảo vệ khoá (cất ngoài server). Mất `dp-keys` hoặc pfx thì mọi token, `state.plist`, khoá team trong DB thành rác: **toàn bộ Apple ID phải đăng nhập lại và mọi certificate phải tạo lại**. Lịch: Task Scheduler hằng ngày chạy `AccountServer.exe backup <thư mục>` (SQLite `VACUUM INTO` + copy `dp-keys`), giữ 14 bản, và thử khôi phục ít nhất một lần.

## Test bắt buộc

**Core.Tests** — port đủ các test sau từ `store.rs` (đặt cùng tên, PascalCase), chúng là tiêu chí nghiệm thu của luật:

`a_fresh_account_is_offered_exactly_three_devices`, `remaining_slots_are_the_cap_minus_what_is_carried`, `a_device_already_on_a_full_account_still_reinstalls`, `a_full_account_hands_the_next_device_to_one_with_room`, `slots_come_back_when_installs_age_out`, `newest_accounts_fill_first_before_older_ones`, `projection_spreads_devices_the_way_a_run_will`, `install_status_is_recorded_and_ages_out`, `install_errors_are_flattened_and_capped`, `marking_an_account_full_uses_up_its_slots_until_it_ages_out`, `a_mark_can_be_cleared_without_touching_real_installs`, `removed_account_is_forgotten`, `re_adding_an_account_makes_it_new_again`, `re_adding_with_different_case_keeps_one_entry_and_its_devices`, `groups_fill_in_order_five_at_a_time`, `the_rate_per_identity_follows_the_setting`, `a_group_takes_new_accounts_again_once_its_sign_ins_are_a_day_old`, `a_sign_in_just_inside_the_window_still_counts`, `a_re_added_account_keeps_its_group_even_when_the_group_is_at_the_rate`, `removing_an_account_and_adding_it_back_places_it_afresh`, `devices_are_planned_into_one_group_per_account`, `a_refused_account_is_left_out_of_the_plan`, `a_device_already_on_an_account_joins_that_account_s_group`, `last_install_is_stamped_and_outlives_the_device_entries`, `re_adding_an_account_keeps_its_install_history`, `undated_members_do_not_count_against_the_rate`.

(Bỏ `grouping_off_means_the_machine_identity_for_everyone` và `accounts_from_before_grouping_are_placed_when_re_added` vì bản Server luôn bật group; bỏ `a_mark_belongs_to_one_account_only` vì không còn UDID giả. Thêm test cho 3 "thay đổi có chủ đích" ở trên.)

**Api.Tests** — tranh chấp, chạy thật trên SQLite file tạm:

- Hai `POST /plans` đồng thời cho cùng 4 UDID, chỉ một tài khoản còn 3 chỗ → tổng chỗ đã cấp ≤ 3.
- Tài khoản đang bị PC-2 giữ lease → kế hoạch của PC-1 cho thiết bị gán sẵn vào nó trả về `waiting`.
- Lease hết hạn không heartbeat → chỗ giữ được trả, tài khoản lại xếp được.
- `refuse sessionExpired` → `needsSignIn: true` trong snapshot của mọi máy; `refuse full` → `restUntil` = now + `FullCooldownHours`.
- Hai `POST /sign-ins` đồng thời khi group 0 đã có 4/5 → một vào group 0, một vào group 1.
- `MinSignInGapSeconds` chặn lần đăng nhập thứ hai quá sớm, kể cả từ máy khác.
- Ghi `state.plist` với `expectedVersion` cũ → `409`; ghi đúng → version tăng.
- `/secrets` của lease thuộc máy khác → `403`; máy bị thu hồi → `401`.
- Import chạy hai lần → kết quả giống hệt lần một.
