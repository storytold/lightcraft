# Tiếng Việt

Tiếng Việt (`vi`, English name **Vietnamese**, native name **Tiếng Việt**) is selected via
**Chỉnh sửa → Ngôn ngữ → Tiếng Việt** (**Edit → Language → Tiếng Việt**) or **Cài đặt → Chung →
Ngôn ngữ** (**Settings → General → Language**). The change takes effect immediately and persists
across restarts (`"language": "vi"` in the UI preferences file). The UI command is
`app.language.vietnamese`; the control channel accepts `ui.set {"language":"vi"}` as well as system
locale tags `vi-VN`, `vi_VN.UTF-8` and `vi-Latn-VN`. Headless runs accept `LIGHTCRAFT_LANGUAGE=vi`.

The catalogs `crates/ui-egui/locales/vi.json` and `crates/ui-egui/locales/vi-formats.json` are
original, meaning-based translations from the English source strings, reusing and adapting matching
terminology from the PhotoCraft, VectorCraft, WordCraft and PdfCraft Vietnamese catalogs where the
meaning fits a photo library and raw development app.

## Conventions and scope

- Top-level menus: **Tệp** (File), **Chỉnh sửa** (Edit), **Xem** (View), **Ảnh** (Photo),
  **Cửa sổ** (Window), **Trợ giúp** (Help).
- Core concepts: **Thư viện** (Library), **Chỉnh ảnh** (Develop), **Album** / **Album thông minh**
  (Album / Smart Album), **Chồng ảnh** (Stack), **Bản sao ảo** (Virtual Copy), **Thiết lập sẵn**
  (Preset), **Hồ sơ** (Profile), **Dải phim** (Filmstrip).
- Tone & colour: **Phơi sáng** (Exposure), **Độ tương phản** (Contrast), **Vùng sáng** (Highlights),
  **Vùng tối** (Shadows), **Vùng trắng** (Whites), **Vùng đen** (Blacks), **Cân bằng trắng**
  (White Balance), **Nhiệt độ màu** (Temperature), **Sắc thái** (Tint), **Độ sống động** (Vibrance),
  **Độ bão hòa** (Saturation), **Độ rõ nét** (Clarity), **Khử sương mù** (Dehaze), **Tối góc**
  (Vignetting), **Hạt** (Grain).
- Dates use `DD/MM/YYYY` (`Chủ nhật, 20/09/2026`, `09/2026`) and 24-hour time (`16:04:05`).
- Vietnamese nouns do not inflect for grammatical number, so count templates use natural
  numeral + classifier/noun phrasing (`Đã nhập {0} ảnh{1:.0}`, `{n} ảnh đã lưu đệm`) and consume
  the English plural suffix with `{:.0}`.
- All precomposed Vietnamese Latin characters and stacked diacritics are covered directly by the
  bundled `Inter-Regular.ttf` and `Inter-SemiBold.ttf` fonts (`Latn` script); no extra font file is
  required.
- Product and format names (`LightCraft`, `PhotoCraft`, `ArtCraft`, `Lightroom`, `Apple Photos`,
  `Apple ProRAW`, `SAM 3`, `ONNX`, `GeoNames`, `OpenStreetMap`, `RAW`, `DNG`, `JPEG`, `JPEG XL`,
  `PNG`, `TIFF`, `AVIF`, `HEIC`, `PSD`, `XMP`, `EXIF`, `GPS`, `ICC`, `LUT`, `sRGB`, `Display P3`,
  `Rec. 2020`, `PQ`, `HLG`, `SDR`, `HDR`, `ISO`, `EV`, `Upright`, `Guided Upright`) remain in
  English.

The plain catalog translates 2,402 interface strings (every non-What's-New catalog message, command
label, Develop control, Settings tab, Smart Album rule field/operator/choice/issue, date-picker
heading and rename token/note), and `vi-formats.json` translates all 298 runtime format templates.
What's New release-note prose and lower-layer technical error strings fall back to English.
