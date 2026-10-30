<p align="center">
  <a href="https://getartcraft.com/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="docs/brand/artcraft-logo-white.svg">
      <img alt="ArtCraft" src="docs/brand/artcraft-logo.svg" width="200">
    </picture>
  </a>
</p>

<p align="center">
  <img src="assets/app-icon/hicolor/256x256/apps/ai.storyteller.lightcraft.png" width="128" height="128" alt="호박색 타일에 스라소니가 새겨진 LightCraft 아이콘">
</p>

<h1 align="center">LightCraft</h1>

<h3 align="center">내 사진을, 내 컴퓨터에서, 내 방식으로.</h3>

<p align="center">
  <a href="README.md">English</a> |
  <a href="README.ko.md">한국어</a> |
  <a href="README.zh-hans.md">简体中文</a> |
  <a href="README.zh-hant.md">繁體中文</a>
</p>

<p align="center">
  <b>사진 관리와 RAW 현상을 위한 오픈소스 앱. 원본을 보존하는 비파괴 편집, 순수 Rust로 구현했습니다.</b><br>
  macOS·Windows·Linux에서 네이티브 앱으로, WebAssembly를 통해 브라우저에서도 실행됩니다. AI 에이전트는 MCP로 앱을 직접 조작할 수 있습니다.
</p>

<p align="center">
  <img alt="Pure Rust" src="https://img.shields.io/badge/pure-Rust-f2a516?style=flat-square&logo=rust&logoColor=white">
  <img alt="macOS, Windows, Linux and Web" src="https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux%20%C2%B7%20Web-8a5800?style=flat-square">
  <img alt="MCP server included" src="https://img.shields.io/badge/MCP-ready-8a5800?style=flat-square">
  <img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-8a5800?style=flat-square">
  <a href="ROADMAP.md"><img alt="Status: young and moving fast" src="https://img.shields.io/badge/status-young%20%26%20moving%20fast-f2a516?style=flat-square"></a>
  <br>
  <a href="https://sonarcloud.io/project/overview?id=storytold_lightcraft"><img alt="Scanned by SonarCloud" src="https://img.shields.io/badge/Scanned_by-SonarCloud-4b9fd5?style=flat-square&logo=sonarqubecloud&logoColor=white"></a>
  <a href="https://coderabbit.ai"><img alt="Reviewed by CodeRabbit" src="https://img.shields.io/coderabbit/prs/github/storytold/lightcraft?style=flat-square&label=Reviewed%20by%20CodeRabbit&logo=coderabbit"></a>
</p>

<p align="center">
  <a href="https://discord.gg/artcraft"><img alt="Discord의 ArtCraft 커뮤니티에 참여하기" src="https://img.shields.io/badge/Join%20us%20on%20Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white" height="40"></a>
</p>

<p align="center">
  <a href="https://getartcraft.com/apps/lightcraft"><b>LightCraft 제품 페이지</b></a> ·
  <a href="https://getartcraft.com/">ArtCraft</a> ·
  <a href="https://getartcraft.com/apps">Crafting Apps 전체 보기</a>
</p>

<br>

<p align="center">
  <img src="docs/images/hero-tetons.jpg" alt="LightCraft 편집 화면. Ansel Adams의 The Tetons and the Snake River와 오른쪽의 Light·Effects 패널, 아래쪽의 네 장짜리 필름스트립" width="100%">
  <br>
  <sub><i>Ansel Adams, “The Tetons and the Snake River” (1942). 퍼블릭 도메인, U.S. National Archives. LightCraft에서 현상했습니다.</i></sub>
</p>

