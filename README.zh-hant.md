<p align="center">
  <a href="https://getartcraft.com/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="docs/brand/artcraft-logo-white.svg">
      <img alt="ArtCraft" src="docs/brand/artcraft-logo.svg" width="200">
    </picture>
  </a>
</p>


<h1 align="center">LightCraft</h1>

<h3 align="center">Your photos. Your pixels. Your machine.</h3>

<p align="center">
  <a href="README.md">English</a> ·
  <a href="README.zh-hans.md">简体中文</a> ·
  <a href="README.zh-hant.md">繁體中文</a>
</p>

<p align="center">
  <b>照片庫與 raw 顯影；以淨室（clean-room）方式、用純 Rust 重新實作的開源 Adobe Lightroom。</b><br>
  在 macOS、Windows 與 Linux 上原生執行，也能透過 WebAssembly 在瀏覽器中使用，AI 代理也能透過 MCP 全程操作。
</p>

<p align="center">
  <img alt="Pure Rust" src="https://img.shields.io/badge/pure-Rust-f2a516?style=flat-square&logo=rust&logoColor=white">
  <img alt="macOS, Windows, Linux and Web" src="https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux%20%C2%B7%20Web-8a5800?style=flat-square">
  <img alt="MCP server included" src="https://img.shields.io/badge/MCP-ready-8a5800?style=flat-square">
  <img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-8a5800?style=flat-square">
  <a href="ROADMAP.md"><img alt="Status: young and moving fast" src="https://img.shields.io/badge/status-young%20%26%20moving%20fast-f2a516?style=flat-square"></a>
</p>

<p align="center">
  <a href="https://discord.gg/artcraft"><img alt="Join the ArtCraft community on Discord" src="https://img.shields.io/badge/Join%20us%20on%20Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white" height="40"></a>
</p>

<p align="center">
  <a href="https://getartcraft.com/apps/lightcraft"><b>getartcraft.com 上的 LightCraft</b></a> ·
  <a href="https://getartcraft.com/">ArtCraft</a> ·
  <a href="https://getartcraft.com/apps">所有 Crafting Apps</a>
</p>

<br>

<p align="center">
  <img src="docs/images/hero-tetons.jpg" alt="LightCraft 的編輯檢視畫面：放大檢視中是安瑟·亞當斯的《提頓山脈與蛇河》，右側開啟著「光線」與「效果」面板，底片顯示窗格中是四張展示照片" width="100%">
  <br>
  <sub><i>安瑟·亞當斯，《提頓山脈與蛇河》（1942）。公眾領域，美國國家檔案館。於 LightCraft 中顯影。</i></sub>
</p>

