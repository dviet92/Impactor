# Impactor bản Server — kho Apple ID dùng chung cho nhiều máy

Tài liệu này mô tả bản Impactor thứ hai: thay vì mỗi máy giữ tài khoản riêng trong `%APPDATA%\PlumeImpactor\accounts.json`, mọi máy trong mạng nội bộ dùng chung một **Account Server**. Máy nào cũng ký được bằng bất kỳ Apple ID nào; đăng nhập mới, thiết bị đã gán, kết quả cài, đánh dấu đầy… được ghi lên server ngay và mọi máy thấy như nhau.

Bản hiện tại (`main`) **giữ nguyên** và vẫn chạy được song song trên cùng một máy.

## Đọc theo thứ tự

1. [`01-architecture.md`](01-architecture.md) — dữ liệu nào lên server, dữ liệu nào ở lại máy, ba quy tắc không được phá, các luồng chính.
2. [`02-server.md`](02-server.md) — Account Server bằng C# (.NET 10, ASP.NET Core, EF Core code-first, SQLite): entity, luật, API, bảo mật, triển khai trên Windows Server 2019, test bắt buộc.
3. [`03-client.md`](03-client.md) — thay đổi phía Impactor (Rust): cho hai bản chạy song song, crate `plume_remote`, bảng đổi chỗ gọi.
4. [`04-pitfalls.md`](04-pitfalls.md) — bài học đã gặp (Apple, identity, certificate, cài song song, git) + checklist chạy thử trên máy thật.
5. [`KICKOFF.md`](KICKOFF.md) — đoạn prompt để dán vào phiên mới bên tab Code.
6. [`CLAUDE.branch.md`](CLAUDE.branch.md) — nội dung `CLAUDE.md` đặt ở gốc branch.

## Đã chốt

| Câu hỏi | Chọn |
|---|---|
| Server chạy ở đâu | Một máy **Windows Server 2019** trong **mạng nội bộ** (không mở ra Internet) |
| Ngôn ngữ server | **C#**, **.NET 10 (LTS, hỗ trợ tới 14/11/2028)**, ASP.NET Core, **EF Core code-first**; DB mặc định SQLite, đổi sang SQL Server Express được |
| Thêm Apple ID ở đâu | **Trong tool**, máy nào cũng được, như bây giờ (cửa sổ login + 2FA giữ nguyên); xong thì đẩy lên server |
| Ai ký app | **Từng máy** ký như hiện tại; server chỉ giữ sổ, giữ bí mật, và điều phối |
| Bản mới để ở đâu | Branch **`feat/account-server`** trong repo `dviet92/Impactor`; server nằm trong thư mục `server/` của cùng repo, để API và client đổi cùng nhau |
| Hai bản trên một máy | Chạy song song được: khác tên app, khác thư mục dữ liệu, khác tiền tố thư mục tạm (xem `03-client.md`, bước 1) |

Còn để ngỏ (mặc định ghi trong `02-server.md`, đổi được khi làm):

- Apple báo "đầy" thì nghỉ bao lâu (`FullCooldownHours`, mặc định 24 giờ).
- Khoảng nghỉ tối thiểu giữa hai lần đăng nhập trên toàn LAN (`MinSignInGapSeconds`, mặc định 45 giây).
- Thời hạn lease (`LeaseMinutes`, mặc định 20 phút).

## Kế hoạch theo giai đoạn

Mỗi giai đoạn là một PR riêng, có tiêu chí "xong khi" rõ ràng.

