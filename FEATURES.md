# RustGirl — Danh sách tính năng đã hoàn thành

> Cập nhật theo tiến độ roadmap "100% Postman parity". Xem plan đầy đủ tại
> `~/.claude/plans/playful-snuggling-orbit.md`. File này chỉ liệt kê tính
> năng đã xong (Phase 1–6); Phase 7–12 chưa làm.

## Phase 1 — Nền tảng data model ✅
- **Auth block** (`AuthKind`/`AuthConfig`) trên `RequestItem`, `Folder`,
  `Collection`, hỗ trợ `Inherit` để kế thừa từ folder/collection cha.
- **Folder lồng nhau** (`Folder.folders: Vec<Folder>`) — không giới hạn độ sâu.
- **Nhiều scope biến số**: Environment > Collection variables > Global
  variables, với `resolve_variables` áp dụng đúng thứ tự ưu tiên.
- **Test harness** `egui_kittest`: chụp ảnh headless để tự kiểm tra UI mỗi
  phase (không cần trình duyệt/thao tác tay).

## Phase 2 — Import / Export ✅
- Import/Export **Postman Collection v2.1** (JSON), giữ đúng auth, script,
  biến số, nested folder.
- Import/Export **Postman Environment v1**.
- Import **OpenAPI 3.0** (JSON hoặc YAML) — one-way, tự nhóm theo tag,
  convert path param, nhận diện request body JSON.
- Import **curl command** (paste trực tiếp) — nhận diện method, header,
  data, basic auth, multipart form.
- Script JS từ Postman được giữ lại dạng comment trong ô script Lua (để
  người dùng tự port sang Lua), export lại vẫn giữ nguyên nếu chưa sửa.
- **Đã test thật với dữ liệu thật** (21 collection Postman thật của người
  dùng, ~700 request) và sửa 3 lỗi thật phát hiện được lúc đó:
  - Một request có **nhiều script cùng loại** (2 script `test` trên 1
    request) trước đây bị ghi đè, chỉ giữ script cuối cùng — giờ nối lại
    đầy đủ, không mất script nào.
  - Placeholder script rỗng của Postman (tab script mở ra nhưng chưa gõ gì,
    `exec: [""]`) trước đây vẫn bị tính là "có script" và tạo cảnh báo giả —
    giờ bỏ qua đúng, không còn cảnh báo thừa.
  - Thêm `pm.response:getHeaders(key)` (số nhiều) vào API Lua — đọc được
    **tất cả** header trùng tên (ví dụ nhiều `Set-Cookie`), thay vì chỉ cái
    đầu tiên như `getHeader` cũ — cần thiết để port đúng script thật của
    người dùng (đọc cookie theo tên cụ thể).

## Phase 3 — Collection tree UX ✅
- **Rename / Delete / Duplicate** cho collection, folder, request ở mọi
  cấp độ (không chỉ cấp gốc).
- **Kéo-thả (drag & drop)** để sắp xếp lại thứ tự và di chuyển
  request/folder giữa các folder.
- **Context menu** (chuột phải) cho mọi item trong cây collection.
- Sửa lỗi thật: request row không chọn được do bị `drag source` chặn mất
  sự kiện click — đã tách riêng "tay cầm kéo" (drag handle) khỏi phần click chọn.

## Phase 4 — Auth UI trong request builder ✅
- Tab **Authorization** riêng cho từng request, áp dụng thật vào request
  gửi đi (trước đó chỉ là data chết).
- Hỗ trợ: **Basic, Bearer, API Key (header/query), Digest (RFC 2617 thật,
  2 lượt request), OAuth 1.0a (HMAC-SHA1 ký chữ ký thật), OAuth 2.0 (Client
  Credentials grant), AWS Signature v4** (đã verify khớp byte-exact với
  test vector chính thức của AWS).
- `Inherit` hiển thị rõ đang kế thừa auth từ đâu (request → folder →
  collection).

