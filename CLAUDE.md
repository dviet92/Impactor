# CLAUDE.md — branch `feat/account-server`

Đây là **bản Server** của Impactor (Rust, iced 0.14): mọi Apple ID nằm trên một Account Server trong LAN (C# .NET 10, thư mục `server/`), các máy Windows dùng chung. Đặc tả đầy đủ: `docs/account-server/` — đọc `README.md` rồi `01` → `04` trước khi sửa gì.

## Quy tắc làm việc

- Người dùng nói tiếng Việt; trả lời và viết tài liệu bằng tiếng Việt. Tên biến, hàm, file, commit message phần mã: tiếng Anh.
- **Giữ diff với `main` nhỏ.** `main` là bản đang dùng hằng ngày và vẫn được sửa tiếp; branch này sẽ merge `main` vào nhiều lần. Code mới đặt trong file/crate mới (`crates/plume_remote`, `apps/plumeimpactor/src/edition.rs`, `server/`); file cũ chỉ sửa ở chỗ gọi.
- **Không đổi** các chỗ liệt kê trong `docs/account-server/04-pitfalls.md` mục "Apple — không được đổi lại" (token `akd`, `pool_max_idle_per_host(0)`, không proxy cho ký, retry 429/503, timeout provisioning).
- Luật chọn tài khoản / identity trên server phải khớp `crates/plume_store/src/store.rs` (trừ ba "thay đổi có chủ đích" ghi trong `02-server.md`). Đổi luật thì đổi cả hai bên và cả test.
- Không bao giờ ghi token, `state.plist`, khoá PEM, khoá máy ra log hay ra test fixture thật. Email trong log luôn rút gọn.
- Môi trường cloud là Linux: không chạy được giao diện Windows, không cắm iPhone, không với tới server trong LAN. Kiểm tra bằng `cargo check -p plumeimpactor`, `cargo test -p plume_store -p plume_remote`, `dotnet test server/ImpactorAccountServer.sln`. Việc gì cần máy thật thì ghi rõ trong mô tả PR để người dùng chạy thử.
- Mỗi giai đoạn trong `README.md` là một PR. Mô tả PR ghi: đã làm gì, đã test gì, cái gì cần người dùng thử trên Windows.

## Build trên Windows (người dùng)

- Client: `cargo build -p plumeimpactor --release` → `target\release\plumeimpactor-server.exe` (sau giai đoạn 1).
- Server: `dotnet publish server/src/AccountServer -c Release -r win-x64 --self-contained true`.
