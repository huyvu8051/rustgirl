# RustGirl — Danh sách tính năng đã hoàn thành

> Cập nhật theo tiến độ roadmap "100% Postman parity". Xem plan đầy đủ tại
> `~/.claude/plans/playful-snuggling-orbit.md`. File này chỉ liệt kê tính
> năng đã xong.

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

## Phase 15 — Điều hướng & quan sát: ảo hóa cây request, phím tắt kiểu
AeroSpace, command palette leader-key, tab Console ✅
- **Sửa lỗi thật do người dùng báo**: "Failed to build request: builder
  error" khi URL dùng `{{baseUrl}}` — giờ báo rõ ràng "biến chưa được thay
  thế" hoặc "URL sau khi thay biến không hợp lệ" thay vì lỗi mù mờ của
  reqwest. Tiện thể sửa luôn 2 bug thật phát hiện khi import 21 collection
  Postman thật của người dùng: script bị đè mất khi có ≥2 event cùng loại,
  và script rỗng để trống trong Postman bị báo nhầm "cần chuyển tay".
- **Ảo hóa cây collection** (`show_rows`) — cây được làm phẳng thành danh
  sách 1 chiều rồi chỉ render các dòng đang hiển thị trên màn hình, hết lag
  khi mở folder có hàng trăm request. "Expand all"/"Collapse all" giờ thao
  tác trực tiếp trên state của app thay vì dựa vào bộ nhớ nội bộ của egui.
  Các nút hành động (thêm request/folder, xóa, export, run) chuyển hết vào
  menu chuột phải để tương thích với việc render theo dòng cố định chiều
  cao — vẫn 1 click, chỉ đổi chỗ.
- **Phím tắt gán request kiểu macOS AeroSpace**: chuột phải vào request →
  "Assign hotkey…" → bấm 1 phím bất kỳ trong `0-9`/`a-z` (trừ `h j k l` —
  dành riêng cho điều hướng tab) để gán. Sau đó **Option+phím đó** ở bất
  kỳ đâu trong app sẽ mở ngay tab request đó. Có badge nhỏ hiện phím đã gán
  cạnh tên request trong cây. Người dùng cũ đang dùng Alt+1..9 cho "Saved
  Requests" được tự động migrate sang hệ thống mới, không mất phím tắt cũ.
- **Option+H / Option+L** để chuyển qua lại giữa các tab đang mở (giống
  `alt-h`/`alt-l` của AeroSpace) — `j`/`k` cố tình chưa gán, để dành làm gì
  đó sau.
- **Command palette đổi cách mở**: thay Alt+Space/Cmd+K bằng chuỗi phím
  kiểu neovim — khi không có ô nào đang focus, gõ `Space` rồi `s` rồi `f`
  (`<leader>sf`) sẽ mở palette. Gõ sai phím giữa chừng thì tự hủy; để quá
  600ms không gõ tiếp cũng tự hủy.
- **Ctrl+N / Ctrl+P** để di chuyển highlight lên/xuống trong palette (kiểu
  Emacs), có vòng lặp khi tới đầu/cuối danh sách.
- **Fuzzy search chuẩn fzf**: thay tìm kiếm theo kiểu "chứa chuỗi con" bằng
  `nucleo-matcher` (cùng thuật toán fzf) — gõ tắt không liền nhau (ví dụ
  "crq" ra "Create Request") vẫn tìm ra, và kết quả xếp hạng theo độ khớp
  thay vì theo thứ tự có sẵn.
- **Tab Console kiểu Postman** + **ghi log ra file**: mỗi lần gửi request
  (thành công hay lỗi) đều hiện trong tab Console (status, thời gian,
  console.log từ script, số test pass/fail) và đồng thời ghi 1 dòng dễ đọc
  vào file `console.log` trên đĩa — để troubleshoot cả sau khi đóng app.
  Đường dẫn file log hiện ngay trong tab Console.
- **Dialog import file giờ chọn được nhiều file cùng lúc** (Postman
  collection, OpenAPI spec, Postman environment) — import hàng loạt trong
  1 lần thay vì phải mở dialog lại từng file, kết quả từng file được tóm
  tắt chung trong 1 dòng thông báo.
- Chưa làm (ghi nhận riêng, chưa nằm trong phạm vi phase này): nút tắt/mở
  sidebar tự động thu gọn khi cửa sổ hẹp lại, và Ctrl+I/Ctrl+O để nhảy tới
  lui giữa lịch sử các tab đã mở kiểu Vim jumplist.