## Phase 5 — Variables & scripting parity ✅
- **Dynamic variables** kiểu Postman: `{{$guid}}`, `{{$timestamp}}`,
  `{{$isoTimestamp}}`, `{{$randomInt}}`, `{{$randomBoolean}}`,
  `{{$randomColor}}`, `{{$randomIP}}`, `{{$randomFirstName/LastName/FullName}}`,
  `{{$randomEmail}}`, `{{$randomWord(s)}}` — mỗi lần xuất hiện sinh giá trị
  mới (giống Postman thật).
- Lua API (`pm.*`) mở rộng: `pm.globals` (get/set), `pm.collectionVariables`
  (get/set), `pm.variables` (đọc gộp cả 3 scope theo đúng thứ tự ưu tiên),
  `pm.sendRequest(url hoặc table, callback)` — gửi request phụ ngay trong
  script, có response trả về callback.
- Nút **snippet nhanh** trên ô script (giống Postman) để chèn sẵn mẫu
  `pm.test`, `pm.environment.set`, `pm.globals.set`,
  `pm.collectionVariables.set`, `pm.sendRequest`, `console.log`.

## Phase 6 — Client settings: cookies, proxy, SSL/certs ✅
- **Cookie jar thật** (`reqwest_cookie_store`) — cookie tự động lưu/gửi
  giữa các request, sống sót qua restart (`cookies.json`), đúng hành vi
  trình duyệt thật (cookie session không có Max-Age sẽ không được lưu ra
  đĩa — đây là hành vi cố ý, không phải bug).
- **Proxy**: bật/tắt, HTTP proxy URL, HTTPS proxy URL, No-proxy hosts
  (hỗ trợ cả `socks5://`).
- **SSL/TLS**: toggle "Disable certificate verification" (có cảnh báo đỏ
  rõ ràng), chọn file Custom CA certificate, chọn file Client certificate
  (mTLS).
- Panel **Settings** riêng trong UI, nút "Save & Apply" lưu và rebuild
  lại HTTP client ngay lập tức.

## Phase 7 — Response viewer parity ✅
- **Tab Cookies**: liệt kê cookie thật trong jar khớp domain của request
  hiện tại (tên, giá trị, path, expires, cờ Secure/HttpOnly).
- **Save Response as Example**: đặt tên và lưu lại 1 response bất kỳ ngay
  trên request đó (không phụ thuộc lịch sử 200-entry toàn cục), có thể xem
  lại/đổi tên/xoá từng example qua chip + menu chuột phải, bấm lại chip để
  quay về response sống (live).
- **Response diff**: chọn 2 example đã lưu, so sánh dòng-theo-dòng (thêm/
  bớt tô màu xanh/đỏ) kèm dòng tóm tắt chênh lệch status code + kích thước.
- HTML Visualizer: **không làm** trong phase này (quyết định của user, có
  thể làm riêng sau nếu cần).

## Phase 8 — Multi-tab requests ✅
- Mở nhiều request cùng lúc dưới dạng **tab**, mỗi tab có state riêng
  (request, response, script log, ví dụ đã lưu...) — giống hệt Postman.
- **Tab bar phong cách Postman**: badge method tô màu riêng (GET xanh lá,
  POST cam, PUT xanh dương, DELETE đỏ...), chấm báo chưa lưu, nút đóng ×,
  nút + mở tab mới, kéo-thả để sắp xếp lại tab.
- Click 1 request đã mở ở tab khác sẽ **focus lại tab đó** thay vì mở trùng
  (giống Postman/trình duyệt thật).
- Đóng tab cuối cùng luôn để lại 1 tab trống, không bao giờ về 0 tab.
- Phím tắt: Cmd/Ctrl+T (tab mới), Cmd/Ctrl+W (đóng tab), Ctrl+Tab (chuyển tab).
- Sidebar hiển thị 2 cấp: request đang mở ở tab active được tô đậm, request
  đang mở ở tab khác có dấu chấm nhỏ đánh dấu.
- **Sửa 1 bug thật quan trọng**: trước đây nếu gửi request ở tab thứ 2 khi
  tab thứ nhất còn đang chờ phản hồi, phản hồi của tab thứ nhất sẽ bị mất
  hoàn toàn (do chỉ theo dõi được 1 request "đang bay" toàn cục). Giờ mỗi
  tab tự theo dõi request riêng của mình nên phản hồi luôn về đúng tab.

