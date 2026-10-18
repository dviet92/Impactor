# 04 — Lưu ý, bẫy đã gặp, và cách kiểm tra trên máy thật

Phần lớn mục dưới đây là bài học đã trả giá ở bản hiện tại (chi tiết nằm trong các ghi chú `claude/*.md` của project "Tool automation trên iPhone non jailbreak"). Bản Server dùng lại nguyên code ký và đăng nhập, nên **đừng "dọn dẹp" những chỗ này**.

## Apple — không được đổi lại

| Chỗ | Đừng làm | Vì sao |
|---|---|---|
| `GSA_CLIENT_APP = "com.apple.AuthKit/1 (com.apple.akd/1.0)"` (`crates/plume_core/src/auth/anisette_data.rs`) và `accepted_client_info()` trong `3rdparty/omnisette/src/remote_anisette_v3.rs` | Đưa `com.apple.dt.Xcode` trở lại header `X-MMe-Client-Info` | Từ 9/2026 Apple trả HTML 503 cho mọi request mang token đó, trước cả khi kiểm tra mật khẩu. Server anisette (`ani.sidestore.app`) vẫn trả chuỗi có `com.apple.dt.Xcode` — client phải tự thay. |
| `gsa_client()` dùng `pool_max_idle_per_host(0)` | Bật lại connection pooling cho GSA | Edge của Apple chỉ cho vài request trên một kết nối keep-alive; request thứ ba trên kết nối cũ nhận HTML. |
| `developer_client()` **không** đi qua proxy | Cho request ký đi qua proxy | Ký không cần proxy, lại chậm. Kết nối tới Account Server cũng **không** đi qua proxy. |
| Retry 429 có chờ (`rate_limit_wait`), 503 thử lại một lần | Retry dồn dập | 429 là giới hạn **theo mạng/identity**; thử dồn chỉ kéo dài thời gian bị chặn. Mọi PC trong LAN ra Internet cùng một IP → **Apple coi cả LAN là một mạng**. Đây là lý do server giãn đăng nhập trên toàn LAN (`MinSignInGapSeconds`). |
| Provisioning anisette: `{"result":"Timeout"}` → `ProvisioningTimeout`, thử lại 6 lần, backoff 5s | Coi Timeout là lỗi cuối | Provision identity mới là bước hay hỏng nhất; đã từng có vòng lặp quay vô tận khi stream kết thúc. |

## Identity, token, certificate

- **Token ràng với identity.** Đổi identity của một tài khoản = token chết. Server không bao giờ được "chuyển" tài khoản sang group khác; chỉ xoá rồi thêm lại.
- **Một khoá certificate cho mỗi team, dùng chung.** Máy nào không có khoá sẽ xin certificate mới, và khi Apple báo quá số lượng (lỗi 7460) code sẽ **revoke** certificate cũ → app đã cài bằng certificate đó có thể ngừng mở. Xem `01-architecture.md`, quy tắc 2.
- **Ký một lần cho cả nhóm**: profile team không chứa UDID, nên đăng ký hết thiết bị rồi ký một lần là đủ cho mọi máy trong nhóm. **Giả định này đúng theo hình dạng API nhưng chưa được xác nhận trên máy thật** (xem `claude/multi-install-sign-once-parallel.md`). Bản Server dựa vào nó, nên đây là mục số 1 trong checklist máy thật bên dưới.
- Luật "tối đa 3 máy / Apple ID / 7 ngày" là **luật vận hành của chủ tool**, không phải con số Apple công bố. Apple có giới hạn riêng (App ID mỗi tuần, số thiết bị) và báo bằng thông báo lỗi; nhận diện bằng `is_account_full_error` trong `subscriptions.rs` (khớp theo hình dạng câu, không theo một chuỗi cố định).

## Cài song song