| # | Việc | Xong khi |
|---|---|---|
| 0 | **Chốt bản hiện tại**: commit + push toàn bộ thay đổi đang nằm trên máy lên `main`, gắn tag, tạo branch (xem mục dưới). Thêm thư mục `docs/account-server/` và `CLAUDE.md`. | `main` trên GitHub chứa đúng bản đang chạy; branch `feat/account-server` tồn tại. |
| 1 | **Hai bản sống chung**: đổi tên app, thư mục dữ liệu, tiền tố stage, tên exe trên branch. | Mở cả hai exe trên cùng máy cùng lúc, mỗi bản giữ dữ liệu riêng; cài bằng bản này không làm hỏng bản kia. |
| 2 | **Server khung**: solution, entity, migration, khoá máy, `/health`, `/snapshot`, `/settings`, lệnh `add-machine` / `cert-fingerprint` / `export-cert` / `migrate`, chạy như Windows Service. | `dotnet test` xanh; chạy được trên Windows Server; một PC gọi được `/health` qua HTTPS. |
| 3 | **Luật + kế hoạch + lease**: port luật từ `store.rs`, `/plans`, `/leases/*`, `/install-results`, `/accounts/*`; sweeper hết hạn. | Toàn bộ test trong danh sách của `02-server.md` xanh, gồm test tranh chấp hai máy. |
| 4 | **Identity + đăng nhập + khoá team**: `/sign-ins/*`, `/identity-groups/*/anisette`, `/teams/*/key`, khoảng nghỉ toàn LAN, khoá provisioning. | Test đăng nhập đồng thời và optimistic concurrency xanh. |
| 5 | **Import**: `/import` (dry-run trước) + nút "Đẩy tài khoản từ bản cũ" trong tool. | Import hai lần cho cùng kết quả; trên máy thật số liệu khớp bản cũ. |
| 6 | **Client nối server**: crate `plume_remote`, `LeaseGuard`, khối "Account server" ở tab Connection, đổi các chỗ gọi theo bảng trong `03-client.md`. | `cargo check` + test xanh; trên máy thật chạy được mục 2–9 của checklist trong `04-pitfalls.md`. |
| 7 | **Refresh / Export / Teams qua lease**, sao lưu tự động, hoàn thiện giao diện. | Mục 10 của checklist (khôi phục từ bản sao lưu) chạy được. |
| 8 | **Đưa vào dùng**: một PC trước, vài ngày sau mới thêm các PC khác. | |

## Giai đoạn 0 — làm trên máy Windows trước khi mở phiên Code

Toàn bộ chỉnh sửa từ 27/08/2026 tới giờ (sửa đăng nhập Apple, 429, proxy, identity group, ký một lần, cài song song, log lỗi theo máy, đánh dấu đầy, kiểm tra .ipa…) **mới chỉ nằm trên ổ đĩa**, chưa commit (`git status` trên máy cho thấy 33 file sửa, commit gần nhất là `b64d4ce fix add multi`, 27/08). Phiên Code clone repo từ GitHub, nên nếu chưa push thì nó sẽ làm việc trên bản cũ của tháng 8.

Trong `C:\Users\Administrator\Documents\GitHub\Impactor`, mở PowerShell:

```powershell
# 1. Để git lưu LF như repo vẫn lưu, thay vì đổi cả file sang CRLF
git config core.autocrlf true

# 2. Xem lại những gì sắp commit (không được có accounts.json, state.plist, *.p12, key.pem)
git status
git diff --stat

# 3. Chốt bản hiện tại lên main
git add -A
git commit -m "Gom thay đổi 27/08–18/10/2026: sửa đăng nhập Apple, proxy, identity group, ký một lần, cài song song, log lỗi, đánh dấu đầy"
git tag local-2026-10-18
git push origin main --tags

# 4. Tạo branch cho bản Server và đẩy lên
git switch -c feat/account-server
git push -u origin feat/account-server
```

Thư mục `docs/account-server/` này nằm sẵn trong repo trên máy, nên sẽ đi theo commit ở bước 3 (có cả trên `main` lẫn branch mới).

## Mở phiên ở tab Code

Chọn repo `dviet92/Impactor`, branch **`feat/account-server`**, rồi dán nội dung [`KICKOFF.md`](KICKOFF.md).
