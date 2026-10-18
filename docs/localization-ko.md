# LightCraft in Korean

Choose **Edit → Language → 한국어** or **Settings → General → Language**. The choice applies at once
and is saved for the next launch (`language: "ko"` in `ui.json`); the control command is
`app.language.korean`. `LIGHTCRAFT_LANGUAGE` selects it for headless runs and accepts `ko`, `ko-KR`
and `ko_KR.UTF-8`; the saved value is always `ko`. LightCraft does not follow the system language,
for Korean as for every other language: a Korean macOS, Windows or Linux still starts in English until
Korean is chosen.

The Korean catalogs cover every plain and formats message the other catalogs have, plus every command,
control and smart-album rule label: menus, shortcuts, library, editing, crop, masks, people,
import/export, slideshow, print, settings, built-in presets and profiles, history, versions, date
headings, status messages and dialogs. The embedded release notes (Help → What's New) stay in
English, as in Japanese and Chinese, and so do technical errors from lower layers. The text was
written from the meaning of the English labels; the Japanese catalog was consulted only for message
context and placeholder structure, and no proprietary translation resources were used.

File names, command ids, template placeholders and albums, presets or metadata you named yourself are
not translated.

## Korean text

Sentences use the formal 합니다체 (`…했습니다`, `…하시겠습니까?`); menus, buttons and slider names are
short noun phrases. Counts put the noun first and use a counter: photos `장` (`사진 3장을
가져왔습니다`), items `개`, jobs and errors `건`. English plural endings are dropped with `{:.0}`.
After a placeholder the sentence is reworded to avoid a particle that depends on the value; where that
is impossible both forms are written (`을(를)`). Dates read `2026년 9월 20일 일요일`, months
`2026년 9월`. The menu triangles `▸` / `▾`, which the interface cannot paint, are written `→` / `▼`.

Inter has no hangul and craft-fonts has no Korean face yet (Noto Sans CJK is allowed only for
Chinese), so Korean text shows boxes until a non-Adobe OFL Korean face is available to the UI.
Proposals are open in craft-fonts (storytold/craft-fonts#25, #28, #29, #30).

## Terminology

| Source | Korean |
|---|---|
| Library / Develop | 라이브러리 / 현상 |
| File / Edit / View / Window / Help | 파일 / 편집 / 보기 / 윈도우 / 도움말 |
| Settings / General / Language | 설정 / 일반 / 언어 |
| Import / Export | 가져오기 / 내보내기 |
| Photos / Albums / Folders / Smart Album | 사진 / 앨범 / 폴더 / 스마트 앨범 |
| Pick / Reject / Rating / Label | 채택 / 제외 / 등급 / 레이블 |
| Crop / Straighten | 자르기 / 수평 맞추기 |
| Masks / Masking | 마스크 / 마스킹 |
| Heal / Clone / Remove | 복구 / 복제 / 제거 |
| Presets / Profile | 사전 설정 / 프로필 |
| History / Versions | 작업 내역 / 버전 |
| Light / Color / Effects / Detail | 조명 / 색상 / 효과 / 세부 |
| Exposure / Contrast | 노출 / 대비 |
| Highlights / Shadows / Whites / Blacks | 밝은 영역 / 어두운 영역 / 흰색 계열 / 검정 계열 |
| Texture / Clarity / Dehaze | 텍스처 / 부분 대비 / 디헤이즈 |
| Vibrance / Saturation | 생동감 / 채도 |
| Temp / Tint | 색온도 / 틴트 |
| Hue / Luminance | 색조 / 광도 |
| Tone Curve / Color Mixer / Color Grading | 톤 곡선 / 색상 혼합 / 컬러 그레이딩 |
| Sharpening / Noise Reduction | 선명하게 하기 / 노이즈 감소 |
| Lens Corrections / Geometry | 렌즈 교정 / 기하 |
| Vignetting / Grain | 비네팅 / 그레인 |
| Smart Preview / Survey / Stack | 스마트 미리 보기 / 통람 / 스택 |
| Slideshow / Print | 슬라이드 쇼 / 인쇄 |
| Reset / Undo / Redo | 재설정 / 실행 취소 / 실행 복귀 |

Please follow this table when you add strings.

## Maintenance and checks

Fixed texts: `crates/ui-egui/locales/ko.json` (2,330 entries). Messages with values:
`crates/ui-egui/locales/ko-formats.json` (297 entries, every format message). The English keys stay
unchanged; Rust checks the placeholders at build time. Strings not yet in the plain catalog show in
English.

```sh
cargo test -p lightcraft-ui-egui i18n::tests -- --nocapture
LIGHTCRAFT_LANGUAGE=ko lightcraft-cli snapshot --demo -o korean.png --size 1600x1000
```

The tests check catalog coverage, placeholders, switching language by menu and control channel,
saving, the `ko` locale tags, dates and count wording. More notes are in
[localization.md](localization.md).