- Mỗi Apple ID một luồng; các Apple ID khác nhau song song tới `install_parallel`. Thiết bị chung một ID **không** tách luồng (yêu cầu của chủ tool).
- Mỗi luồng giải nén vào thư mục riêng `get_package_bundle_in_slot(slot)` — `get_package_bundle()` luôn xoá và giải nén lại vào **cùng một chỗ**, hai luồng dùng nó cùng lúc sẽ phá file của nhau khi đang ký.
- Phần ký chạy trong `spawn_blocking` + runtime `current_thread` riêng vì future của signer (`apple_codesign` `PrivateKey`, callback `dyn FnMut`) không `Send`, nên không dùng `tokio::spawn` được.
- Khi tải `.ipa`, `Package::new` đọc xuyên mọi entry (kiểm CRC) và log `sha256` — giữ nguyên, nó là công cụ chẩn đoán vụ ".ipa bị nhỏ đi" vẫn còn mở (`claude/ipa-stage-crash-cleanup.md`).

## Dữ liệu và bảo mật

- Token, `state.plist`, khoá PEM, khoá máy: **không bao giờ** ghi vào log, kể cả log debug; email luôn rút gọn (2 ký tự đầu). Đó là quy ước cũ, áp dụng cho cả server.
- Ai có `accounts.db` + `dp-keys` + pfx là có toàn bộ Apple ID mà không cần mật khẩu. Ba thứ đó cất như cất mật khẩu.
- Sau khi đã đẩy tài khoản từ bản cũ lên server: **đừng dùng cùng những Apple ID đó trên bản cũ song song nữa**. Bản cũ không biết lease, không đếm theo server, và nếu nó tạo certificate mới thì revoke luôn certificate mà mọi máy khác đang dùng. Muốn chắc ăn: xoá các tài khoản đó khỏi bản cũ trên máy đã import.

## Git và môi trường làm việc

- Repo trên GitHub lưu **LF**; ổ đĩa Windows của chủ tool đang để **CRLF** (không có `.gitattributes`, `core.autocrlf` chưa đặt). Đặt `git config core.autocrlf true` trước khi commit trên Windows, nếu không mỗi commit sẽ biến cả file thành CRLF và diff trông như viết lại toàn bộ. Nên thêm `.gitattributes` (`* text=auto`) trên branch.
- Phiên Code chạy Linux trong cloud: **không** thấy server trong LAN, **không** cắm được iPhone, **không** chạy được giao diện Windows. Trong cloud chỉ làm được: viết code, `cargo check`/`cargo test`, `dotnet build`/`dotnet test` (cài .NET 10 SDK nếu chưa có). Mọi thứ đụng tới Apple, iPhone, Windows Service đều phải chủ tool chạy thử trên Windows.
- Code chỉ dành cho Windows (Windows Service, DPAPI) để sau `if (OperatingSystem.IsWindows())` / `#[cfg(windows)]`, để test vẫn chạy được trên Linux.

## Checklist chạy thử trên máy thật (chủ tool làm, theo thứ tự)

1. **Ký một lần cho nhiều máy** (trên bản hiện tại, trước khi làm gì khác): Multi Install 2–3 iPhone cùng một Apple ID → máy thứ 2, thứ 3 có cài vào và **mở được** app không.
2. Server: `/health` gọi được từ một PC khác qua HTTPS; certificate sai thì client từ chối.
3. Import từ một PC (dry-run trước) → số tài khoản/identity/khoá team đúng như bản cũ hiển thị.
4. Từ PC-2 (chưa từng đăng nhập Apple ID nào): cài một iPhone bằng tài khoản PC-1 đã import → **không** hiện cảnh báo reset certificate, **không** bị "needs signing in again".
5. Hai PC cùng bấm Multi Install, tổng số máy nhiều hơn số chỗ còn lại của một tài khoản → không tài khoản nào vượt 3 máy; máy bị tài khoản bận thì nằm ở "Đang chờ Apple ID" rồi tự chạy tiếp.
6. Tắt nguồn một PC giữa lúc ký → sau `LeaseMinutes` tài khoản tự trống lại.
7. Thêm Apple ID mới từ PC-1 và PC-2 gần như cùng lúc → server bắt giãn ra; cả hai vào đúng identity group; group mới được provision đúng một lần.
8. Đánh dấu đầy trên PC-1 → PC-2 thấy `(marked)` sau vài giây và không chọn tài khoản đó.
9. Rút cáp mạng của PC → bản Server báo lỗi rõ, không ký bằng dữ liệu cũ.
10. Sao lưu, rồi khôi phục sang một thư mục khác và chạy server từ bản khôi phục → đăng nhập/cài vẫn được (chứng minh `dp-keys` + pfx đã được sao lưu đúng).