> [!NOTE]
> **ArtCraft 是一個由各行各業的創作者組成的社群。** 數位藝術、生成藝術、音樂、
> 遊戲 &mdash; 只要你在創作，你就是我們的一員。**[來 Discord 打聲招呼吧](https://discord.gg/artcraft)。**

<p align="center">
  <a href="#edit-like-you-mean-it">編輯</a> ·
  <a href="#color-grading-the-cinematic-way">色彩分級</a> ·
  <a href="#before--after">編輯前／後對比</a> ·
  <a href="#masking-that-goes-where-you-point">遮色片</a> ·
  <a href="#presets-profiles--the-color-mixer">預設集</a> ·
  <a href="#organize-everything">照片庫</a> ·
  <a href="#built-for-agents">代理與 MCP</a> ·
  <a href="#fast-native-private">效能</a> ·
  <a href="#feature-status">功能狀態</a> ·
  <a href="#quick-start">快速開始</a> ·
  <a href="ROADMAP.md">路線圖</a> ·
  <a href="#downloads">下載</a> ·
  <a href="#the-crafting-apps">Crafting Apps</a>
</p>

<br>

## Edit like you mean it

LightCraft 是裝在單一原生應用程式裡的完整暗房。每一項調整都是**非破壞性**的，永遠不會動到原始檔案。
每個滑桿都經過一條**場景參考、寬色域、32 位元浮點的管線**算繪：亮部像底片一樣柔和過渡，陰影提亮卻不產生光暈，
色彩從拍攝到匯出始終保持乾淨。

<table>
<tr>
<td width="50%" valign="top">

### ☀️ 光線
**曝光、對比、亮部、陰影、白色色階、黑色色階**，搭配邊緣感知的局部色調映射（對對數亮度做引導式濾波）。
把亮部拉到 −100，就能救回一片過曝的天空，而且不會出現用簡單曲線時常見的灰色光暈。

### 🎨 色彩
依**色溫**與**色調**調整**白平衡**（raw 用 K 值，JPEG 用相對值），附帶預設、自動，以及點一下就能中和的**滴管**。
保護膚色的**自然飽和度**、**飽和度**、8 段式**混色器**（色相／飽和度／明度），以及附帶混合與平衡的
三區**色彩分級**色輪。所有運算都在 OkLCh 這個現代感知色彩空間中完成。

</td>
<td width="50%" valign="top">

### ✨ 效果
讓細節更細緻的**紋理**、讓中間調更有力的**清晰度**、**去朦朧**（暗通道先驗加引導式細化；
推到負值可增添氛圍感）、裁切後附帶亮部優先、圓度與羽化的**暗角**，以及解析度無關、可調大小與粗糙度的
底片**顆粒**。

### 📈 色調曲線
可移動分界點的參數曲線（區域曲線），**再加上** RGB、紅、綠、藍的點曲線。曲線在結構上就是單調的，
所以不會意外出現色調反轉。

</td>
</tr>
</table>

<p align="center">
  <img src="docs/images/curve-tetons.jpg" alt="「光線」面板下方開啟的色調曲線，RGB 點曲線上是一條平緩的 S 形曲線，套用在《提頓山脈與蛇河》上" width="100%">
  <br>
  <sub>RGB 點曲線上一條平緩的 S 形曲線，就在「光線」滑桿正下方。<i>安瑟·亞當斯，1942（公眾領域）。</i></sub>
</p>

<br>

## Color grading, the cinematic way

用可隨處拖曳的色輪，分別為陰影、中間調與亮部做分離色調。下圖中，桃樂絲·蘭格的
*《移民母親》* 只拖曳兩次，就調出溫暖、如沖印照片般的色調（亮部 42°，陰影 28°）。

<p align="center">
  <img src="docs/images/grading-migrant-mother.jpg" alt="中間調、陰影與亮部的色彩分級色輪，旁邊是桃樂絲·蘭格的《移民母親》，調成溫暖的沖印色調" width="100%">
  <br>
  <sub>「色彩」面板中的陰影、中間調與亮部色輪。<i>桃樂絲·蘭格，《移民母親》（1936）。公眾領域，美國國會圖書館。</i></sub>
</p>

<br>

## Before & after

按住 <kbd>\\</kbd> 偷看原圖，按 <kbd>Y</kbd> 並排比較，或按 <kbd>Shift</kbd>+<kbd>Y</kbd> 分割檢視。
下面每一張都是 LightCraft 的真實截圖，由代理透過[控制通道](#built-for-agents)自動擷取。

<table>
<tr>
<td width="50%"><img src="docs/images/ba-tetons.jpg" alt="《提頓山脈與蛇河》的並排編輯前／後對比：編輯後的雲層更深，河岸邊的暗部更通透"><br><sub><b>《提頓山脈與蛇河》。</b> 亮部 −45、陰影 +38、清晰度 +28、去朦朧 +18。<i>安瑟·亞當斯，1942（公眾領域）。</i></sub></td>
<td width="50%"><img src="docs/images/ba-migrant-mother.jpg" alt="《移民母親》的並排編輯前／後對比：編輯後更溫暖，陰影被提亮"><br><sub><b>《移民母親》。</b> 陰影 +42、紋理 +18、分離色調分級、暗角。<i>桃樂絲·蘭格，1936（公眾領域）。</i></sub></td>
</tr>
<tr>
<td width="50%"><img src="docs/images/ba-earthrise.jpg" alt="《地出》的並排編輯前／後對比：月球地平線上方是地球，編輯後略暖、更濃郁"><br><sub><b>《地出》。</b> 去朦朧 +22、亮部 −30、更暖的白平衡、自然飽和度 +22。<i>NASA／比爾·安德斯，阿波羅 8 號，1968（公眾領域）。</i></sub></td>
<td width="50%"><img src="docs/images/ba-blue-marble.jpg" alt="《藍色大理石》的並排編輯前／後對比：編輯後黑色更深，雲層細節更扎實"><br><sub><b>《藍色大理石》。</b> 亮部 −38、黑色色階 −20、去朦朧 +15、自然飽和度 +30。<i>NASA，阿波羅 17 號，1972（公眾領域）。</i></sub></td>
</tr>
</table>

<br>

## Masking that goes where you point

用**筆刷**塗抹（大小、羽化、流量、濃度、擦除），放置附可拖曳控制點的**線性漸層**與**放射狀漸層**，
或依**明度範圍**、**色彩範圍**、**天空**、**主體**與**背景**來選取。用**新增／減去／交集**組合各個元件，
反轉其中任何一個，每個遮色片可設定 15 項局部調整（色溫、色調、曝光、對比、亮部、陰影、白色色階、黑色色階、紋理、
清晰度、去朦朧、色相、飽和度、銳利化、雜訊抑制），再加上一個整體的強度。

<p align="center">
  <img src="docs/images/masking.jpg" alt="遮色片面板中有一個線性天空遮色片與一個放射狀陽光光暈遮色片；放射狀漸層在湖景照片上以紅色疊加層畫在太陽周圍" width="100%">
  <br>
  <sub>疊加在線性「天空」遮色片之上的放射狀「陽光光暈」遮色片（色溫 +40、曝光 +0.50）。<i>照片來自 LightCraft 程式產生的示範照片庫。</i></sub>
</p>

<br>

## Presets, profiles & the Color Mixer

內建十八個手工打造的預設集（*黃金時刻、青綠與橙色、褪色霧面、硒調色、清晰風景*等），每個都有 0 到 200 % 的**強度**滑桿。你可以從任意一組設定儲存自己的預設集、加入喜好項目，
並在整個選取範圍中，只複製、貼上或同步你選定的那些群組。

<p align="center">
  <img src="docs/images/presets-mixer.jpg" alt="預設集欄依黑白、色彩、底片、風景、人像與樣式分組，旁邊是「色彩」面板，8 段式混色器開啟在紫色上" width="100%">
  <br>
  <sub>預設集欄與 8 段式混色器並排。<i>照片來自示範照片庫。</i></sub>
</p>

<table>
<tr>
<td width="50%" valign="top">

### ✂️ 裁切、拉直與幾何
自由或鎖定的長寬比（1:1、4:5、5:7、2:3、4:3、16:9、16:10、原始比例），拖曳旋轉拉直時會始終把最大的裁切框
保持在畫面內，三分法／格線／黃金比例裁切疊加，翻轉與 90° 旋轉，再加上手動的垂直、水平、旋轉、長寬比、
縮放與偏移變換。

</td>
<td width="50%" valign="top">

### ⚫️ 黑白
一鍵（<kbd>V</kbd>）轉為單色，搭配 8 段式**黑白混色**，可壓暗天空或讓樹葉發亮。
之後再用色彩分級調出硒調色、深褐色調或分離色調的沖印效果。

</td>
</tr>
<tr>
<td width="50%"><img src="docs/images/crop-tetons.jpg" alt="《提頓山脈與蛇河》上的裁切工具：一個略微旋轉的 16:9 裁切框，附三分法裁切疊加，右側是幾何滑桿"><br><sub>一個 16:9 的裁切框拉直了 +2.5°，附三分法裁切疊加。<i>安瑟·亞當斯，1942（公眾領域）。</i></sub></td>
<td width="50%"><img src="docs/images/bw-split.jpg" alt="沙丘的編輯前／後分割檢視：左邊是彩色，右邊是附顆粒與暗角的黑白"><br><sub>分割檢視：左邊是彩色原圖，右邊是附清晰度、暗角與顆粒的黑白。<i>示範照片庫。</i></sub></td>
</tr>
</table>

<br>

## Organize everything

一個不擋路的照片庫：**所有照片**、**最近加入**、**精選照片**、**依日期**、以巢狀收在**資料夾**裡的**相簿**，
以及**最近刪除**。用 <kbd>0</kbd>–<kbd>5</kbd> 評分，用 <kbd>P</kbd> / <kbd>X</kbd> /
<kbd>U</kbd> 加旗標，用 <kbd>6</kbd>–<kbd>9</kbd> 加色彩標籤。搜尋支援欄位語法：
`rating:>3 flag:pick iso:>800 camera:x2 date:2026-04 keyword:mountains`。每個檢視都能依拍攝日期、
匯入日期、編輯日期、名稱、評分、大小或隨機排序（一種穩定的亂序；「檢視 → 排序 → 重新隨機排序」可換一個順序）。
左右對齊的**照片格狀檢視**與**方格檢視**都經過虛擬化處理，
所以無論是四十張還是四萬張照片都一樣流暢。

<table>
<tr>
<td width="50%"><img src="docs/images/grid-demo.jpg" alt="24 張示範照片的左右對齊照片格狀檢視，附評分與旗標，側邊欄是 Travel 2026 資料夾下的巢狀相簿"><br><sub>照片格狀檢視，相簿以巢狀收在資料夾裡，附評分與旗標。<i>示範照片庫。</i></sub></td>
<td width="50%"><img src="docs/images/grid-pd.jpg" alt="方格檢視，顯示四張公眾領域展示照片及其星級評分與精選旗標"><br><sub>四張公眾領域展示照片的方格檢視。</sub></td>
</tr>
<tr>
<td colspan="2"><img src="docs/images/info-earthrise.jpg" alt="《地出》的資訊面板，顯示檔名、2400 × 2400 的 JPEG 尺寸、評分、標題欄位與相機中繼資料"><br><sub>資訊面板：檔案、尺寸、評分、標題、圖說、著作權與相機中繼資料。<i>NASA／比爾·安德斯，《地出》，阿波羅 8 號，1968（公眾領域）。</i></sub></td>
</tr>
</table>

<br>

## Built for agents

LightCraft 裡的每一個選單項目、滑桿、筆刷筆觸、裁切控點與按鍵，都是一個擁有穩定 ID 與
JSON 參數的**指令**。介面、鍵盤、CLI、JSON-lines 控制通道與一個 **MCP 伺服器**全都透過同一個入口分派。
代理可以挑片、顯影、為天空加上遮色片並匯出，而且能*看到*結果。

```sh
lightcraft --control 7980 ~/Pictures/trip
```
```jsonc
{"method": "engine.execute", "params": {"command": "photo.flag",  "params": {"flag": "pick"}}}
{"method": "engine.execute", "params": {"command": "develop.set", "params": {"values": {"light.highlights": -45, "light.shadows": 38}}}}
{"method": "engine.execute", "params": {"command": "mask.add",    "params": {"kind": "radial", "center": [0.62, 0.4], "rx": 0.2, "ry": 0.14}}}
{"method": "ui.clickWidget",   "params": {"id": "slider:effects.clarity"}}       // 依名稱操作任意控制項
{"method": "ui.pointer",       "params": {"events": [{"kind":"down","x":0.2,"y":0.3}, {"kind":"up","x":0.4,"y":0.3}]}}
{"method": "ui.screenshot",    "params": {"path": "after.png"}}
```

- **指令註冊表。** `engine.commands` 列出可用的指令；`develop.controls` 列出每個滑桿的範圍、
  預設值與目前值。
- **MCP 伺服器。** `lightcraft-cli mcp` 把指令註冊表開放給 Claude（或任何 MCP 用戶端），
  同時提供匯入、查詢、顯影、遮色片、算繪（以影像回傳）與匯出的輔助工具。它可以無介面執行，也可以附掛
  到正在執行的應用程式上，支援截圖、點擊與手勢。詳見 [docs/mcp.md](docs/mcp.md)。

  ```sh
  cargo build --release -p lightcraft-cli
  claude mcp add lightcraft -- "$PWD/target/release/lightcraft-cli" mcp ~/Pictures/shoot          # 無介面
  claude mcp add lightcraft-app -- "$PWD/target/release/lightcraft-cli" mcp --connect 127.0.0.1:7980  # 操作執行中的應用程式
  ```
- **可腳本化的 CLI：** `lightcraft-cli run --import in.dng develop.set control=light.exposure value=0.7 app.export
  path=out.jpg longEdge=2048` 可執行任意指令鏈（無介面、對已儲存的照片庫，或對執行中的應用程式），
  每條指令印出一行 JSON 結果；`lightcraft-cli render in.dng -o out.jpg --set light.exposure=0.7 --preset …`。
- **一切皆可復原**，包括代理的操作：一次滑桿拖曳（或腳本中一連串的更新）算一步復原。
- **縮圖目錄（原生應用程式／CLI）：** 選取照片，然後 **檔案 → 縮圖目錄 PDF…**。可選擇 A4 或 Letter、橫向、
  列數與欄數，以及檔名圖說；儲存分頁 PDF，再用你的 PDF 檢視器列印。匯出會套用目前的編輯，每張照片完整放入
  而不再次裁切，以 150 dpi 和 sRGB 算繪。背景匯出支援取消，並保護原始檔案與 XMP 附屬檔案。從 CLI 呼叫：

  ```sh
  lightcraft-cli run --import ~/Pictures/shoot library.selectAll export.contactSheet path=Contact.pdf paper=a4 columns=3 rows=4 captions=true
  ```

  `export.contactSheet` 也接受明確的 `ids`、`landscape=true` 和 `paper=letter`。只有在整份 PDF 成功產生之後，
  它才會取代既有的輸出檔案。限制：1,000 張照片、100 頁、256 MiB；圖說使用與浮水印相同的字型涵蓋範圍來點陣化
  （CJK 文字需要 craft-fonts），過長的名稱會在中間省略。
  這是一套縮圖目錄工作流程，
  不是印表機驅動程式或自訂拼版編輯器。
- **列印版面（原生應用程式／CLI）：** **檔案 → 列印…** 會開啟「列印」模組的版面編輯器：單張照片、
  縮圖目錄、相片包（自動版面會把格子排進頁面）與自訂相片包（拖曳並調整格子大小、堆疊順序、對齊格子或
  格線），縮放填滿／旋轉以符合、相片邊框與內框線，紙張尺寸從 4 × 6 到 13 × 19 以及 A3 到 A5 或自訂，
  邊界、以英吋／公分／公釐／點為單位的尺規、頁面背景、72–720 ppi，以及已儲存的範本。**列印為 PDF…**
  會在背景算繪。一切都是 `print.*` 指令
  （`print.get`、`print.set`、`print.cell.add`、`print.layout.autoLayout`、`print.exportPdf`、
  `print.template.save`……），所以 CLI 和 MCP 也能操作它：

  ```sh
  lightcraft-cli run --import ~/Pictures/shoot print.set 'layout={"style":"picturePackage","page":{"paper":"8x10"}}' \
      print.cell.add width=5 height=7 print.cell.add width=2.5 height=3.5 print.layout.autoLayout print.exportPdf path=Package.pdf
  ```

  印表機色彩管理與頁面疊加層（身分標識、浮水印、頁碼）目前還沒有。
- **每個控制項都可定址**（`ui.widgets`）且可依名稱點擊，所以代理操作的是真實介面，而不是走旁門。
- 本 README 中的截圖全部由 [`docs/showcase/`](docs/showcase/) 腳本全自動產生。
  協定參考：[docs/control-protocol.md](docs/control-protocol.md)。

<br>

## Fast, native, private

- **純 Rust，無 C。** 我們自己的 RAW 解碼器（DNG、Canon CR2/CR3、Sony ARW、Nikon NEF、Fujifilm RAF 含 X-Trans、
  Panasonic RW2／Leica RWL、Pentax PEF、Olympus ORF），我們自己的色彩科學，我們自己的管線。JPEG、PNG、TIFF、WebP、
  PSD 合成圖與 JPEG XL 現在就能開啟；加上 `--features heif` 還能開啟 HEIC/HEIF 照片（HEVC 於建置時選用，
  與 PhotoCraft 中一致）。
- **場景參考與寬色域。** 內部使用線性 Rec.2020 浮點、Bradford 色彩適應的白平衡、以色域映射取代裁剪，
  raw 走底片式肩部曲線，未經改動的 JPEG 則採像素級直通。
- **解析度無關的編輯。** 半徑與筆刷大小都相對於影像，所以 400 px 的預覽、你的 5K 顯示器和 60 MP 的匯出
  看起來完全一致。
- **GPU 加速，與 CPU 結果一致。** 整條顯影管線都以 wgpu 計算核心執行（Metal／Vulkan／DX12），
  並與 CPU 管線比對到 1/255 以內的誤差。在 24 MP 的 raw 上（Apple M4 Pro）：一次滑桿更新約 4 ms 重新算繪，
  冷啟動的 2.5 MP 放大檢視約 30 ms，全尺寸匯出約 0.3 秒（含平行 JPEG 編碼）。
  沒有 GPU 時，同一條管線會在所有 CPU 核心上執行，只重做受滑桿影響的那些階段。批次匯出會同時算繪多張照片，
  所以一張的解碼與編碼能和另一張的算繪重疊。
- **即時挑片。** 開啟一張 raw 會在約 0.1 秒內顯示其內嵌的相機預覽或快取算繪結果，完整算繪隨後跟上
  （24 MP 約 0.2–0.5 秒）。上一張與下一張會在背景準備好，因此瀏覽整批照片時每張約 50 ms。
- **背景算繪。** 一個工作執行緒池在 UI 執行緒之外算繪放大檢視、編輯前／後對比與每一個可見縮圖：
  拖曳時用草稿品質，放開後以完整品質算繪。
- **本機優先。** 沒有帳號、沒有雲端、沒有遙測、沒有訂閱。你的目錄是一份只會附加寫入、可讀的操作紀錄，
  可以比對差異、備份或重播。

<br>

## Feature status

LightCraft 還很年輕，而且進展飛快：**Alpha 階段**。**老實說，我們目前的進度是**（細節見[路線圖](ROADMAP.md)和
[docs/target-app-parity.md](docs/target-app-parity.md)）：

- **依功能數量計算，約達 Lightroom 的 81%**（核心功能 98%），逐行記錄在
  [docs/parity-checklist.md](docs/parity-checklist.md)。
- **作為日常取代 Lightroom 的工具，整體產品更接近 61%，對一般全職攝影師約 51%，一般休閒用途約 65%。**
  在單機上處理 JPEG/DNG，以及大多數 Sony／Panasonic／
  Fujifilm／Nikon／較舊款 Canon 的 raw 都很出色；各機型詳情見 [docs/raw-parity.md](docs/raw-parity.md)。
- **最大的缺口：**
  - **相機色彩校正：** Sony、Nikon、Panasonic、Fujifilm 和 Canon CR3 的 raw 會從各自的相機 JPEG 得到受保護的估計值，內建 ILCA-99M2、ILCE-7CR、ILCE-7M4、ILCE-7RM5、X-H2S、X-T4、Nikon Z 6II、Z 8 和 D5300 描述檔；仍缺少實測校正，其他 raw 或被拒絕的擬合會保留中性矩陣；
  - **壓縮的 Olympus raw 與不受支援的 CR3 變體：** 這些在有內嵌 JPEG 預覽時會改用預覽畫面。Fujifilm 無損／有損壓縮 RAF 現在能解碼感光元件資料；[驗證與現有照片庫重新載入說明](docs/raf-compression.md)；
  - **AI 遮色片與降噪：** 主體與天空選取目前是傳統啟發式演算法；
  - **網路照片庫：** 預覽只讀取 raw 的標頭 — 或者，對於 CR3 或預覽標籤位於其後的 TIFF raw，只讀取稀疏規劃器指定的範圍 — 以及其內嵌的 JPEG（56.5 MB → 5.9 MB，10 秒 → 1 秒，測得於測試用的網路共用資料夾），但*顯影後*的畫面仍需要透過網路傳輸感光元件資料；詳見 [docs/raw-preview-reads.md](docs/raw-preview-reads.md)；
  - **螢幕上的 HDR 顯示、影片，以及 Lightroom Classic 的列印、書冊與地圖模組。**
- **接下來做什麼：** 見[依影響力排序的缺口](docs/gaps.md)和[路線圖](docs/roadmap.md)。

| 領域 | 狀態 |
|---|---|
| 照片庫：相簿、資料夾、智慧型相簿、堆疊（含自動堆疊）、虛擬副本、評分、旗標、標籤、篩選列、搜尋、排序、格狀檢視、底片顯示窗格 | ✅ |
| 挑片：比較（同步縮放）與總覽檢視、自動前進、即時預覽 | ✅ |
| 光線、色彩、效果（暗角樣式）、色調曲線（+ 細化飽和度、目標調整）、混色器（+ 目標調整）、點色彩、色彩分級、校正、黑白 | ✅ |
| 遮色片：筆刷、線性、放射狀、明度／色彩範圍、新增／減去／交集 | ✅（AI 主體／天空目前使用傳統啟發式演算法） |
| 裁切、拉直工具 + 自動拉直、翻轉、旋轉、長寬比、裁切疊加參考線 | ✅ |
| 描述檔（色彩、中性、鮮豔、風景、人像、單色：我們自己的風格）、預設集、版本、歷史紀錄、複製／貼上／同步設定 | ✅ |
| 相機色彩：DNG 檔案使用自帶矩陣 | ✅ DNG · 🚧 自家 Sony／Fujifilm 描述檔；缺少實測校正資料庫 |
| 原生 macOS 選單列（由指令註冊表產生）、控制通道 + 每個控制項可定址、無介面 UI 快照 | ✅ |
| RAW：DNG、CR2、CR3（無損 CRX Bayer 以及版本 0x100/0x200 的 C-RAW）、ARW、NEF（未壓縮 + 無損／有損壓縮，含有損壓縮經分割後的格式）、Fujifilm RAF（未壓縮 + 無損／有損壓縮，Bayer + X-Trans）、Panasonic RW2／Leica RWL／Panasonic RAW（所有 raw 格式，從 DMC-LX1 到 DC-S1RM2）、Pentax PEF、Olympus ORF（未壓縮，12.8 bpp）、Samsung SRW（未壓縮）；所有格式皆支援內嵌預覽，含 CR3 | 🚧 · 其他 CR3 變體、壓縮 ORF／SRW 和 Nikon HE NEF 解碼 ⬜（[raw 相容度](docs/raw-parity.md)） |
| 細節：銳利化、明度 + 色彩降噪 | ✅ · AI 降噪 🚧（Bayer，使用者自行安裝權重）、超解析度 ⬜ |
| 斑點移除／修復／仿製（自動取樣來源）、顯示斑點、紅眼與寵物眼校正（自動瞳孔偵測、眼神光） | ✅ · 內容感知填色、斑點圖釘編輯 🚧 |
| 匯出：JPEG／PNG／TIFF／WebP／AVIF／DNG／原始格式，尺寸調整、檔案大小上限、輸出銳利化、命名範本、批次處理、中繼資料政策、文字或影像浮水印 | ✅ |
| HDR：附餘裕上限的 HDR 編輯、SDR 呈現、視覺化 HDR；匯出為 ISO 21496-1 增益圖 JPEG、PQ AVIF 或 32 位元浮點 TIFF | ✅ · HDR 顯示 ⬜ |
| 照片庫持久化（防當機的操作紀錄 + 快照、背景壓縮、儲存失敗會回報）、磁碟縮圖快取 | ✅ |
| 匯入：原位置加入／複製／移動、重新命名與資料夾範本、裝置、重複檢測、監看資料夾；本機資料夾瀏覽 | ✅ |
| MCP 伺服器（無介面或執行中的應用程式、持久化照片庫）、CLI、控制通道 | ✅ |
| XMP 附屬檔案（讀取／寫入、自動寫入）、讀取 `crs:` 顯影設定、預設集檔案（`.lcpreset`、XMP 預設集） | ✅ |
| 鏡頭校正（變形、暗角、自動 + 手動色差、去除邊緣色差、內嵌於 DNG 檔案的鏡頭校正資料、Panasonic／Leica RW2／RWL 變形資料以及 Nikon Z 6II／Z 8／Z f 變形資料）、幾何（變換、限制裁切範圍）、透視校正（自動／水平／垂直／完整／引導式） | ✅ · 相機鏡頭描述檔（我們自己的）⬜ |
| 照片合併：HDR（自動對齊、去除殘影）、全景（球面／圓柱／透視、邊界變形、自動裁切）、HDR 全景 → DNG | ✅ |
| GPU 管線（wgpu 計算，與 CPU 一致到 1/255 以內）、裝置限制／發生錯誤時改用 CPU | ✅ · 瀏覽器中的 WebGPU 🚧 |
| AI：分割遮色片、AI 降噪、超解析度、人臉；影片 | 🚧（見[缺口清單](docs/gaps.md)） |
| 網頁版（透過 WASM 在瀏覽器中使用同一套介面）：照片庫持久化於 OPFS/IndexedDB、Web Worker 算繪、匯出下載 | ✅ · WebGPU、Safari/Firefox 測試 🚧 |

<sub>✅ 現已可用 · 🚧 進行中 · ⬜ 尚未開始</sub>

<br>

## Quick start

建置整個工作區（含桌面、網頁與 CLI 應用程式）需要 Rust 1.95 或更新版本。

```sh
git clone https://github.com/storytold/lightcraft && cd lightcraft
cargo run --release -p lightcraft                       # 開啟你的照片庫（~/Pictures/LightCraft Library；新照片庫會先帶示範照片）
cargo run --release -p lightcraft -- ~/Pictures/trip    # 匯入你的照片（掃描資料夾，跳過重複項目）
cargo run --release -p lightcraft -- --memory           # 用完即丟的記憶體示範工作階段（不寫入任何內容）
cargo run --release -p lightcraft -- --control 7980     # 附帶自動化通道
cargo xtask web --serve                                 # 在瀏覽器中執行同一個應用程式：http://127.0.0.1:8080/
cargo run --release -p lightcraft-cli -- render photo.jpg -o out.jpg --set light.exposure=0.5
cargo xtask ci                                          # fmt、clippy、測試、分層、wasm 檢查
```

CI 預設使用行表除錯資訊，並依啟動時可用記憶體每 1.5 GB 開一個建置工作
（每個 CPU 最多一個；讀不到記憶體時為 4 個），所以一台 8 GB、剩餘 6 GB 可用的機器會用 4 個任務建置，
而一台忙碌、剩餘記憶體很少的機器會用更少。測試執行緒數隨之調整，上限為 4。若不限制，每個 CPU 各跑
一個帶完整除錯資訊的連結器，測試還沒開始記憶體就會耗盡。GPU 測試的涵蓋範圍不受影響。明確設定
`CARGO_PROFILE_DEV_DEBUG`、`CARGO_BUILD_JOBS`、`RUST_TEST_THREADS` 與 `NEXTEST_TEST_THREADS` 會覆寫這些預設值。
日常開發指令保留各自的設定。

**記憶體受限機器上的 release 建置：** [#1196](https://github.com/storytold/lightcraft/issues/1196)
回報在一台 16 GB 的 Arch/AUR 機器上，同時建置桌面應用程式與 CLI，並啟用完整除錯資訊、thin LTO
及單一程式碼產生單元時，連結過程遭到 OOM 終止。上述 CI 預設值並不適用於一般的
`cargo build --release`。建議先用一個建置工作；若記憶體充足，可嘗試兩個：

```sh
CARGO_BUILD_JOBS=1 cargo build --release -p lightcraft -p lightcraft-cli
# 或者：CARGO_BUILD_JOBS=2 cargo build --release -p lightcraft -p lightcraft-cli
```

如果打包環境啟用了完整除錯資訊或較耗記憶體的 LTO 設定，可以使用僅對本條指令生效的
覆寫設定，降低原始碼建置所需的記憶體：

```sh
CARGO_BUILD_JOBS=1 CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo build --release -p lightcraft -p lightcraft-cli
```

這些 POSIX shell 賦值只影響這一條指令，不會變更全域 Cargo 設定或儲存庫的 release profile。
停用 LTO 可能改變效能與二進位檔大小；停用除錯資訊會減少可用的除錯細節。這是建置建議，
並不保證任何機器都有足夠的記憶體。下游打包者應逐一建置大型二進位檔、不要同時進行，並避免在低記憶體
的建置機器上同時使用完整除錯資訊、LTO 與單一程式碼產生單元。自行匯出 profile 設定的
PKGBUILD 可能會覆寫繼承的環境變數；請在本機調整打包腳本或聯絡維護者，而不要假設替
`makepkg` 加上環境變數前綴就能優先生效。

測試在缺少 GPU、語料庫、craft-fonts 字型或模型時會直接通過而不檢查任何內容（libtest 沒有「已跳過」
這個狀態），所以 `cargo xtask ci` 會把它們列出來：測試會用 `lightcraft_testkit::skip!` 記錄跳過原因，
執行結束時會列出機器提供的環境（作業系統、CPU、記憶體、每個後端的 GPU 配接器、語料庫、字型）以及每一項
跳過的原因。它會寫入 `target/ci/report.json` 和 `target/ci/summary.md`，後者是可直接貼進 PR 的
Markdown 區塊（`--summary-md` 會把它印出來）；`--hardware` 還會在每個有實體配接器的後端上執行
GPU 測試。不會上傳任何內容。

**中文與日文文字**需要共用字型儲存庫，這是一個可選的建置輸入（所有正式發布版都會包含它）：

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
CRAFT_FONTS_DIR=../craft-fonts cargo run --release -p lightcraft
```

不指定它時，LightCraft 一樣能建置與執行，只是中文和日文文字沒有字形。字型從不提交到
本儲存庫，字型的收錄規範記載在該儲存庫：見 [craft-fonts README](https://github.com/storytold/craft-fonts#readme)。

**介面語言：** **編輯 → 語言**（English、简体中文、繁體中文（台灣）、日本語、Português (Brasil)、Español、Deutsch、Русский、Français、Українська、Čeština、ქართული）或 **設定 → 一般 →
語言**；選擇後立即生效並會保留。詳見 [docs/localization.md](docs/localization.md)。

**記錄檔：** 桌面應用程式會把記錄檔寫到標準錯誤輸出，以及設定資料夾裡的 `logs/lightcraft.log`
（Linux `$XDG_CONFIG_HOME/lightcraft/logs/`，預設為 `~/.config/lightcraft/logs/`；macOS
`~/Library/Application Support/LightCraft/logs/`；Windows `%APPDATA%\LightCraft\logs\`），絕不會寫進照片庫裡。
**可攜模式：** 在執行檔旁放一個名為 `portable.txt`（或 `LightCraft.portable`）的檔案，會把設定
資料夾移到執行檔旁的 `LightCraftData`。從桌面選單或 Dock 啟動沒有終端機，所以請把這個檔案附在
錯誤回報裡（**說明 ▸ 開啟記錄資料夾** 會在檔案管理員中顯示它）。每次啟動會把
上一個記錄檔移到 `lightcraft.1.log`，再上一個移到 `lightcraft.2.log`，這樣當機那次的記錄檔就能在下次啟動後
保留下來；檔案到 16 MiB 就會停止增長，`--version` 和 `--help` 不會寫記錄檔，帶
`LIGHTCRAFT_NO_PREFS` 執行時只會寫到標準錯誤輸出。預設情況下，LightCraft 自己的 crate 會記錄到 `info`
層級，其他一切則記錄到 `warn`。`LIGHTCRAFT_LOG=info` 或 `debug` 的行為和以前一樣（LightCraft 自己的 crate
用該層級，其餘只記警告與錯誤；其他值：只記警告與錯誤），並且優先於 `RUST_LOG`；未設定 `LIGHTCRAFT_LOG` 時，`RUST_LOG` 會把預設值換成
env_logger 風格的指令，例如 `RUST_LOG=debug` 或 `RUST_LOG=warn,lightcraft_pipeline=trace`（以
`*` 結尾的指令會涵蓋所有以它開頭的目標，如 `lightcraft*=debug`）。panic 一樣會記錄在那裡，
並且仍然會寫在暫存資料夾裡的 `lightcraft-panics.log`。記錄器程式碼位於 `apps/lightcraft/src/logging.rs`。

網頁版需要 `wasm32-unknown-unknown` target 以及對應版本的 `wasm-bindgen` CLI
（`cargo xtask web` 會印出確切的安裝指令）；詳見 [docs/web.md](docs/web.md)。

**Nix** 會建置桌面應用程式與 `lightcraft-cli`（Nix 建置永遠包含 craft-fonts 輸入，所以日文
文字有字形）：

```sh
nix run github:storytold/lightcraft                    # 桌面應用程式
nix build github:storytold/lightcraft                  # → ./result/bin/{lightcraft,lightcraft-cli}
nix develop github:storytold/lightcraft                # rust 工具鏈 + 原生相依套件 + 字型
```

在 NixOS 上，請從 `nix develop` 執行 `cargo` 建置：在它之外，應用程式找不到 `libvulkan.so.1`，
偵測不到 GPU，會改用 CPU 算繪（啟動時會印出一次提示，說明這樣會慢很多）。`nix run` 和 `nix build`
會自行設定好路徑。

在 flake 設定中（NixOS、home-manager、nix-darwin）：

```nix
# flake.nix
inputs.lightcraft.url = "github:storytold/lightcraft";
# 選用：使用你自己的 nixpkgs，而不是 LightCraft 鎖定的那個
# inputs.lightcraft.inputs.nixpkgs.follows = "nixpkgs";

# 然後在 NixOS 或 home-manager 模組中（`inputs` 在作用範圍內）：
nixpkgs.overlays = [ inputs.lightcraft.overlays.default ];   # 讓 `pkgs.lightcraft` 可用
environment.systemPackages = [ pkgs.lightcraft ];           # home-manager：home.packages = [ pkgs.lightcraft ];
```

`nix build` 會安裝與 .deb/.rpm 相同的 desktop 檔案、hicolor 圖示和 AppStream 中繼資料，並執行
`cargo test --workspace` 作為其檢查階段（用 `pkgs.lightcraft.overrideAttrs { doCheck = false; }` 跳過）。

**鍵盤：** <kbd>G</kbd> 格狀檢視 · <kbd>D</kbd> 詳細檢視 · <kbd>E</kbd> 編輯 · <kbd>C</kbd> 裁切 · <kbd>M</kbd> 遮色片 ·
<kbd>Shift</kbd>+<kbd>P</kbd> 預設集 · <kbd>\\</kbd> 原圖 · <kbd>Y</kbd> 編輯前／後 · <kbd>Z</kbd> 縮放 ·
<kbd>J</kbd> 顯示溢色 · <kbd>⌘Z</kbd> 復原 · <kbd>⌘/</kbd> 全部快速鍵。

## How it's built

一個引擎優先的 Cargo 工作區，由一群小而經過測試的 crate 組成，並強制分層（`cargo xtask layers`）：`geom`、
`color`、`raster`、`tiff` → `raw`、`codecs`、`meta`、`develop` → `pipeline` → `catalog` → `engine` → `ui-egui`。
egui 前端是一個可替換的 crate；它之下沒有任何東西知道 UI 的存在。`mcp` 和各個應用程式（`lightcraft`、
`lightcraft-cli`）則坐落在 `engine` 之上。

## Contributing

人類與代理遵循同一套規則，所以請先讀 [AGENTS.md](AGENTS.md)。重點如下：

- **淨室。** 絕不閱讀 Adobe 的二進位檔或 GPL 的 raw／照片程式碼（darktable、RawTherapee、LibRaw、rawspeed、dcraw……）；
  只依據公開規格與黑盒觀察來工作。
- **絕不使用 Adobe 資產：** 不使用 Adobe 產品的圖示、截圖、預設集、描述檔、LUT 或字型。儲存庫中的每一張
  影像、圖示和字型都是原創、公眾領域、創用 CC、OFL 或寬鬆授權的，並在同一次提交中
  於 [assets/ATTRIBUTION.md](assets/ATTRIBUTION.md) 新增條目。新的字型要提交到
  [storytold/craft-fonts](https://github.com/storytold/craft-fonts)，而不是這裡。
- **純 Rust**、強制的 crate 分層、一切皆指令，並且每次提交前 `cargo xtask ci` 都必須通過
  （一次提交對應一個任務 ID）。
- **絕不當機。** 非測試程式碼要回傳錯誤而不是 panic：不使用 `unwrap()`、`expect()`、`panic!` 或
  `unsafe`，對任何來自輸入的資料都做受檢的索引存取，並且每一個當機修復都要附一個回歸測試。細節見
  [AGENTS.md](AGENTS.md#never-crash-outranks-feature-work)。

有問題、想法，或是想先討論一個 bug？來 [Discord](https://discord.gg/artcraft)。

<br>

## Downloads

**第一次使用 LightCraft？** 從 [getartcraft.com 的 LightCraft 頁面](https://getartcraft.com/apps/lightcraft)下載。那是最簡單的安裝方式。

**想要特定的建置版本或格式？** 在 GitHub 上，[最新發布版](https://github.com/storytold/lightcraft/releases/latest)包含下面列出的所有建置版本，[所有發布版](https://github.com/storytold/lightcraft/releases)則有更早的版本與其說明。檔名中的 `<ver>` 是版本號，`SHA256SUMS.txt` 列出了每個檔案的檢查碼。

### Windows

| 建置版本 | 安裝程式 | 可攜版 |
|---|---|---|
| x64（64 位元 Intel/AMD） | `lightcraft-<ver>-windows-x64.msi` | `lightcraft-<ver>-windows-x64-portable.zip` |
| arm64（Snapdragon 及其他 ARM PC） | `lightcraft-<ver>-windows-arm64.msi` | `lightcraft-<ver>-windows-arm64-portable.zip` |
| x86（32 位元） | `lightcraft-<ver>-windows-x86.msi` | `lightcraft-<ver>-windows-x86-portable.zip` |

安裝程式與執行檔都有程式碼簽章。安裝程式會詢問安裝位置（預設為
`C:\Program Files\LightCraft`；升級時會留在你選擇的資料夾），結束時會有一頁確認 LightCraft 已安裝，
並可選擇立即啟動。無訊息安裝：`msiexec /i lightcraft-<ver>-windows-x64.msi /qn INSTALLFOLDER="D:\Apps\LightCraft\"`。

### macOS

| 建置版本 | 檔案 | 備註 |
|---|---|---|
| 應用程式，通用版（Apple 晶片 + Intel） | `lightcraft-<ver>-macos-universal.dmg` | 已簽章並經過公證 |
| 命令列工具，通用版 | `lightcraft-cli-<ver>-macos-universal.zip` | 已簽章並經過公證 |

### Linux

| 格式 | x86_64 | aarch64（ARM64） | 備註 |
|---|---|---|---|
| AppImage | `lightcraft-<ver>-linux-x86_64.AppImage` | `lightcraft-<ver>-linux-aarch64.AppImage` | 隨處可執行；用 [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate)（`.zsync` 檔案）自我更新 |
| Flatpak | `lightcraft-<ver>-linux-x86_64.flatpak` | `lightcraft-<ver>-linux-aarch64.flatpak` | 沙箱化；`flatpak install --user <file>` |
| Debian/Ubuntu | `lightcraft-<ver>-linux-x86_64.deb` | `lightcraft-<ver>-linux-aarch64.deb` | |
| Fedora/RHEL/openSUSE | `lightcraft-<ver>-linux-x86_64.rpm` | `lightcraft-<ver>-linux-aarch64.rpm` | |
| 壓縮檔 | `lightcraft-<ver>-linux-x86_64.tar.gz` | `lightcraft-<ver>-linux-aarch64.tar.gz` | 解壓到任意位置即可使用 |

RISC-V（riscv64）：`lightcraft-<ver>-linux-riscv64.tar.gz`，僅提供壓縮檔（glibc 2.39 或更新版本，例如 Ubuntu 24.04+）。

在 Wayland 桌面環境中，LightCraft 會在可用時透過 XWayland 執行，因為只有 X11 支援
視窗的檔案拖放；設定 `LIGHTCRAFT_WAYLAND=1` 可改用原生 Wayland（更清晰的小數縮放，但無法拖放）。

### FreeBSD

| 建置版本 | 檔案 |
|---|---|
| x86_64 | `lightcraft-<ver>-freebsd-x86_64.tar.gz` |

### Web (WebAssembly)

| 建置版本 | 檔案 | 備註 |
|---|---|---|
| 靜態網站 | `lightcraft-web-<ver>.zip` | 可在現代瀏覽器中執行；可託管在任何靜態伺服器上 |

<br>

## The Crafting Apps

LightCraft 是 **Crafting Apps** 之一：來自 [ArtCraft](https://getartcraft.com/) 團隊的免費開源創作工具，
每一個都用 Rust 從零開始撰寫，每一個都能獨立使用。

| | 應用程式 | 用途 | 程式碼 | 了解更多 |
|:-:|---|---|---|---|
| <img src="https://raw.githubusercontent.com/storytold/photocraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.photocraft.png" alt="" width="32" height="32"> | **PhotoCraft** | 影像編輯：圖層、遮色片、文字和真正的 PSD 檔案 | [GitHub](https://github.com/storytold/photocraft) | [網站](https://getartcraft.com/apps/photocraft) |
| <img src="https://raw.githubusercontent.com/storytold/vectorcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.vectorcraft.png" alt="" width="32" height="32"> | **VectorCraft** | 向量插畫 | [GitHub](https://github.com/storytold/vectorcraft) | [網站](https://getartcraft.com/apps/vectorcraft) |
| <img src="https://raw.githubusercontent.com/storytold/filmcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.filmcraft.png" alt="" width="32" height="32"> | **FilmCraft** | 影片剪輯、調色與聲音 | [GitHub](https://github.com/storytold/filmcraft) | [網站](https://getartcraft.com/apps/filmcraft) |
| <img src="https://raw.githubusercontent.com/storytold/lightcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.lightcraft.png" alt="" width="32" height="32"> | **LightCraft** | **照片庫與 raw 顯影 · 你現在在這裡** | [GitHub](https://github.com/storytold/lightcraft) | [網站](https://getartcraft.com/apps/lightcraft) |
| <img src="https://raw.githubusercontent.com/storytold/pdfcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.pdfcraft.png" alt="" width="32" height="32"> | **PdfCraft** | 閱讀、整理與保護 PDF | [GitHub](https://github.com/storytold/pdfcraft) | [網站](https://getartcraft.com/apps/pdfcraft) |
| <img src="https://raw.githubusercontent.com/storytold/effectcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.effectcraft.png" alt="" width="32" height="32"> | **EffectCraft** | 動態圖形與視覺特效 | [GitHub](https://github.com/storytold/effectcraft) | [網站](https://getartcraft.com/apps/effectcraft) |
| <img src="https://raw.githubusercontent.com/storytold/designcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.designcraft.png" alt="" width="32" height="32"> | **DesignCraft** | 版面編排與出版 | [GitHub](https://github.com/storytold/designcraft) | [網站](https://getartcraft.com/apps/designcraft) |

還有 [**ArtCraft**](https://getartcraft.com/) 本身，我們為想要真正掌控創作的人打造的 AI 影像與影片工作室。

<br>

<p align="center">
  <a href="https://discord.gg/artcraft"><img alt="Join the ArtCraft community on Discord" src="https://img.shields.io/badge/Join%20us%20on%20Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white" height="40"></a>
</p>

<h3 align="center">Come make things with us</h3>

<p align="center">
  我們的 Discord 是各種創作者聚集的地方：畫畫的人、拍照的人、做動畫的人、剪片子的人、
  排版的人，還有仍在摸索自己喜歡做什麼的人。分享你在做的東西，
  尋求幫助，告訴我們哪裡壞了，或者告訴我們你希望這些工具能做到什麼。
  不管你用什麼媒材，也不管你做了多久，這裡都歡迎你。
</p>

<p align="center">
  <a href="https://discord.gg/artcraft"><b>discord.gg/artcraft</b></a> ·
  <a href="https://getartcraft.com/">getartcraft.com</a> ·
  <a href="https://getartcraft.com/apps">The Crafting Apps</a> ·
  <a href="https://getartcraft.com/apps/lightcraft">LightCraft</a>
</p>

<br>

## License and credits

LightCraft 採用 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 雙重授權，任你選擇。
Copyright (c) 2026 ArtCraft Team and the LightCraft contributors。必要的聲明見 [NOTICE](NOTICE)。

隨附的字型、圖示、影像與其他資產保留各自的開放授權；每一項都在
[assets/ATTRIBUTION.md](assets/ATTRIBUTION.md) 中列出了作者、來源與授權。

展示用照片皆為公眾領域作品，取自 Wikimedia Commons：安瑟·亞當斯，*《提頓山脈與蛇河》*
（1942，美國國家檔案館）；桃樂絲·蘭格，*《移民母親》*（1936，美國國會圖書館）；比爾·安德斯／NASA，
*《地出》*（1968）；NASA，*《藍色大理石》*（1972）。示範照片庫由 LightCraft 程式產生。介面字型：
Inter（SIL OFL 1.1）。使用 [craft-fonts](https://github.com/storytold/craft-fonts) 建置的版本（所有正式發布版）
還會內嵌其中文與日文字型（Noto Sans CJK SC、BIZ UDPGothic、BIZ UDMincho、Shippori Mincho；SIL OFL 1.1），列於其
[ATTRIBUTION.md](https://github.com/storytold/craft-fonts/blob/main/ATTRIBUTION.md) 中。所有圖示皆為原創。

[`docs/brand/`](docs/brand/) 中的 ArtCraft 名稱、字標與標誌是
ArtCraft 團隊的商標，不在本授權範圍內。它們只能以未經修改的形式，作為
本儲存庫與 LightCraft 的一部分，依 [`docs/brand/LICENSE-brand.txt`](docs/brand/LICENSE-brand.txt) 使用。
分支與修改版本必須移除它們。

<sub>Adobe、Photoshop、Illustrator、Premiere Pro、Lightroom、Acrobat、After Effects 與 InDesign 是 Adobe Inc. 在美國及／或其他國家的商標或註冊商標。LightCraft 是一個獨立的開源專案，與 Adobe Inc. 沒有任何關係，也未獲其贊助或認可；這些名稱僅用於說明它所相容的工作流程。</sub>

<p align="center">
  <a href="https://getartcraft.com/"><img alt="ArtCraft" src="docs/brand/artcraft-mark.svg" width="28"></a><br>
  <sub>由 <a href="https://getartcraft.com/">ArtCraft</a> 團隊與社群製作。</sub>
</p>

## Star history

[![Star History Chart](https://api.star-history.com/svg?repos=storytold/lightcraft&type=Date&legend=top-left)](https://www.star-history.com/?repos=storytold%2Flightcraft&type=date&legend=top-left)

照片庫、工具與預設集面板可以重新分組、拆分、浮動於應用程式視窗內、關閉與重新開啟。工具面板會保留目前的編輯／裁切／遮色片模式；照片檢視區與底片顯示窗格則維持在一起。拖曳面板標題到某個目標邊緣或另一個標題上即可調整。右鍵選單提供「浮動面板」、「移至群組」與「關閉面板」。既有的面板切換按鈕會在上次儲存的位置重新開啟面板；「重設面板配置」則會還原應用程式的版面配置。浮動面板會保持在目前的應用程式視窗之內。