> [!NOTE]
> **ArtCraft는 다양한 분야의 창작자가 모이는 커뮤니티입니다.** 디지털 아트, 생성형 AI, 음악, 게임 등
> 무엇을 만들든 환영합니다. **[Discord에서 만나보세요.](https://discord.gg/artcraft)**

<p align="center">
  <a href="#edit-like-you-mean-it">사진 편집</a> ·
  <a href="#color-grading-the-cinematic-way">컬러 그레이딩</a> ·
  <a href="#before--after">편집 전후 비교</a> ·
  <a href="#masking-that-goes-where-you-point">마스크</a> ·
  <a href="#presets-profiles--the-color-mixer">프리셋</a> ·
  <a href="#organize-everything">라이브러리</a> ·
  <a href="#built-for-agents">에이전트와 MCP</a> ·
  <a href="#fast-native-private">성능</a> ·
  <a href="#feature-status">개발 현황</a> ·
  <a href="#quick-start">빠른 시작</a> ·
  <a href="ROADMAP.md">로드맵</a> ·
  <a href="#downloads">다운로드</a> ·
  <a href="#the-crafting-apps">Crafting Apps</a>
</p>

<br>

<a id="edit-like-you-mean-it"></a>
## 원하는 모습으로 사진 편집하기

LightCraft는 사진 관리부터 RAW 현상까지 한 앱에서 처리합니다. 모든 보정은 **비파괴 방식**으로 적용하므로
편집할 때 원본을 덮어쓰지 않습니다. 슬라이더 조정은 **scene-referred 방식의 광색역 32-bit float 파이프라인**을 거쳐
렌더링됩니다. 밝은 영역의 부드러운 톤 전환과 어두운 영역의 디테일을 함께 다룰 수 있습니다.

<table>
<tr>
<td width="50%" valign="top">

### ☀️ 밝기와 명암
**노출, 대비, 하이라이트, 섀도, 화이트, 블랙**을 조정합니다. 경계를 고려하는 로컬 톤 매핑을 사용하며,
내부적으로는 로그 휘도에 guided filter를 적용합니다. 단순한 곡선 조정에서 생기기 쉬운 회색 테두리를 줄이면서
밝은 하늘과 어두운 영역을 보정할 수 있습니다.

### 🎨 색상
색온도와 틴트로 **화이트 밸런스**를 조정하고, 프리셋·자동 조정·**스포이드**를 사용할 수 있습니다.
RAW는 Kelvin 기준, JPEG는 상대값으로 조정합니다. 피부색을 고려하는 **생동감**, **채도**,
색조·채도·휘도를 조정하는 8개 색상별 **색상 믹서(Color Mixer)**, 혼합과 균형을 조정하는 **컬러 그레이딩(Color Grading)** 휠을 제공합니다.
색상 계산에는 지각적 색 공간인 OkLCh를 사용합니다.

</td>
<td width="50%" valign="top">

### ✨ 효과
미세한 디테일을 살리는 **텍스처**, 중간톤의 대비를 조정하는 **부분 대비(Clarity)**,
안개를 줄이거나 음수 값으로 분위기를 더하는 **디헤이즈(Dehaze)**를 제공합니다.
크롭 후 적용되는 **비네팅**은 하이라이트 보호·원형도·페더를 조절할 수 있고,
필름 **그레인**은 해상도에 따라 달라지지 않도록 크기와 거칠기를 계산합니다.

### 📈 톤 곡선
영역의 경계를 옮길 수 있는 파라메트릭 곡선과 RGB·Red·Green·Blue 포인트 곡선을 함께 제공합니다.
곡선의 단조성을 유지하므로 의도하지 않은 톤 반전이 생기지 않습니다.

</td>
</tr>
</table>

<p align="center">
  <img src="docs/images/curve-tetons.jpg" alt="Light 패널 아래의 Tone Curve. The Tetons and the Snake River에 완만한 RGB S자 곡선을 적용한 화면" width="100%">
  <br>
  <sub>밝기 슬라이더 아래에서 RGB 포인트 곡선을 조정할 수 있습니다. <i>Ansel Adams, 1942 (퍼블릭 도메인).</i></sub>
</p>

<br>

<a id="color-grading-the-cinematic-way"></a>
## 사진의 분위기를 만드는 컬러 그레이딩

섀도·중간톤·하이라이트의 색을 각각 컬러 휠로 조정합니다. 아래 예시는 Dorothea Lange의
*Migrant Mother*에 하이라이트 42°, 섀도 28°를 적용해 따뜻한 인화 사진의 느낌을 만든 것입니다.

<p align="center">
  <img src="docs/images/grading-migrant-mother.jpg" alt="Migrant Mother를 따뜻한 톤으로 보정한 화면과 중간톤·섀도·하이라이트의 Color Grading 휠" width="100%">
  <br>
  <sub>Color 패널의 섀도·중간톤·하이라이트 휠. <i>Dorothea Lange, “Migrant Mother” (1936). 퍼블릭 도메인, Library of Congress.</i></sub>
</p>

<br>

<a id="before--after"></a>
## 편집 전후 비교

<kbd>\\</kbd>로 원본 보기를 전환하고, <kbd>Y</kbd>로 나란히 비교하거나
<kbd>Shift</kbd>+<kbd>Y</kbd>로 한 화면을 나누어 비교할 수 있습니다.
아래 이미지는 모두 에이전트가 [제어 채널](#built-for-agents)로 촬영한 실제 LightCraft 화면입니다.

<table>
<tr>
<td width="50%"><img src="docs/images/ba-tetons.jpg" alt="The Tetons and the Snake River 편집 전후. 구름의 대비와 강 주변의 어두운 영역을 보정한 모습"><br><sub><b>The Tetons and the Snake River.</b> Highlights −45, Shadows +38, Clarity +28, Dehaze +18. <i>Ansel Adams, 1942 (퍼블릭 도메인).</i></sub></td>
<td width="50%"><img src="docs/images/ba-migrant-mother.jpg" alt="Migrant Mother 편집 전후. 어두운 영역을 밝히고 따뜻한 색감을 더한 모습"><br><sub><b>Migrant Mother.</b> Shadows +42, Texture +18, 분할 토닝과 비네팅 적용. <i>Dorothea Lange, 1936 (퍼블릭 도메인).</i></sub></td>
</tr>
<tr>
<td width="50%"><img src="docs/images/ba-earthrise.jpg" alt="Earthrise 편집 전후. 달 너머 지구의 색을 조금 더 따뜻하고 풍부하게 보정한 모습"><br><sub><b>Earthrise.</b> Dehaze +22, Highlights −30, 따뜻한 화이트 밸런스, Vibrance +22. <i>NASA / Bill Anders, Apollo 8, 1968 (퍼블릭 도메인).</i></sub></td>
<td width="50%"><img src="docs/images/ba-blue-marble.jpg" alt="The Blue Marble 편집 전후. 검은색과 구름의 디테일을 강조한 모습"><br><sub><b>The Blue Marble.</b> Highlights −38, Blacks −20, Dehaze +15, Vibrance +30. <i>NASA, Apollo 17, 1972 (퍼블릭 도메인).</i></sub></td>
</tr>
</table>

<br>

<a id="masking-that-goes-where-you-point"></a>
## 필요한 부분만 보정하는 마스크

**브러시**의 크기·페더·플로우·밀도를 조정해 칠하거나 지울 수 있습니다.
**선형·방사형 그레이디언트**의 핀을 드래그해 위치를 조정하고,
**휘도 범위·색상 범위·하늘·피사체·배경**으로 영역을 선택할 수도 있습니다.
여러 선택 영역을 **추가·빼기·교차**로 조합하거나 반전하고, 각 마스크에서 색온도·틴트·노출·대비·
하이라이트·섀도·화이트·블랙·텍스처·Clarity·Dehaze·색조·채도·선명도·노이즈·모아레·디프린지를
조정합니다. 마스크 전체의 강도도 별도로 조절할 수 있습니다.

데스크톱의 **Object**와 **Describe** 마스크는 SAM 3로 클릭 또는 텍스트에 해당하는 대상을 선택합니다.
모델은 별도 설치해야 하며 [설정 방법과 이용 조건](docs/ai-masks.md)을 확인하세요.
피사체·하늘·배경 선택은 아직 전통적인 휴리스틱 방식입니다.
이미 만들어진 모델 기반 마스크는 모델이 없는 환경에서도 렌더링하고 내보낼 수 있습니다.

<p align="center">
  <img src="docs/images/masking.jpg" alt="호수 장면에 선형 Sky 마스크와 태양 주위의 방사형 Sun glow 마스크를 겹친 화면. 선택 영역은 빨간색으로 표시" width="100%">
  <br>
  <sub>선형 “Sky” 마스크 위에 방사형 “Sun glow” 마스크를 적용했습니다(Temp +40, Exposure +0.50). <i>LightCraft가 절차적으로 생성한 데모 사진입니다.</i></sub>
</p>

<br>

<a id="presets-profiles--the-color-mixer"></a>
## 프리셋, 프로필, Color Mixer

*Golden Hour, Teal & Orange, Faded Matte, Selenium Tone, Crisp Landscape* 등
41개의 기본 프리셋을 제공하며 **강도(Amount)**를 0~200%로 조절할 수 있습니다.
원하는 설정 그룹만 골라 나만의 프리셋을 저장하고, 즐겨찾기에 추가하거나
선택한 여러 사진에 보정을 복사·붙여넣기·동기화할 수 있습니다.

<p align="center">
  <img src="docs/images/presets-mixer.jpg" alt="B&W·Color·Film·Landscape·Portrait·Style로 묶인 프리셋 목록과 보라색을 선택한 8색 Color Mixer" width="100%">
  <br>
  <sub>프리셋 목록과 8개 색상을 개별 조정하는 Color Mixer. <i>데모 라이브러리 사진입니다.</i></sub>
</p>

<table>
<tr>
<td width="50%" valign="top">

### ✂️ 자르기(크롭), 수평 맞추기, 기하 보정
자유 비율 또는 고정 비율(1:1, 4:5, 5:7, 2:3, 4:3, 16:9, 16:10, 원본)을 사용합니다.
드래그로 수평을 맞추면 이미지 안에서 가능한 가장 넓은 크롭 영역을 유지합니다.
삼분할·격자·황금비 가이드, 뒤집기·90° 회전과 함께 수직·수평·회전·종횡비·크기·오프셋을 수동 조절할 수 있습니다.

</td>
<td width="50%" valign="top">

### ⚫️ 흑백
<kbd>V</kbd>로 흑백 전환 후 8개 색상별 **B&W Mix**로 하늘을 어둡게 하거나 나뭇잎을 밝게 만들 수 있습니다.
Color Grading을 더하면 셀레늄·세피아·분할 토닝 느낌도 표현할 수 있습니다.

</td>
</tr>
<tr>
<td width="50%"><img src="docs/images/crop-tetons.jpg" alt="The Tetons and the Snake River의 16:9 크롭과 삼분할 가이드, 오른쪽 Geometry 슬라이더"><br><sub>삼분할 가이드를 보며 +2.5° 수평 보정을 적용한 16:9 크롭. <i>Ansel Adams, 1942 (퍼블릭 도메인).</i></sub></td>
<td width="50%"><img src="docs/images/bw-split.jpg" alt="모래언덕 사진을 분할 비교. 왼쪽은 컬러 원본, 오른쪽은 그레인과 비네팅을 적용한 흑백"><br><sub>왼쪽은 원본, 오른쪽은 Clarity·비네팅·그레인을 적용한 흑백입니다. <i>데모 라이브러리.</i></sub></td>
</tr>
</table>

<br>

<a id="organize-everything"></a>
## 사진 정리와 라이브러리

**모든 사진, 최근 추가, 선택한 사진, 날짜별 보기, 폴더 안의 앨범, 최근 삭제한 사진**으로 라이브러리를 정리합니다.
<kbd>0</kbd>~<kbd>5</kbd>로 별점을, <kbd>P</kbd> / <kbd>X</kbd> / <kbd>U</kbd>로 플래그를,
<kbd>6</kbd>~<kbd>9</kbd>로 색상 라벨을 지정합니다. 검색에서는 다음과 같은 필드 조건을 사용할 수 있습니다.

`rating:>3 flag:pick iso:>800 camera:x2 date:2026-04 keyword:mountains person:"Jane Doe"`

일반 검색어로 얼굴에 붙인 이름도 찾을 수 있고, 여러 단어를 하나로 검색할 때는 따옴표로 묶습니다.
촬영일·가져온 날짜·편집일·파일명·별점·크기 또는 무작위 순서로 정렬할 수 있습니다.
무작위 순서는 유지되며 **View → Sort → Reshuffle**에서 다시 섞습니다.
**Photo Grid**와 **Square Grid**는 화면에 보이는 항목만 그리므로 사진 수가 많아도 효율적으로 탐색할 수 있습니다.

**인물** 보기에서는 XMP에서 읽은 이름을 포함해 이름별로 얼굴을 모아 보여주며, 사진 상세 화면에서 얼굴 영역을 수정할 수 있습니다.
데스크톱의 자동 얼굴 검출·인식은 **Settings → Faces**에서 설치하는 선택적 모델을 사용합니다.
모델 가중치는 기본 포함되지 않습니다. [얼굴 모델 설정 안내](docs/faces.md)를 참고하세요.

<table>
<tr>
<td width="50%"><img src="docs/images/grid-demo.jpg" alt="데모 사진 24장에 별점과 플래그를 표시한 Photo Grid와 Travel 2026 폴더 아래의 앨범 목록"><br><sub>폴더 안의 앨범, 별점, 플래그를 보여주는 Photo Grid. <i>데모 라이브러리.</i></sub></td>
<td width="50%"><img src="docs/images/grid-pd.jpg" alt="별점과 선택 플래그가 표시된 퍼블릭 도메인 사진 네 장의 Square Grid"><br><sub>퍼블릭 도메인 예시 사진을 모은 Square Grid.</sub></td>
</tr>
<tr>
<td colspan="2"><img src="docs/images/info-earthrise.jpg" alt="Earthrise의 파일명·2400×2400 크기·JPEG 형식·별점·제목·캡션·카메라 정보를 보여주는 Info 패널"><br><sub>Info 패널에는 파일 정보, 별점, 제목, 캡션, 저작권과 카메라 메타데이터가 표시됩니다. <i>NASA / Bill Anders, “Earthrise”, Apollo 8, 1968 (퍼블릭 도메인).</i></sub></td>
</tr>
</table>

<br>

<a id="built-for-agents"></a>
## 에이전트와 MCP

LightCraft의 메뉴·슬라이더·브러시·크롭·키보드 동작은 고정된 ID와 JSON 매개변수를 가진 **명령**입니다.
UI·CLI·JSON-lines 제어 채널·**MCP 서버**가 모두 같은 명령을 실행합니다.
에이전트가 사진을 고르고, 현상하고, 하늘을 마스킹하고, 내보낸 결과를 화면으로 확인할 수 있습니다.

```sh
lightcraft --control 7980 ~/Pictures/trip
```
```jsonc
{"method": "engine.execute", "params": {"command": "photo.flag",  "params": {"flag": "pick"}}}
{"method": "engine.execute", "params": {"command": "develop.set", "params": {"values": {"light.highlights": -45, "light.shadows": 38}}}}
{"method": "engine.execute", "params": {"command": "mask.add",    "params": {"kind": "radial", "center": [0.62, 0.4], "rx": 0.2, "ry": 0.14}}}
{"method": "ui.clickWidget",   "params": {"id": "slider:effects.clarity"}}       // 이름으로 위젯 조작
{"method": "ui.pointer",       "params": {"events": [{"kind":"down","x":0.2,"y":0.3}, {"kind":"up","x":0.4,"y":0.3}]}}
{"method": "ui.screenshot",    "params": {"path": "after.png"}}
```

- **명령 목록:** `engine.commands`에서 사용할 수 있는 명령을, `develop.controls`에서 슬라이더의 범위·기본값·현재 값을 확인합니다.
- **MCP 서버:** `lightcraft-cli mcp`는 Claude를 비롯한 MCP 클라이언트에 명령 목록과 가져오기·검색·현상·마스크·렌더링·내보내기 도구를 제공합니다.
  렌더링 결과는 이미지로 반환됩니다. 화면 없이 실행하거나, 실행 중인 앱에 연결해 스크린샷·클릭·제스처를 사용할 수 있습니다.
  자세한 내용은 [MCP 문서](docs/mcp.md)를 참고하세요.

  ```sh
  cargo build --release -p lightcraft-cli
  claude mcp add lightcraft -- "$PWD/target/release/lightcraft-cli" mcp ~/Pictures/shoot          # headless
  claude mcp add lightcraft-app -- "$PWD/target/release/lightcraft-cli" mcp --connect 127.0.0.1:7980  # 실행 중인 앱
  ```

- **CLI 자동화:** `lightcraft-cli run --import in.dng develop.set control=light.exposure value=0.7 app.export path=out.jpg longEdge=2048`처럼
  여러 명령을 순서대로 실행할 수 있습니다. Headless·저장된 라이브러리·실행 중인 앱을 대상으로 동작하고, 명령마다 JSON 결과를 출력합니다.
  단일 파일은 `lightcraft-cli render in.dng -o out.jpg --set light.exposure=0.7 --preset …`로 처리할 수도 있습니다.
- **실행 취소:** 에이전트의 작업도 되돌릴 수 있습니다. 슬라이더를 한 번 드래그하거나 스크립트에서 연속으로 조정한 값은 하나의 실행 취소 단계로 묶입니다.
- **Contact Sheet PDF(네이티브 앱·CLI):** 사진을 선택하고 **File → Contact Sheet PDF…**를 실행합니다.
  A4·Letter, 가로 방향, 행·열 수, 파일명 캡션을 설정해 여러 페이지의 PDF를 만들고 PDF 뷰어에서 인쇄할 수 있습니다.
  현재 보정을 적용하며, 추가로 크롭하지 않고 각 셀에 맞춰 150 dpi·sRGB로 렌더링합니다.
  백그라운드 내보내기는 취소할 수 있고 원본과 XMP sidecar를 보호합니다.

  ```sh
  lightcraft-cli run --import ~/Pictures/shoot library.selectAll export.contactSheet path=Contact.pdf paper=a4 columns=3 rows=4 captions=true
  ```

  `export.contactSheet`에는 `ids`, `landscape=true`, `paper=letter`도 지정할 수 있습니다.
  PDF 전체가 완성된 뒤에만 기존 출력 파일을 교체합니다. 제한은 사진 1,000장·100페이지·256 MiB입니다.
  캡션은 워터마크와 같은 폰트로 래스터화되며 CJK에는 craft-fonts가 필요합니다.
  긴 이름은 중간을 생략합니다. 이 기능은 사진 목록을 PDF로 만드는 용도이며 프린터 드라이버나 자유로운 인쇄 레이아웃 편집기는 아닙니다.
- **인쇄 레이아웃(네이티브 앱·CLI):** **File → Print…**에서 Single Image·Contact Sheet·Picture Package·Custom Package를 편집합니다.
  Picture Package는 Auto Layout으로 셀을 페이지에 배치하며, Custom Package는 셀 이동·크기·겹침 순서·셀/격자 스냅을 지원합니다.
  Zoom to Fill·Rotate to Fit, 사진 테두리, 안쪽 선, 4×6~13×19 및 A3~A5·사용자 지정 용지,
  여백, in/cm/mm/pt 눈금, 페이지 배경, 72~720 ppi와 저장된 템플릿을 사용할 수 있습니다.
  **Print to PDF…**는 백그라운드로 실행됩니다. 모든 동작은 `print.*` 명령이므로 CLI와 MCP에서도 사용할 수 있습니다.

  ```sh
  lightcraft-cli run --import ~/Pictures/shoot print.set 'layout={"style":"picturePackage","page":{"paper":"8x10"}}' \
      print.cell.add width=5 height=7 print.cell.add width=2.5 height=3.5 print.layout.autoLayout print.exportPdf path=Package.pdf
  ```

  `print.get`, `print.set`, `print.cell.add`, `print.layout.autoLayout`, `print.exportPdf`, `print.template.save` 등의 명령을 제공합니다.
  Print Job 패널에서는 프린터·용지 ICC 프로필, 렌더링 의도, 검은점 보정, 인쇄 보정, 샤프닝, 8/16-bit PDF 출력을 설정합니다.
  OS 인쇄 대화상자를 통한 직접 인쇄와 일부 페이지 오버레이(식별판·워터마크 등)는 아직 지원하지 않습니다.
- **슬라이드쇼:** **View → Slideshow**에서 현재 보이는 사진을 전체 화면으로 재생하며, 페이드·반복·무작위 순서를 지원합니다.
  **Settings → Slideshow**에서 레이아웃·템플릿·배경·오버레이·제목 화면을 설정합니다.
  네이티브 앱은 PDF·JPEG 시퀀스·MP4/WebM의 AV1 영상으로 내보낼 수 있습니다.
  음악과 전용 Slideshow 모듈 편집기는 아직 없습니다. [슬라이드쇼 문서](docs/slideshow.md)를 참고하세요.
- **위젯 제어:** `ui.widgets`로 각 위젯을 찾고 이름으로 클릭할 수 있습니다. 에이전트도 사용자가 보는 UI를 조작합니다.
- 이 README의 스크린샷은 [`docs/showcase/`](docs/showcase/) 스크립트로 만들었습니다.
  [제어 프로토콜 문서](docs/control-protocol.md)에서 전체 API를 확인할 수 있습니다.

<br>

<a id="fast-native-private"></a>
## 빠르게, 내 컴퓨터에서

- **순수 Rust:** C 의존성 없이 RAW 디코더(DNG, Canon CR2/CR3, Sony ARW, Nikon NEF, X-Trans를 포함한 Fujifilm RAF,
  Panasonic RW2 / Leica RWL, Pentax PEF, Olympus ORF), 색상 처리, 렌더링 파이프라인을 구현했습니다.
  JPEG·PNG·TIFF·WebP·PSD 합성 이미지·JPEG XL·GIF 첫 프레임·BMP를 열 수 있습니다.
  HEIC/HEIF는 `--features heif` 빌드 옵션으로 지원하며, HEVC 포함 여부는 PhotoCraft와 마찬가지로 빌드 시 선택합니다.
- **광색역 파이프라인:** 내부적으로 선형 Rec.2020 float와 Bradford 색순응 화이트 밸런스를 사용합니다.
  색역 밖 색상은 클리핑 대신 gamut mapping으로 처리하고, RAW의 밝은 부분에는 filmic shoulder를 적용합니다.
  편집하지 않은 JPEG는 픽셀을 그대로 통과시킵니다.
- **해상도에 독립적인 편집:** 반경과 브러시 크기를 이미지에 대한 상대값으로 저장해,
  400px 미리보기·5K 화면·60MP 내보내기에서 같은 보정이 적용됩니다.
- **GPU 가속과 CPU 기준 검증:** 현상 파이프라인은 wgpu compute(Metal / Vulkan / DX12)로 실행하며,
  CPU 결과와 1/255 이내 오차를 기준으로 검증합니다.
  프로젝트가 Apple M4 Pro의 24MP RAW에서 측정한 값은 슬라이더 재렌더링 약 4ms, 캐시 없는 2.5MP 상세 보기 약 30ms,
  병렬 JPEG 인코딩을 포함한 원본 크기 내보내기 약 0.3초입니다.
  GPU가 없으면 같은 파이프라인이 여러 CPU 코어에서 실행되고, 슬라이더가 영향을 주는 단계만 다시 계산합니다.
  일괄 내보내기에서는 여러 사진을 함께 처리해 디코딩·렌더링·인코딩을 겹쳐 실행합니다.
- **빠른 사진 선별:** RAW를 열면 카메라 내장 미리보기 또는 캐시를 약 0.1초 안에 표시하고,
  이어서 전체 렌더링을 진행합니다(24MP 기준 약 0.2~0.5초).
  앞뒤 사진은 백그라운드에서 준비하므로 사진을 넘기는 시간은 약 50ms입니다.
- **백그라운드 렌더링:** 상세 보기·편집 전후 비교·현재 보이는 썸네일을 UI 스레드 밖에서 렌더링합니다.
  드래그 중에는 빠른 미리보기를, 놓은 뒤에는 최종 품질을 표시합니다.
- **로컬 우선:** 계정·클라우드·사용 통계 수집·구독 없이 사용할 수 있습니다.
  카탈로그는 사람이 읽을 수 있는 작업을 계속 추가하는 로그로 저장되며, 차이 비교·백업·재생이 가능합니다.

<br>

<a id="feature-status"></a>
## 개발 현황과 한계

LightCraft는 빠르게 개발 중인 **alpha** 단계입니다. 자세한 평가는
[로드맵](ROADMAP.md)과 [기능·품질 비교 문서](docs/target-app-parity.md)에서 확인할 수 있습니다.

- **기능 개수 기준으로 약 82%**(핵심 기능 98%)를 구현했습니다.
  [기능 체크리스트](docs/parity-checklist.md)에서 항목별로 추적합니다.
- **일상적인 작업 도구로서의 준비도는 전체 제품 약 61%, 일반적인 사진 작업 약 51%, 가벼운 사용 약 65%로 평가합니다.**
  JPEG/DNG와 대부분의 Sony·Panasonic·Fujifilm·Nikon·구형 Canon RAW를 한 컴퓨터에서 다루는 용도로 사용할 수 있습니다.
  [카메라별 RAW 지원 현황](docs/raw-parity.md)을 확인하세요.
- **주요 미완성 영역:**
  - **카메라 색상 보정:** Sony·Nikon·Panasonic·Fujifilm·Canon CR3는 카메라 JPEG로 추정한 색감과 일부 내장 프로필을 사용합니다.
    내장 프로필 대상은 ILCA-99M2·ILCE-7CR·ILCE-7M4·ILCE-7RM5·X-H2S·X-T4·Nikon Z 6II·Z 8·D5300 등입니다.
    실측 기반 보정은 아직 부족하며, 지원하지 않거나 적합도가 낮은 파일에는 중립 행렬을 적용합니다.
  - **압축 Olympus RAW 및 일부 CR3:** 지원하지 않는 형식은 내장 JPEG가 있으면 미리보기로 엽니다.
    Fujifilm 무손실·손실 압축 RAF는 센서 데이터를 디코딩합니다.
    [검증 내용과 기존 라이브러리 다시 읽기 안내](docs/raf-compression.md)를 참고하세요.
  - **AI 품질과 모델 설치:** 피사체·하늘 선택은 휴리스틱 방식이며, Object/Describe는 선택적으로 설치하는 SAM 3를 사용합니다.
    AI Denoise는 별도 가중치를 설치한 Bayer RAW를 지원합니다. X-Trans·선형 RGB 노이즈 제거와 Super Resolution은 아직 없습니다.
    [AI 마스크](docs/ai-masks.md), [AI Denoise](docs/denoise.md) 문서를 참고하세요.
  - **네트워크 라이브러리:** 미리보기에는 RAW 헤더와 내장 JPEG만 읽고, 필요한 태그가 뒤쪽에 있는 CR3·TIFF RAW는
    sparse planner가 계산한 범위만 읽습니다(테스트 공유 폴더에서 56.5MB → 5.9MB, 10초 → 1초).
    하지만 현상에는 네트워크로 센서 데이터를 읽어야 합니다. [RAW 미리보기 읽기](docs/raw-preview-reads.md)를 참고하세요.
  - **HDR 화면 출력, 영상 가져오기·편집, Book·Map 모듈:** 아직 미완성입니다.
    인쇄 레이아웃과 색상 관리 PDF 출력은 동작하지만 직접 인쇄와 일부 페이지 오버레이는 미완성입니다.
    슬라이드쇼 재생·내보내기는 가능하며 전용 편집기와 음악은 아직 없습니다.
- 다음 작업은 [우선순위별 미완성 항목](docs/gaps.md)과 [개발 로드맵](docs/roadmap.md)에서 확인할 수 있습니다.

| 영역 | 상태 |
|---|---|
| 라이브러리: 앨범·폴더·스마트 앨범·스택(자동 묶기 포함)·가상 사본·별점·플래그·라벨·필터·검색·정렬·그리드·필름스트립 | ✅ |
| 사진 선별: 확대 동기화를 지원하는 비교, Survey 보기, 자동 다음 사진, 즉시 미리보기 | ✅ |
| 밝기·색상·효과·비네팅·톤 곡선·채도 보정·대상 지정 조정·Color Mixer·Point Color·Color Grading·Calibration·흑백 | ✅ |
| 마스크: 브러시·선형·방사형·휘도/색상 범위·추가/빼기/교차·Object·Describe | ✅ · Object/Describe는 SAM 3 필요, 피사체/하늘은 휴리스틱 방식([자세히](docs/ai-masks.md)) |
| 크롭·수평 보정 및 자동 수평·뒤집기·회전·종횡비·가이드 | ✅ |
| 자체 프로필(Color·Neutral·Vivid·Landscape·Portrait·Monochrome), 프리셋·버전·기록·설정 복사/붙여넣기/동기화 | ✅ |
| 카메라 색상: DNG 행렬, 카메라 JPEG 기반 색감 추정, 일부 카메라 내장 프로필 | 🚧 실측 보정과 기종별 검증 부족([RAW 지원 현황](docs/raw-parity.md)) |
| 명령 목록으로 생성하는 macOS 메뉴 막대, 제어 채널, 이름으로 접근하는 위젯, headless UI 스크린샷 | ✅ |
| RAW: DNG, CR2, CR3(무손실 CRX Bayer 및 0x100/0x200 C-RAW), ARW, NEF(비압축·무손실/손실 압축), RAF(비압축·무손실/손실 압축, Bayer·X-Trans), Panasonic RW2 / Leica RWL / Panasonic RAW(DMC-LX1~DC-S1RM2), PEF, 비압축 12.8 bpp ORF, 비압축 SRW; CR3 등 각 형식의 내장 미리보기 | 🚧 기타 CR3, 압축 ORF/SRW, Nikon HE NEF 디코딩은 ⬜ ([자세히](docs/raw-parity.md)) |
| 디테일: 샤프닝, 휘도·색상 노이즈 감소 | ✅ · AI Denoise 🚧(Bayer, 별도 가중치 [설치](docs/denoise.md)), Super Resolution ⬜ |
| Remove·Heal·Clone, 자동 소스 선택, 비생성형 Fill, 수정 가능한 보정 핀, 얼룩 시각화, 적목·동물 눈 보정 | ✅ · Fill은 PatchMatch 사용, 생성형 채우기는 없음 |
| 내보내기: JPEG·PNG·TIFF·병합 PSD·WebP·AVIF·JPEG XL·RAW/렌더링 DNG·원본, 크기/파일 용량 제한, 출력 샤프닝, 파일명 템플릿, 일괄 작업, 메타데이터 정책, 문자/이미지 워터마크 | ✅ |
| HDR: 헤드룸 제한, SDR 렌더링, HDR 시각화, gain map JPEG·PQ/HLG AVIF·PQ PNG/JPEG XL·float TIFF 출력 | ✅ · HDR 화면 출력 ⬜ |
| 카탈로그 작업 로그·스냅샷·백그라운드 압축·저장 오류 안내, 디스크 썸네일 캐시 | ✅ |
| 가져오기: 원래 위치에 추가·복사·이동, 이름/폴더 템플릿, 장치, 중복 감지, 감시 폴더, 로컬 폴더 탐색 | ✅ |
| MCP(headless 또는 실행 중인 앱, 영구 라이브러리), CLI, 제어 채널 | ✅ |
| XMP sidecar 읽기/쓰기·자동 저장, `crs:` 현상 설정 읽기, `.lcpreset`·XMP 프리셋 | ✅ |
| 광학 보정: 왜곡·비네팅·자동/수동 색수차·디프린지·DNG 내장 보정·Panasonic/Leica RW2/RWL 및 Nikon Z 6II/Z 8/Z f 왜곡 정보; Geometry·Constrain Crop·Upright(Auto/Level/Vertical/Full/Guided) | ✅ · 자체 카메라/렌즈 프로필 데이터베이스 ⬜ |
| 사진 합성: HDR(자동 정렬·고스트 제거), 파노라마(구면/원통/원근·경계 변형·자동 크롭), HDR 파노라마 → DNG | ✅ |
| GPU 파이프라인(wgpu compute, CPU 대비 1/255 이내 검증), 장치 제한/오류 시 CPU 전환 | ✅ · 브라우저 WebGPU 🚧 |
| 인물 보기, XMP 얼굴 이름, 얼굴 영역 편집, 자동 얼굴 검출·인식 | ✅ · 자동 기능은 선택적 모델 필요([설정](docs/faces.md)) |
| GPX 위치 자동 태깅, 오프라인 역지오코딩 | ✅ · 지명은 GeoNames 데이터 필요, Map 보기 ⬜ |
| 인쇄 레이아웃·Picture Package·템플릿·색상 관리 PDF | ✅ 레이아웃/PDF · 🚧 직접 인쇄·일부 페이지 오버레이 |
| 슬라이드쇼 재생·레이아웃·오버레이·템플릿·PDF/JPEG/AV1 출력 | ✅ 재생 · 🚧 전용 편집기·음악([자세히](docs/slideshow.md)) |
| 영상 가져오기·편집, Book 모듈 | ⬜ |
| 웹(WASM): OPFS/IndexedDB 라이브러리, Web Worker 렌더링, 내보낸 파일 다운로드 | ✅ · WebGPU 및 Safari/Firefox 검증 🚧 |

<sub>✅ 사용 가능 · 🚧 개발 중 · ⬜ 미구현</sub>

<br>

<a id="quick-start"></a>
## 빠른 시작

데스크톱·웹·CLI를 포함한 workspace 빌드에는 **Rust 1.95 이상**이 필요합니다.

```sh
git clone https://github.com/storytold/lightcraft && cd lightcraft
cargo run --release -p lightcraft                       # 라이브러리 열기(기본: ~/Pictures/LightCraft Library, 새 라이브러리에는 데모 사진 포함)
cargo run --release -p lightcraft -- ~/Pictures/trip    # 사진 가져오기(하위 폴더 검색, 중복 제외)
cargo run --release -p lightcraft -- --memory           # 메모리에서만 실행하는 데모 세션
cargo run --release -p lightcraft -- --control 7980     # 자동화 제어 채널 활성화
cargo xtask web --serve                                 # 브라우저: http://127.0.0.1:8080/
cargo run --release -p lightcraft-cli -- render photo.jpg -o out.jpg --set light.exposure=0.5
cargo xtask ci                                          # fmt, clippy, HEIF, tests, parity, layers, assets, WASM
cargo xtask ci --quick                                  # 현재 브랜치의 변경과 관련된 검사
```

`cargo xtask ci --quick`은 `origin/main`과의 공통 조상을 기준으로 비교합니다(다른 기준은 `--base REF`).
fmt·clippy·변경된 패키지의 테스트·layers·assets를 실행하고, 관련 변경이 있으면 HEIF·parity·WASM 검사도 실행합니다.
`Cargo.toml`·`Cargo.lock`·lint 설정을 바꾸면 전체를 검사합니다.
GitHub의 모든 PR에서는 전체 `cargo xtask ci`를 실행합니다(`.github/workflows/ci.yml`).

한 컴퓨터에서는 기본적으로 CI 하나만 실행됩니다. 다른 worktree에서 시작한 CI는 앞선 작업이 끝나기를 기다립니다.
동시에 여러 빌드를 돌려 전체 실행 시간이 길어지는 것을 막기 위한 설정입니다.
`LIGHTCRAFT_CI_SLOTS=2`로 두 개를 허용하거나 `LIGHTCRAFT_CI_LOCK=off`로 잠금을 끌 수 있습니다.

CI는 기본적으로 line-table debug 정보를 사용하고, 시작 시 사용 가능한 RAM 1.5GB당 빌드 작업 하나를 배정합니다.
CPU 수를 넘지 않으며 메모리를 읽지 못하면 4개를 사용합니다.
예를 들어 여유 메모리가 6GB라면 빌드 작업은 4개이고, 테스트 스레드 수도 이에 맞추되 최대 4개입니다.
전체 디버그 정보를 가진 링커를 CPU 수만큼 동시에 실행하면 테스트 전에 메모리가 소진될 수 있기 때문입니다.
GPU 검증 범위는 줄이지 않습니다.

`CARGO_PROFILE_DEV_DEBUG`·`CARGO_BUILD_JOBS`·`RUST_TEST_THREADS`·`NEXTEST_TEST_THREADS`를 명시하면 이 기본값보다 우선합니다.
테스트는 `cargo test`로 실행하며, `LIGHTCRAFT_NEXTEST=1`이면 [cargo-nextest](https://nexte.st)를 사용합니다.
이 프로젝트에서는 테스트마다 데모 장면을 다시 생성하므로 nextest가 더 느릴 수 있습니다.
일반 개발 명령은 각자의 설정을 그대로 사용합니다.

**메모리가 적은 환경의 release 빌드:** [#1196](https://github.com/storytold/lightcraft/issues/1196)은
16GB Arch/AUR 환경에서 전체 debug 정보·thin LTO·codegen unit 1개로 데스크톱과 CLI를 동시에 링크할 때 OOM이 발생한 사례입니다.
위 CI 기본값은 일반 `cargo build --release`에는 적용되지 않습니다. 먼저 작업 하나로 빌드하고, 여유가 있으면 두 개를 시도하세요.

```sh
CARGO_BUILD_JOBS=1 cargo build --release -p lightcraft -p lightcraft-cli
# 여유가 있다면: CARGO_BUILD_JOBS=2 cargo build --release -p lightcraft -p lightcraft-cli
```

패키징 환경에서 전체 debug 정보나 메모리를 많이 쓰는 LTO 설정을 사용한다면, 해당 명령에만 아래 설정을 적용할 수 있습니다.

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo build --release -p lightcraft -p lightcraft-cli
```

이 POSIX shell 환경 변수 지정은 해당 명령에만 적용되며 전역 Cargo 설정이나 저장소의 release 프로필을 바꾸지 않습니다.
LTO를 끄면 성능과 바이너리 크기가 달라질 수 있고, debug 정보를 끄면 디버깅 정보가 줄어듭니다.
모든 컴퓨터에서 메모리가 충분하다는 보장은 아닙니다. 배포 패키지를 만드는 경우 큰 바이너리는 순서대로 빌드하고,
메모리가 적은 환경에서 전체 debug 정보·LTO·codegen unit 1개를 함께 사용하는 것은 피하세요.
PKGBUILD가 자체 프로필 설정을 export하면 상속받은 환경 변수를 덮어쓸 수 있으므로,
`makepkg` 앞의 변수 지정이 항상 우선한다고 가정하지 말고 패키징 설정을 조정하거나 관리자에게 문의하세요.

GPU·테스트 사진 corpus·craft-fonts·모델이 필요한 테스트는 해당 자원이 없으면 검사를 생략하고 통과로 보고될 수 있습니다.
libtest에 별도의 “skipped” 상태가 없기 때문입니다.
이를 구분하기 위해 테스트는 `lightcraft_testkit::skip!`으로 생략 사실을 기록하고, `cargo xtask ci`는
OS·CPU·RAM·백엔드별 GPU·corpus·폰트 등 실행 환경과 생략 사유를 출력합니다.
결과는 `target/ci/report.json`과 PR에 붙일 수 있는 `target/ci/summary.md`에 저장합니다.
`--summary-md`로 요약을 출력하고, `--hardware`로 사용 가능한 GPU 백엔드별 테스트도 실행할 수 있습니다.
결과를 외부로 업로드하지는 않습니다.

**한국어·중국어·일본어 표시**에는 공유 폰트 저장소가 필요합니다. 선택적 빌드 입력이며 공식 릴리스에는 포함됩니다.

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
CRAFT_FONTS_DIR=../craft-fonts cargo run --release -p lightcraft
```

폰트 없이도 빌드와 실행은 가능하지만 해당 언어의 글자가 표시되지 않습니다.
폰트 파일은 이 저장소에 넣지 않습니다. [craft-fonts README](https://github.com/storytold/craft-fonts#readme)에
폰트 추가 기준과 라이선스가 설명되어 있습니다.

**언어 설정:** **Edit → Language → 한국어** 또는 **Settings → General → Language**에서 선택합니다.
바꾸면 즉시 적용되고 다음 실행에도 유지됩니다.
한국어 외에 English, 简体中文, 繁體中文（台灣）, 日本語, Português (Brasil), Español, Deutsch, Русский,
Français, Українська, Polski, Čeština, Italiano, ქართული, Svenska, Suomi, Bahasa Indonesia, Tiếng Việt를 지원합니다.
[한국어 사용·번역 안내](docs/localization-ko.md)와 [언어 추가 안내](docs/localization.md)를 참고하세요.

**로그:** 데스크톱 앱은 표준 오류와 설정 폴더의 `logs/lightcraft.log`에 기록합니다. 라이브러리 안에는 저장하지 않습니다.

- Linux: `$XDG_CONFIG_HOME/lightcraft/logs/`, 기본값 `~/.config/lightcraft/logs/`
- macOS: `~/Library/Application Support/LightCraft/logs/`
- Windows: `%APPDATA%\LightCraft\logs\`

**포터블 모드:** 실행 파일 옆에 `portable.txt` 또는 `LightCraft.portable`를 만들면
설정이 같은 위치의 `LightCraftData` 폴더에 저장됩니다.
앱 메뉴나 Dock에서 실행하면 터미널이 없으므로, 문제를 보고할 때 로그 파일을 첨부하세요.
**Help → Open Log Folder**에서 해당 폴더를 열 수 있습니다.

시작할 때 이전 로그는 `lightcraft.1.log`, 그 이전 로그는 `lightcraft.2.log`로 이동하므로
비정상 종료 기록이 다음 실행에도 남습니다. 로그는 16MiB까지만 커지며 `--version`·`--help`는 로그를 만들지 않습니다.
`LIGHTCRAFT_NO_PREFS`를 지정하면 표준 오류에만 기록합니다.
기본 로그 수준은 LightCraft crate의 `info`, 나머지는 `warn`입니다.
`LIGHTCRAFT_LOG=info` 또는 `debug`는 LightCraft crate에 해당 수준을 적용하고, 다른 crate는 경고·오류만 기록합니다.
다른 값은 경고·오류만 기록합니다. `LIGHTCRAFT_LOG`가 `RUST_LOG`보다 우선하며,
`RUST_LOG`만 지정하면 `RUST_LOG=debug`나 `RUST_LOG=warn,lightcraft_pipeline=trace` 같은
env_logger 형식으로 기본값을 대체합니다. `lightcraft*=debug`처럼 `*`로 끝나는 대상은 같은 접두사의 모든 대상을 포함합니다.
panic도 로그에 기록되며 임시 폴더의 `lightcraft-panics.log`에도 남습니다.
로거 구현은 `apps/lightcraft/src/logging.rs`입니다.

웹 빌드에는 `wasm32-unknown-unknown` target과 버전에 맞는 `wasm-bindgen` CLI가 필요합니다.
`cargo xtask web`이 정확한 설치 명령을 안내합니다. [웹 문서](docs/web.md)를 참고하세요.

**Nix**로 데스크톱과 `lightcraft-cli`를 빌드할 수도 있습니다. Nix 빌드는 craft-fonts를 함께 가져옵니다.

```sh
nix run github:storytold/lightcraft                    # 데스크톱 앱 실행
nix build github:storytold/lightcraft                  # → ./result/bin/{lightcraft,lightcraft-cli}
nix develop github:storytold/lightcraft                # Rust toolchain·네이티브 의존성·폰트
```

NixOS에서 `cargo`로 빌드할 때는 `nix develop` 안에서 실행하세요.
밖에서는 앱이 `libvulkan.so.1`을 찾지 못해 GPU 대신 CPU로 렌더링할 수 있으며, 시작 시 안내가 표시됩니다.
`nix run`과 `nix build`는 필요한 경로를 설정합니다.

Flake 설정(NixOS·home-manager·nix-darwin)의 예시:

```nix
# flake.nix
inputs.lightcraft.url = "github:storytold/lightcraft";
# 선택 사항: LightCraft가 고정한 버전 대신 자신의 nixpkgs로 빌드
# inputs.lightcraft.inputs.nixpkgs.follows = "nixpkgs";

# inputs가 있는 NixOS 또는 home-manager 모듈:
nixpkgs.overlays = [ inputs.lightcraft.overlays.default ];   # pkgs.lightcraft 제공
environment.systemPackages = [ pkgs.lightcraft ];           # home-manager: home.packages = [ pkgs.lightcraft ];
```

`nix build`는 .deb/.rpm과 같은 desktop 파일·hicolor 아이콘·AppStream 메타데이터를 설치하고,
검증 단계에서 `cargo test --workspace`를 실행합니다.
`pkgs.lightcraft.overrideAttrs { doCheck = false; }`로 이 검증을 생략할 수 있습니다.

**주요 단축키:** <kbd>G</kbd> 그리드 · <kbd>D</kbd> 상세 보기 · <kbd>E</kbd> 편집 · <kbd>C</kbd> 자르기 ·
<kbd>M</kbd> 마스크 · <kbd>Shift</kbd>+<kbd>P</kbd> 프리셋 · <kbd>\\</kbd> 원본 ·
<kbd>Y</kbd> 편집 전후 비교 · <kbd>Z</kbd> 확대 · <kbd>J</kbd> 클리핑 · <kbd>⌘Z</kbd> 실행 취소 · <kbd>⌘/</kbd> 전체 단축키.

<a id="how-its-built"></a>
## 코드 구조

작고 테스트 가능한 crate로 나눈 Cargo workspace입니다. 의존성 계층은 `cargo xtask layers`로 검사합니다.

`geom`·`color`·`raster`·`tiff` → `raw`·`codecs`·`meta`·`develop` → `pipeline` → `catalog` → `engine` → `ui-egui`

egui 프런트엔드는 교체할 수 있는 crate 하나이며, 아래 계층은 UI에 의존하지 않습니다.
`mcp`와 앱(`lightcraft`, `lightcraft-cli`)은 `engine` 위에서 동작합니다.

<a id="contributing"></a>
## 기여하기

사람과 에이전트 모두 같은 규칙을 따릅니다. 먼저 [AGENTS.md](AGENTS.md)를 읽어주세요.

- **Clean-room 개발:** 독점 소프트웨어의 바이너리나 GPL RAW/사진 처리 코드(darktable·RawTherapee·LibRaw·rawspeed·dcraw 등)를 읽지 않습니다.
  공개 명세와 외부 동작 관찰을 바탕으로 구현합니다.
- **독점 자산 사용 금지:** 상용 제품의 아이콘·스크린샷·프리셋·프로필·LUT·폰트를 가져오지 않습니다.
  저장소의 이미지·아이콘·폰트는 직접 만든 것이거나 퍼블릭 도메인·Creative Commons·OFL·허용적 오픈소스 라이선스여야 합니다.
  같은 커밋에서 [assets/ATTRIBUTION.md](assets/ATTRIBUTION.md)에 출처와 라이선스를 기록합니다.
  새 폰트는 이 저장소가 아닌 [storytold/craft-fonts](https://github.com/storytold/craft-fonts)에 추가합니다.
- **순수 Rust, 의존성 계층, 명령 기반 동작**을 유지합니다. 커밋마다 하나의 작업 ID를 사용하고,
  커밋 전에 `cargo xtask ci`를 통과해야 합니다.
- **비정상 종료 방지:** 테스트 외 코드는 panic 대신 오류를 반환합니다.
  `unwrap()`·`expect()`·`panic!`·`unsafe`를 사용하지 않고, 입력에서 나온 값으로 접근할 때는 범위를 검사합니다.
  crash 수정에는 회귀 테스트를 함께 추가합니다. 자세한 내용은 [AGENTS.md](AGENTS.md#never-crash-outranks-feature-work)를 참고하세요.

질문이나 아이디어가 있거나, 버그를 먼저 논의하고 싶다면 [Discord](https://discord.gg/artcraft)에서 이야기해 주세요.

<br>

<a id="downloads"></a>
## 다운로드

처음 사용한다면 [LightCraft 제품 페이지](https://getartcraft.com/apps/lightcraft)에서 설치 파일을 찾을 수 있습니다.

특정 버전이나 형식이 필요하면 GitHub의 [최신 릴리스](https://github.com/storytold/lightcraft/releases/latest)를 확인하세요.
[전체 릴리스](https://github.com/storytold/lightcraft/releases)에는 이전 버전과 릴리스 노트도 있습니다.
파일명의 `<ver>`는 버전 번호이며, `SHA256SUMS.txt`에 각 파일의 체크섬이 있습니다.

### Windows

| 아키텍처 | 설치 파일 | 포터블 |
|---|---|---|
| x64(64-bit Intel/AMD) | `lightcraft-<ver>-windows-x64.msi` | `lightcraft-<ver>-windows-x64-portable.zip` |
| arm64(Snapdragon 등 ARM PC) | `lightcraft-<ver>-windows-arm64.msi` | `lightcraft-<ver>-windows-arm64-portable.zip` |
| x86(32-bit) | `lightcraft-<ver>-windows-x86.msi` | `lightcraft-<ver>-windows-x86-portable.zip` |

설치 파일과 실행 파일에는 코드 서명이 있습니다. 설치 대상과 폴더를 선택할 수 있으며,
기본값은 현재 계정의 `%LOCALAPPDATA%\Programs\ArtCraft\LightCraft`입니다.
“Everyone on this computer”를 선택하면 `C:\Program Files\ArtCraft\LightCraft`
(32-bit는 `Program Files (x86)`)에 설치합니다. 업그레이드할 때도 선택한 폴더를 유지합니다.
설치 완료 화면에서 바로 실행할 수 있습니다. 무인 설치 예시:

```sh
msiexec /i lightcraft-<ver>-windows-x64.msi /qn INSTALLFOLDER="D:\Apps\LightCraft\"
```

자동 설치의 기본 대상도 현재 계정입니다. `INSTALLFOLDER`는 폴더만 지정하며 설치 대상을 바꾸지 않습니다.
모든 사용자에게 설치하려면 관리자 터미널이나 배포 도구에서 `ALLUSERS=1`을 추가하세요.

### macOS

| 빌드 | 파일 | 비고 |
|---|---|---|
| 앱, Universal(Apple silicon + Intel) | `lightcraft-<ver>-macos-universal.dmg` | 서명 및 공증 완료 |
| CLI, Universal | `lightcraft-cli-<ver>-macos-universal.zip` | 서명 및 공증 완료 |

### Linux

| 형식 | x86_64 | aarch64(ARM64) | 비고 |
|---|---|---|---|
| AppImage | `lightcraft-<ver>-linux-x86_64.AppImage` | `lightcraft-<ver>-linux-aarch64.AppImage` | 포터블, `.zsync` 파일과 [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate)로 업데이트 |
| Flatpak | `lightcraft-<ver>-linux-x86_64.flatpak` | `lightcraft-<ver>-linux-aarch64.flatpak` | 샌드박스 실행, `flatpak install --user <file>` |
| Debian/Ubuntu | `lightcraft-<ver>-linux-x86_64.deb` | `lightcraft-<ver>-linux-aarch64.deb` | |
| Fedora/RHEL/openSUSE | `lightcraft-<ver>-linux-x86_64.rpm` | `lightcraft-<ver>-linux-aarch64.rpm` | |
| Tarball | `lightcraft-<ver>-linux-x86_64.tar.gz` | `lightcraft-<ver>-linux-aarch64.tar.gz` | 원하는 위치에 압축 해제 |

RISC-V(riscv64)는 `lightcraft-<ver>-linux-riscv64.tar.gz`만 제공합니다.
glibc 2.39 이상(Ubuntu 24.04 이상 등)이 필요합니다.

Wayland 데스크톱에서는 파일 드래그 앤 드롭을 지원하는 X11 경로를 사용하기 위해, 가능하면 XWayland로 실행합니다.
`LIGHTCRAFT_WAYLAND=1`이면 네이티브 Wayland를 사용합니다. 이 경우 fractional scaling은 더 선명하지만 파일 드롭은 지원하지 않습니다.

### FreeBSD

| 아키텍처 | 파일 |
|---|---|
| x86_64 | `lightcraft-<ver>-freebsd-x86_64.tar.gz` |

### Web(WebAssembly)

| 빌드 | 파일 | 비고 |
|---|---|---|
| 정적 사이트 | `lightcraft-web-<ver>.zip` | 최신 브라우저에서 실행, 정적 웹 서버로 배포 가능 |

<br>

<a id="the-crafting-apps"></a>
## Crafting Apps

LightCraft는 [ArtCraft](https://getartcraft.com/) 팀의 무료 오픈소스 창작 도구인 **Crafting Apps** 중 하나입니다.
각 앱은 Rust로 처음부터 구현했으며 독립적으로 사용할 수 있습니다.

| | 앱 | 용도 | 코드 | 더 알아보기 |
|:-:|---|---|---|---|
| <img src="https://raw.githubusercontent.com/storytold/photocraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.photocraft.png" alt="" width="32" height="32"> | **PhotoCraft** | 레이어·마스크·텍스트·PSD를 지원하는 이미지 편집 | [GitHub](https://github.com/storytold/photocraft) | [웹사이트](https://getartcraft.com/apps/photocraft) |
| <img src="https://raw.githubusercontent.com/storytold/vectorcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.vectorcraft.png" alt="" width="32" height="32"> | **VectorCraft** | 벡터 일러스트레이션 | [GitHub](https://github.com/storytold/vectorcraft) | [웹사이트](https://getartcraft.com/apps/vectorcraft) |
| <img src="https://raw.githubusercontent.com/storytold/filmcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.filmcraft.png" alt="" width="32" height="32"> | **FilmCraft** | 영상 편집·색보정·사운드 | [GitHub](https://github.com/storytold/filmcraft) | [웹사이트](https://getartcraft.com/apps/filmcraft) |
| <img src="https://raw.githubusercontent.com/storytold/lightcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.lightcraft.png" alt="" width="32" height="32"> | **LightCraft** | **사진 관리와 RAW 현상 · 현재 저장소** | [GitHub](https://github.com/storytold/lightcraft) | [웹사이트](https://getartcraft.com/apps/lightcraft) |
| <img src="https://raw.githubusercontent.com/storytold/pdfcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.pdfcraft.png" alt="" width="32" height="32"> | **PdfCraft** | PDF 읽기·정리·보호 | [GitHub](https://github.com/storytold/pdfcraft) | [웹사이트](https://getartcraft.com/apps/pdfcraft) |
| <img src="https://raw.githubusercontent.com/storytold/effectcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.effectcraft.png" alt="" width="32" height="32"> | **EffectCraft** | 모션 그래픽과 시각 효과 | [GitHub](https://github.com/storytold/effectcraft) | [웹사이트](https://getartcraft.com/apps/effectcraft) |
| <img src="https://raw.githubusercontent.com/storytold/designcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.designcraft.png" alt="" width="32" height="32"> | **DesignCraft** | 페이지 레이아웃과 출판 | [GitHub](https://github.com/storytold/designcraft) | [웹사이트](https://getartcraft.com/apps/designcraft) |

[**ArtCraft**](https://getartcraft.com/)는 이미지와 영상을 세밀하게 제어하고 싶은 창작자를 위한 AI 스튜디오입니다.

<br>

<p align="center">
  <a href="https://discord.gg/artcraft"><img alt="Discord의 ArtCraft 커뮤니티에 참여하기" src="https://img.shields.io/badge/Join%20us%20on%20Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white" height="40"></a>
</p>

<h3 align="center">함께 만들어 가요</h3>

<p align="center">
  Discord에는 그림·사진·영상·타이포그래피 등 다양한 분야의 창작자와,
  무엇을 만들지 탐색 중인 사람들이 모여 있습니다.
  작업을 공유하고, 도움을 요청하고, 문제점이나 바라는 기능을 알려주세요.
  어떤 분야에서 얼마나 오래 작업했든 환영합니다.
</p>

<p align="center">
  <a href="https://discord.gg/artcraft"><b>discord.gg/artcraft</b></a> ·
  <a href="https://getartcraft.com/">getartcraft.com</a> ·
  <a href="https://getartcraft.com/apps">Crafting Apps</a> ·
  <a href="https://getartcraft.com/apps/lightcraft">LightCraft</a>
</p>

<br>

<a id="license-and-credits"></a>
## 라이선스와 출처

LightCraft는 [MIT](LICENSE-MIT) 또는 [Apache-2.0](LICENSE-APACHE) 중 선택한 라이선스로 사용할 수 있습니다.
Copyright (c) 2026 ArtCraft Team and the LightCraft contributors. 필수 고지는 [NOTICE](NOTICE)에 있습니다.

포함된 폰트·아이콘·사진 등의 자산에는 각자의 오픈 라이선스가 적용됩니다.
저작자·출처·라이선스는 [assets/ATTRIBUTION.md](assets/ATTRIBUTION.md)에 기록되어 있습니다.

예시 사진은 Wikimedia Commons를 통해 사용한 퍼블릭 도메인 작품입니다.
Ansel Adams의 *The Tetons and the Snake River*(1942, U.S. National Archives),
Dorothea Lange의 *Migrant Mother*(1936, Library of Congress),
Bill Anders / NASA의 *Earthrise*(1968), NASA의 *The Blue Marble*(1972)을 사용했습니다.
데모 라이브러리 사진은 LightCraft가 절차적으로 생성합니다.

기본 UI 폰트는 Inter(SIL OFL 1.1)입니다.
[craft-fonts](https://github.com/storytold/craft-fonts)를 포함한 빌드(공식 릴리스 포함)는
나눔고딕(NanumGothic)과 중국어·일본어 폰트(Noto Sans CJK SC, BIZ UDPGothic, BIZ UDMincho, Shippori Mincho 등)도 사용합니다.
각 폰트의 출처와 SIL OFL 1.1 라이선스는 해당 저장소의
[ATTRIBUTION.md](https://github.com/storytold/craft-fonts/blob/main/ATTRIBUTION.md)에 있습니다. 아이콘은 모두 자체 제작했습니다.

[`docs/brand/`](docs/brand/)의 ArtCraft 이름·워드마크·로고는 ArtCraft Team의 상표이며
위 오픈소스 라이선스에 포함되지 않습니다.
[브랜드 이용 조건](docs/brand/LICENSE-brand.txt)에 따라 이 저장소와 LightCraft의 일부로만,
수정하지 않은 상태로 사용할 수 있습니다. 포크와 수정판에서는 제거해야 합니다.

<p align="center">
  <a href="https://getartcraft.com/"><img alt="ArtCraft" src="docs/brand/artcraft-mark.svg" width="28"></a><br>
  <sub><a href="https://getartcraft.com/">ArtCraft</a> 팀과 커뮤니티가 함께 만듭니다.</sub>
</p>

<a id="star-history"></a>
## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=storytold/lightcraft&type=Date&legend=top-left)](https://www.star-history.com/?repos=storytold%2Flightcraft&type=date&legend=top-left)

Library·Tools·Presets 패널은 묶기·분할·앱 내부 플로팅·닫기·다시 열기를 지원합니다.
Tools 패널은 현재 Edit/Crop/Masking 모드를 유지하며, 사진 보기와 필름스트립은 함께 이동합니다.
패널 제목을 다른 제목이나 가장자리로 드래그해 배치를 바꿀 수 있습니다.
제목의 컨텍스트 메뉴에는 Float panel·Move to group·Close panel이 있습니다.
패널 표시를 다시 켜면 저장된 위치에서 열리고, Reset Panel Layout으로 초기 배치로 돌아갑니다.
플로팅 패널은 현재 앱 창 안에만 표시됩니다.