## Phase 9 — Collection Runner ✅
- Chạy **cả collection hoặc chỉ 1 folder** tuần tự, lặp lại N lần (hoặc theo
  số dòng file dữ liệu), qua panel Runner riêng.
- **Data file CSV hoặc JSON**: mỗi dòng/mỗi object trở thành 1 bộ biến số ưu
  tiên cao nhất cho iteration đó (đúng kiểu Postman) — kể cả giá trị JSON
  không phải string cũng tự chuyển thành text, không bị từ chối.
- Chạy pre-request/post-response script (kể cả `pm.test`) đầy đủ cho từng
  request trong mỗi lần lặp, y hệt như gửi request thủ công.
- Có nút **Stop** dừng giữa chừng, thanh tiến trình, delay tuỳ chỉnh giữa
  các request.
- Bảng kết quả trực tiếp: method, tên, status (tô màu), thời gian, số test
  pass/fail — kèm tổng kết cuối cùng và nút "Copy results as JSON" để xuất
  báo cáo.
- Kết quả Runner **không** lưu vào History chung (tránh làm tràn giới hạn
  200 mục khi chạy hàng loạt).
- Vào Runner bằng nút "Run" cạnh mỗi collection hoặc "Run folder" trong menu
  chuột phải của folder.

## Phase 10 — Code generation ✅
- Tab **Code** mới trong request editor: sinh code mẫu để gửi request hiện
  tại bằng **curl, JavaScript (fetch), Python (requests), Rust (reqwest)**.
- Giữ nguyên `{{variable}}` chưa resolve trong code sinh ra (giống Postman
  thật) — code có thể copy dùng ở môi trường khác.
- Auth tĩnh (Basic/Bearer/ApiKey/OAuth2 có sẵn token) được nhúng thật vào
  code, dùng đúng cách "chuẩn" của từng ngôn ngữ (`-u` của curl,
  `HTTPBasicAuth`/`HTTPDigestAuth` của Python, `.basic_auth()`/`.bearer_auth()`
  của Rust...).
- Auth cần ký theo thời gian thực (OAuth1, AWS SigV4) hoặc cần round-trip
  sống (Digest ở JS/Rust) thì **không** giả một chữ ký sai — thay vào đó
  hiện ghi chú rõ ràng, tránh sinh ra code trông đúng nhưng chạy sai.
- Nút "Copy snippet" để copy nhanh.

## Phase 11 — UI polish ✅
- **Theme switcher** thật: Light / Dark / System, đổi trong Settings, áp
  dụng ngay lập tức, lưu lại giữa các lần mở app.
- **Command palette** hợp nhất (Cmd/Ctrl+K, giữ cả Alt+Space cũ): gõ vào là
  tìm được cả request lẫn lệnh — New Tab, Close Tab, Save, mở Settings, mở
  Collection Runner, chuyển tab sidebar, chuyển Environment, đổi hướng
  Split, đổi Theme — tất cả trong 1 ô tìm kiếm.
- Thêm phím tắt còn thiếu: **Cmd/Ctrl+Enter** để gửi (song song Alt+Enter
  cũ), **Cmd/Ctrl+S** để lưu request, **Escape** hủy đang đổi tên (rename)
  dở dang.

---

## Tổng kết hạ tầng kỹ thuật
- **101 unit test** (từ 7 ban đầu), cover storage round-trip, auth signing
  (AWS SigV4 + RFC 2617 Digest theo test vector chuẩn), import/export,
  dynamic variables, scripting API, settings/cookie persistence, saved
  examples round-trip, multi-tab concurrency, Collection Runner, code
  generation (kể cả chạy thật lệnh curl sinh ra để xác nhận đúng), theme +
  command palette.
- **Snapshot test** headless (`egui_kittest`) cho từng phase UI, ảnh lưu ở
  `tests/snapshots/`, dùng để tự kiểm tra không cần chạy app thật — kể cả
  snapshot đầu tiên ở chế độ sáng (light theme).
- Lưu trữ: mỗi request/folder/collection là 1 file JSON riêng (không còn
  1 file `data.json` khổng lồ), tự động migrate dữ liệu cũ.

