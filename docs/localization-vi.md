# LightCraft bằng tiếng Việt

Chọn **Chỉnh sửa → Ngôn ngữ → Tiếng Việt** hoặc **Cài đặt → Chung → Ngôn ngữ**.
Lựa chọn áp dụng ngay và được lưu cho lần mở sau (`language: "vi"` trong `ui.json`).

Các tag locale hệ thống sau cũng chọn tiếng Việt: `vi`, `vi-VN`, `vi_VN`, `vi_VN.UTF-8`.

## Phạm vi

Catalog tiếng Việt phủ **toàn bộ** bề mặt localization hiện tại của LightCraft
(đo 2026-10-13: union plain **2 222** + formats **249** = **2 471** thông điệp):

- `crates/ui-egui/locales/vi.json` — thông điệp plain / nhãn hiển thị (2 222 khóa)
- `crates/ui-egui/locales/vi-formats.json` — thông điệp có giá trị runtime và mẫu ngày (249 khóa)

Điều này là **100% bề mặt localizable/runtime hiện tại**, không phải “mọi chuỗi tiếng Anh
trong toàn bộ repository”. Lỗi kỹ thuật từ crate tầng dưới vẫn có thể là tiếng Anh, giống
các ngôn ngữ khác.

## Quy ước thuật ngữ

| English | Vietnamese |
|---|---|
| File / Folder | Tệp / Thư mục |
| Library / Album / Smart Album | Thư viện / Album / Album thông minh |
| Import / Export | Nhập / Xuất |
| Exposure / Contrast | Phơi sáng / Tương phản |
| Highlights / Shadows / Whites / Blacks | Vùng sáng / Vùng tối / Vùng trắng / Vùng đen |
| White Balance / Temperature / Tint | Cân bằng trắng / Nhiệt độ / Tông |
| Vibrance / Saturation | Độ rực rỡ / Độ bão hòa |
| Texture / Clarity / Dehaze / Grain / Vignette | Kết cấu / Độ rõ / Khử sương mù / Hạt / Tối góc |
| Tone Curve | Đường cong tông màu |
| Sharpening / Noise Reduction / AI Denoise | Làm nét / Giảm nhiễu / Khử nhiễu AI |
| Masking / Brush / Linear Gradient | Mặt nạ / Cọ / Chuyển sắc tuyến tính |
| Preset / Profile / History / Version | Thiết lập sẵn / Hồ sơ / Lịch sử / Phiên bản |
| Appearance (theme) | Giao diện |
| Subject (AI) | Chủ thể |
| Candidate / Select | Ảnh ứng viên / Chọn |
| Flag / Picked / Rejected / Unflagged | Gắn cờ / Đã chọn / Đã loại / Không gắn cờ |
| Shutter Speed | Tốc độ màn trập |
| Smart Previews | Xem trước thông minh |

Tên sản phẩm và chuẩn giữ nguyên khi là định danh: LightCraft, ArtCraft, GitHub, Discord,
JPEG, PNG, TIFF, WebP, AVIF, HEIF, HEIC, RAW, DNG, RGB, HDR, ICC, EXIF, XMP, GPU, MCP, SAM, ISO, OK.

Tên album / preset / profile do người dùng tạo, tên tệp, command ID và metadata nhập tay
không được dịch.

## Ngày và số lượng

- Ngày đầy đủ: `{weekday}, {day}/{month}/{year}` (ví dụ `Chủ Nhật, 20/9/2026`)
- Tháng/năm: `Tháng {month} năm {year}`
- Thứ viết tắt trên date picker: T2…T7, CN
- Thông điệp đếm dùng dạng trung tính (`22 ảnh`), không tạo số ít/số nhiều nhân tạo như tiếng Anh

## Font

Tiếng Việt dùng Inter (Latin + dấu). Không cần craft-fonts CJK. Kiểm tra glyph trong
`vietnamese_panels_dialogs_and_diacritics_paint_with_bundled_fonts`.

## Kiểm tra

```sh
cargo test -p lightcraft-ui-egui i18n::tests -- --nocapture
LIGHTCRAFT_LANGUAGE=vi lightcraft-cli snapshot --demo -o vi.png --size 1600x1000
LIGHTCRAFT_LANGUAGE=vi lightcraft-cli snapshot --demo -o vi-narrow.png --size 1280x800
```

Các test tiếng Việt bắt buộc phủ catalog union, nhãn lệnh/control/Settings/Smart Album,
đổi ngôn ngữ, lưu `vi`, formats, ngày, dữ liệu người dùng, và glyph dấu.

## Nguồn dịch (clean-room)

Dịch từ chuỗi tiếng Anh nguồn của LightCraft. PhotoCraft / VectorCraft / PDFCraft chỉ dùng
làm bộ nhớ thuật ngữ khi cùng nguồn tiếng Anh và cùng ngữ cảnh UI. Không dùng gói ngôn ngữ
Adobe Lightroom hay tài nguyên độc quyền.