- **125 unit test** (từ 108) — thêm test cho việc làm phẳng cây theo từng
  mức mở, round-trip phím tắt, migration Alt+1..9 cũ, ghi console log, và
  xếp hạng fuzzy match.

## Phase 16 — Nút tắt/mở sidebar, jumplist tab kiểu Vim ✅
- **Nút "Hide Sidebar"/"Show Sidebar"** ở top bar — bấm để ẩn/hiện sidebar
  bên trái bất cứ lúc nào.
- **Tự động ẩn sidebar khi cửa sổ hẹp lại** (dưới ~640px), và **tự động
  hiện lại** khi mở rộng cửa sổ ra — nếu tự tay bấm mở lại sidebar lúc đang
  hẹp thì nó không bị tự đóng lại nữa (chỉ tự động can thiệp lúc chuyển từ
  rộng sang hẹp hoặc ngược lại, không phải mỗi frame).
- **Ctrl+I / Ctrl+O** để nhảy tới lui giữa lịch sử các tab đã từng mở, giống
  jumplist của Vim — không chỉ là cycle tuần tự như Ctrl+Tab, mà đi theo
  đúng thứ tự các tab thực sự đã ghé qua, kể cả khi quay lại rồi mở tab mới
  (lịch sử "đi tiếp" phía trước bị xóa, giống nút back/forward trình
  duyệt). Nếu 1 tab trong lịch sử đã bị đóng thì tự động bỏ qua, không bị
  kẹt.
- **Dialog import chọn nhiều file cùng lúc** (Postman collection, OpenAPI,
  Postman environment) — import hàng loạt trong 1 lần, mỗi file báo kết
  quả riêng trong 1 dòng thông báo chung.
- **128 unit test** (từ 125) — thêm test cho logic jumplist (đi tới/lui,
  dừng ở 2 đầu, bỏ qua tab đã đóng) và test tự động ẩn/hiện sidebar theo độ
  rộng cửa sổ.

## Phase 17 — Refactor giao diện giống Postman hết mức ✅
- **Màu cam thương hiệu của Postman** áp dụng toàn app: màu chọn/hover/focus
  và nút Send đổi sang cam, viền tab đang mở cũng viền cam — áp dụng đồng
  thời cho cả theme Sáng/Tối/System qua 1 lần chỉnh style dùng chung.
- **Bo góc mềm hơn** cho nút/ô nhập — giống độ bo góc thật của giao diện
  Postman thay vì góc gần vuông mặc định của egui.
- **Màu theo method (GET/POST/PUT/...)** giờ hiện khắp nơi thay vì chỉ ở
  tab đang mở: trong cây collection, trong History, trong command palette,
  và trong bảng kết quả Collection Runner.
- **Màu status code tách rõ 4 mức** (2xx xanh lá, 3xx xanh dương, 4xx cam,
  5xx đỏ) thay vì chỉ 3 mức cũ gộp chung 4xx/5xx làm một màu đỏ.
- **Thứ tự tab request** đổi thành Params → Authorization → Headers →
  Body → Pre-request Script → Tests → Code, giống đúng thứ tự thật của
  Postman (trước đây Auth nằm sau Headers/Body). Tab "Auth" đổi tên thành
  "Authorization" cho khớp nhãn thật của Postman.
- **Đếm số dòng đang bật ngay trên tên tab**: "Params (2)", "Headers (3)"
  — giống hệt kiểu Postman, cùng quy ước với "Tests (đã pass/tổng)" có sẵn.
- **Thứ tự tab response** đổi thành Body → Cookies → Headers → Tests →
  Request → Diff (2 tab cuối là phần mở rộng riêng của app, không có ở
  Postman nên xếp sau cùng).
- **Sidebar có màu nền hơi tối hơn** một chút so với vùng nội dung chính —
  tạo cảm giác đây là 1 vùng "khung" riêng biệt giống Postman thật, thay vì
  phẳng lì như trước.
- **Console chuyển thành khay kéo ở đáy cửa sổ** giống Postman thật (trước
  đây Console chiếm toàn bộ màn hình giữa, che mất request/response) — giờ
  bấm nút "Console" chỉ mở/đóng 1 khay nhỏ ở dưới cùng, request/response
  vẫn hiển thị bình thường phía trên, có nút "×" đóng riêng trong khay.
