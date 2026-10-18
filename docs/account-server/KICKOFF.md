# Prompt mở phiên ở tab Code

Mở phiên mới với repo `dviet92/Impactor`, branch `feat/account-server` (tạo ở giai đoạn 0 trong `README.md`), rồi dán nguyên đoạn dưới:

---

Mình muốn làm **bản Server** của Impactor trên branch `feat/account-server`: thay vì mỗi máy tính giữ Apple ID riêng, mọi máy Windows trong mạng nội bộ dùng chung một Account Server viết bằng C# (.NET 10, ASP.NET Core, EF Core code-first, SQLite), chạy trên Windows Server 2019 của mình. Bản hiện tại trên `main` giữ nguyên và phải chạy song song được.

Toàn bộ đặc tả nằm ở `docs/account-server/`. Trước khi viết code:

1. Đọc `docs/account-server/README.md`, rồi `01-architecture.md`, `02-server.md`, `03-client.md`, `04-pitfalls.md`.
2. Đọc `crates/plume_store/src/store.rs` (luật chọn tài khoản, identity group — server phải chép đúng), `apps/plumeimpactor/src/subscriptions.rs` (luồng cài nhiều máy) và `crates/plume_core/src/utils/certificate.rs` (vì sao khoá certificate phải dùng chung).
3. Copy `docs/account-server/CLAUDE.branch.md` thành `CLAUDE.md` ở gốc repo trên branch này, thêm `.gitattributes` với `* text=auto`.
4. Tóm tắt lại cho mình bằng tiếng Việt: bạn hiểu kiến trúc thế nào, và chỗ nào trong đặc tả bạn thấy chưa rõ hoặc sai so với code thật. Hỏi mình trước khi tự quyết những chỗ đó.

Sau khi mình xác nhận, làm lần lượt theo bảng giai đoạn trong `README.md`, bắt đầu từ **giai đoạn 1** (cho hai bản chạy song song) rồi **giai đoạn 2** (khung server). Mỗi giai đoạn một PR, ghi rõ đã test gì trong cloud và cần mình thử gì trên Windows / iPhone thật.

---

## Ghi chú cho người dùng

- Nên chọn model mạnh nhất cho phiên này: phần luật và tranh chấp giữa nhiều máy cần đọc code kỹ.
- Phiên Code không thấy được máy của anh: không chạy được tool, không thấy server trong LAN, không thấy iPhone. Mọi thứ thật phải anh build và chạy trên Windows rồi dán lỗi/log lại cho nó.
- Nếu phiên Code hỏi về chuyện đã xảy ra ở bản hiện tại (lỗi 503, 429, identity, ký một lần…), các ghi chú chi tiết nằm trong project "Tool automation trên iPhone non jailbreak" (`claude/*.md`). Có thể dán đoạn liên quan cho nó.