## Phase 12 — Testing & CI hardening ✅
- Chạy `cargo fmt` toàn bộ codebase (thuần format, không đổi hành vi — xác
  nhận bằng bộ test chạy y hệt trước/sau).
- Sửa hết 46 cảnh báo `cargo clippy` (toàn bộ là gợi ý style/idiom, không có
  bug logic nào).
- Thêm job **lint** riêng vào CI (`cargo fmt --check` + `cargo clippy -D
  warnings`), chạy song song với job build 3 hệ điều hành sẵn có.
- Xác minh cục bộ: 101 test pass, 0 cảnh báo clippy, 0 chênh lệch format.
  Việc đẩy code lên để thấy CI chạy xanh thật trên GitHub Actions cần lệnh
  `git push` — mình chưa tự làm, đợi bạn xác nhận.

Đến đây là **hoàn tất toàn bộ 12 phase trong roadmap**.

## Phase 13 — Rà soát tính năng còn thiếu, đợt 1: các việc nhỏ/nhanh ✅
- **Mô tả (description)** cho request/folder/collection — hiện ở đầu tab
  Params (request), và trong màn hình edit riêng (folder/collection, mở
  bằng menu chuột phải → "Edit"). Round-trip đầy đủ qua Postman Collection
  v2.1 (`info.description` và `description` từng item).
- **Pre-request & Tests script ở cấp folder/collection** (trước đây chỉ
  chạy được script ở cấp request) — script chạy theo thứ tự: collection →
  từng folder (ngoài vào trong) → request, dùng chung 1 `env`/`globals`/
  `collectionVariables` xuyên suốt, nên script ở collection `pm.environment.set(...)`
  thì script ở request phía sau đọc được ngay. Áp dụng cho cả gửi request
  thường lẫn Collection Runner.
- **Timeout riêng cho từng request** (field "Timeout (ms)" cạnh nút Send) —
  để trống thì dùng mặc định của HTTP client như trước giờ.
- **Save Response to File** byte-chính-xác — nút "Save Response" trong màn
  hình response, ghi đúng bytes gốc nhận được (không qua bước chuyển thành
  text rồi mất mát), xác nhận bằng cả unit test lẫn tải thật một file qua
  mạng rồi so khớp bytes.
- **Bulk Edit** cho các bảng key-value (Params/Headers/Form/biến môi
  trường) — nút "Bulk Edit" chuyển bảng thành 1 ô text, mỗi dòng
  `key: value`, dòng bị tắt thì có `//` phía trước — giống hệt kiểu Postman.
- Hai việc trong danh sách ban đầu, sau khi tìm hiểu kỹ hóa ra **không hề
  nhỏ**, nên tách riêng chứ không nhét vào phase này:
  - **Ẩn giá trị bí mật (secret) + Current/Initial Value** cho biến môi
    trường/collection/global — cần một kiểu dữ liệu mới (khác `KeyValue`
    đang dùng ở ~50 chỗ) và một màn hình sửa biến hoàn toàn mới (hiện chưa
    có màn hình nào để sửa biến collection/global cả) → đề xuất làm
    **Phase 14** riêng, chưa bắt đầu.
  - **Chứng chỉ client theo từng domain** — đề xuất **bỏ hẳn**, vì
    `reqwest::Client` cố định chứng chỉ lúc khởi tạo, muốn hỗ trợ nhiều
    domain cần nuôi cả một nhóm client riêng — không đáng để đổi kiến trúc
    cho một nhu cầu ít gặp với app desktop.
- **108 unit test** (từ 101) — thêm test round-trip cho field mới, test
  xác nhận script cấp collection/folder chạy đúng thứ tự và chia sẻ biến,
  test round-trip Bulk Edit, và test mạng thật xác nhận bytes lưu file
  khớp chính xác với response nhận được.
- 3 snapshot mới (`egui_kittest`): màn hình edit folder (mô tả + 2 script
  tab), chế độ Bulk Edit, và tiện thể xác nhận field mô tả hiển thị đúng
  trong request editor — không gặp lỗi thiếu icon nào.