- **Sửa lỗi tràn giao diện khi import nhiều file cùng lúc**: import ít file
  thì vẫn báo chi tiết từng file như trước; import nhiều file thì gộp lại
  thành 1 dòng ngắn gọn ("Imported 10 files successfully.") và chỉ liệt kê
  chi tiết những file bị lỗi. Dòng thông báo giờ tự xuống dòng, giới hạn
  chiều cao, và có nút "×" để tắt hẳn đi thay vì cứ nằm lì mãi.
- **133 unit test** (từ 130) — thêm test cho các mức màu status/method, và
  test cho logic gộp thông báo import hàng loạt.

## Phase 17 follow-up — icon cây thư mục, chuột phải, phím tắt bằng bàn phím ✅
- **Icon mở/đóng thật** thay cho chữ "v"/">" — vẽ tam giác thật bằng chính
  hàm vẽ nội bộ của egui, không phải font glyph, nên không bao giờ bị lỗi
  ô vuông trống (tofu box) như các icon Unicode từng gặp trước đây.
- **Nút "+" thêm request** hiện lại trên mỗi dòng collection/folder — bị
  mất từ đợt viết lại cây ảo hóa, giờ có lại cùng lúc với menu chuột phải.
- **Sửa lỗi chuột phải không có tác dụng gì cả** — đây là lỗi thật, có từ
  rất lâu (không phải do vừa làm hỏng): egui coi vùng kéo-thả (drop zone)
  của mỗi dòng chỉ "cảm nhận" hover, không cảm nhận click, nên chuột phải
  không bao giờ được ghi nhận. Lần sửa đầu tiên (ép vùng đó cảm nhận click)
  lại gây ra lỗi mới — nó tranh mất click với icon mở/đóng và nút tên bên
  trong, làm hỏng luôn cả việc mở/đóng cây (phát hiện ngay vì 1 test cũ tự
  nhiên fail, không phải đoán). Sửa đúng cách: đọc thẳng trạng thái chuột
  phải thay vì đăng ký thêm 1 widget cảm nhận click chồng lên — không còn
  tranh chấp. Có test giả lập chuột phải thật để đảm bảo không tái diễn.
- **`space` → `a` → `h`**: gán phím tắt cho request đang mở (tab đang active)
  ngay bằng bàn phím, không cần chuột phải — banner màu vàng ở trên giờ ghi
  rõ tên request đang được gán ("...cho "Get Users"...") thay vì chữ chung
  chung.
- **`space` → `s` → `e`**: mở nhanh command palette, tự lọc sẵn theo
  "Environment:" để chọn environment ngay bằng bàn phím.
- **Badge phím tắt trên tab đang mở**: tab nào có request đã gán phím tắt
  thì hiện luôn "[phím]" ngay trên chip của tab đó, giống hệt badge đã có
  trong sidebar — trước đây chỉ thấy trong sidebar, mở request ra thì mất.
- Thêm 6 test tương tác thật (`egui_kittest`, chạy bằng `cargo test --
  --ignored` vì cần GPU): click chuột phải thật để xác nhận menu context
  mở đúng, gõ phím leader-chord thật cho cả 2 tổ hợp mới, và snapshot cho
  banner gán phím tắt + badge phím tắt trên tab.
- **`space` → `u` → `s`**: tắt/mở sidebar nhanh bằng bàn phím, tương đương
  hệt nút "Hide/Show Sidebar" ở top bar (dùng chung 1 hàm nên hành vi giống
  nhau 100%, kể cả việc không bị tự đóng lại khi cửa sổ đang hẹp).
- **`space` → `s` → `f` giờ chỉ tìm request, không tìm command nữa** — vì
  giờ đã có phím riêng cho từng lệnh hay dùng (`se`/`ah`/`us`), nên "sf"
  quay về đúng nghĩa "tìm request" (search files) thay vì trộn chung với
  danh sách lệnh, kể cả khi từ khóa gõ vào tình cờ khớp cả tên 1 request
  lẫn 1 lệnh nào đó.
- **Sửa lỗi Ctrl+N/P không tự cuộn trong ô tìm kiếm** — trước đây bấm
  Ctrl+N nhiều lần, ô được chọn cứ trôi xuống dưới mà danh sách không tự
  cuộn theo, nên chọn ra ngoài màn hình lúc nào không biết. Giờ danh sách
  tự cuộn để luôn thấy dòng đang chọn, nhưng chỉ cuộn đúng lúc vừa bấm
  Ctrl+N/P (không cuộn liên tục mỗi frame) để không phá việc tự cuộn tay
  bằng chuột.
