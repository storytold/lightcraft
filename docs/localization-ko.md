# LightCraft 한국어

[English README](../README.md) | [한국어 README](../README.ko.md)

**Edit → Language → 한국어** 또는 **Settings → General → Language**에서 한국어를
선택합니다. 한국어 화면에서는 **편집 → 언어 → 한국어**로 표시됩니다.
선택한 언어는 `ui.json`에 `"language": "ko"`로 저장되며, 다음 실행에도 유지됩니다.
다른 언어로 돌아갈 때도 같은 메뉴를 사용합니다. 시스템 언어가 한국어라고 해서 자동으로
한국어로 바뀌지는 않습니다. `LIGHTCRAFT_LANGUAGE=ko`는 설정이 없는 실행이나 headless
화면 검증에 사용할 수 있습니다.

## 번역 기준

한국어로 읽기 편한 사진 편집 UI를 목표로 합니다. 영어를 단어마다 옮기거나 모든 용어를
한글로 바꾸지는 않습니다. 메뉴와 버튼은 짧게, 안내와 오류는 다음에 할 일을 알 수 있게 씁니다.

| 원문 | 화면 표시 | 기준 |
|---|---|---|
| Import / Export | 가져오기 / 내보내기 | 일상적인 작업은 한국어로 |
| Settings / Language | 설정 / 언어 | 설정을 쉽게 찾을 수 있도록 |
| Light / Exposure / Contrast | 밝기 / 노출 / 대비 | 사진 편집 문맥에 맞는 표현 |
| White Balance / Preset | 화이트 밸런스 / 프리셋 | 널리 쓰이는 용어 유지 |
| Color Mixer / Color Grading | 색상 믹서 / 컬러 그레이딩 | 사진 편집에서 익숙한 용어 |
| Amount / Long Edge / Short Edge | 강도 / 긴 변 / 짧은 변 | 조작과 크기 설정의 의미가 드러나도록 |
| Undo / Redo / Survey | 실행 취소 / 다시 실행 / 모아 보기 | 기능을 설명하는 자연스러운 표현 |
| AI Denoise, RawNIND, SAM 3 | 원문 유지 | 기능·모델 이름을 검색하기 쉽게 |
| RAW, JPEG, ISO, HDR, EXIF, XMP, ICC, GPU, CPU, sRGB, ONNX | 원문 유지 | 파일 형식·표준·약어 |
| Golden Hour, Warm Glow 등 | 원문 유지 | 기본 creative 프리셋 이름의 식별성 유지 |

파일명, 사용자가 만든 앨범·프리셋 이름, 명령 ID와 단축키는 바꾸지 않습니다.
날짜는 `2026년 9월 20일 일요일`, 사진 수는 `사진 3장`처럼 표시합니다.
사진 처리와 렌더링 설정은 언어를 바꿔도 달라지지 않습니다.

## 글꼴과 빌드

한글은 [craft-fonts](https://github.com/storytold/craft-fonts)의 **NanumGothic Regular**로
표시합니다. NHN Corporation과 Sandoll Communications의 SIL OFL-1.1 글꼴로,
LightCraft의 고정 폰트 버전 `b076417b5880bd4154bd22f00695b1d28c6bea83`에 이미 포함되어 있습니다.
글꼴 파일을 이 저장소에 복사하지 않습니다. 네이티브와 WebAssembly 빌드 모두 이 한글
글꼴을 포함하며, 배포용 `CRAFT_FONTS_REQUIRED=1` 빌드에서는 누락을 오류로 처리합니다.
무료로 사용할 수 있고 앱에 포함해 배포할 수도 있습니다. 배포 시 저작권 고지와
[OFL 라이선스](https://github.com/storytold/craft-fonts/blob/b076417b5880bd4154bd22f00695b1d28c6bea83/fonts/nanum-gothic/OFL.txt)를
함께 보존해야 합니다. 영문과 숫자는 기존 Inter를 사용합니다.
`cargo xtask web`의 `web-site/`에도 `OFL-nanum-gothic.txt`를 포함한 글꼴 라이선스를
복사합니다. 웹사이트를 배포할 때는 `OFL-*.txt` 파일도 함께 올려야 합니다.

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
git -C ../craft-fonts checkout b076417b5880bd4154bd22f00695b1d28c6bea83
CRAFT_FONTS_DIR=../craft-fonts CRAFT_FONTS_REQUIRED=1 cargo run -p lightcraft
```

`CRAFT_FONTS_DIR` 없이 빌드하면 앱은 실행되지만 한글이 네모로 표시될 수 있습니다.
이는 번역 설정만으로 해결되지 않으므로 글꼴을 포함해 다시 빌드해야 합니다.

회색 글씨가 흐리게 보인다면 **설정 → 인터페이스 → 다크 테마 → 미드나이트(Midnight)**를
선택해 보세요. 기본 Charcoal보다 배경이 어두워 보조 글씨의 대비가 높습니다.
제어 채널의 `ui.zoomFactor`에 `{"factor": 1.25}`를 주면 UI를 125%로 확대해
비교할 수 있습니다. 이 배율은 현재 실행에만 적용되며, 저장되는 설정은 아닙니다.
사진 자체의 확대율이나 내보내기 크기는 바뀌지 않습니다.

## 범위와 유지보수

`crates/ui-egui/locales/ko.json`은 일반 문구, `ko-formats.json`은 값·개수·날짜가
들어가는 문구입니다. 영어 키는 그대로 두고 값만 수정합니다. 새 영문 문구나 번역하지 않은
하위 계층의 기술 오류, What's New는 영어로 표시됩니다. 원문과 같은 약어·제품명이 있다고
해서 누락된 번역은 아닙니다. 전체 문구의 한국어 사용자 교정은 아직 필요합니다.

초기 카탈로그는 yunbin (@sksdl)의 [PR #1728](https://github.com/storytold/lightcraft/pull/1728),
커밋 `7f2624dc2fca19e0a4712368bb666c2eff0c36da`에서 가져왔습니다. @FriOct가 Codex와 함께
표현을 다듬고 최신 UI 문구를 추가했습니다. 현재 일반 문구 2,547개와 format 문구 307개를
포함합니다. 이 수는 영어로 유지한 항목과 이전 버전의 키도 포함한 **카탈로그 항목 수**이며,
모든 오류와 릴리스 노트까지 번역했다는 뜻은 아닙니다. 출처·라이선스는
[`assets/ATTRIBUTION.md`](../assets/ATTRIBUTION.md)에 기록합니다.

```sh
CRAFT_FONTS_DIR=../craft-fonts cargo test --workspace i18n::tests --lib
CRAFT_FONTS_DIR=../craft-fonts cargo xtask ci
```

검사는 언어 메뉴와 설정 저장·복원, `ko-KR` 등 언어 태그, 한국어 날짜와 개수 표현,
고유 이름 보존, 모든 카탈로그의 placeholder와 실제 한글 glyph를 확인합니다.
번역 누락은 `i18n::tests::translation_gaps_report --nocapture`로 확인할 수 있습니다.
