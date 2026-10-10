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
  <a href="README.zh-hans.md">简体中文</a>
</p>

<p align="center">
  <b>照片图库与 raw 显影；Adobe Lightroom 的开源净室实现，用纯 Rust 重建。</b><br>
  在 macOS、Windows 和 Linux 上原生运行，也可通过 WebAssembly 在浏览器中使用，并能由 AI 智能体经 MCP 端到端驱动。
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
  <a href="https://getartcraft.com/apps">全部 Crafting Apps</a>
</p>

<br>

<p align="center">
  <img src="docs/images/hero-tetons.jpg" alt="LightCraft 的编辑视图：放大视图中是安塞尔·亚当斯的《特顿山脉与蛇河》，右侧打开着 Light 与 Effects 面板，底部的胶片窗格中是四张展示照片" width="100%">
  <br>
  <sub><i>安塞尔·亚当斯，《特顿山脉与蛇河》（1942）。公有领域，美国国家档案馆。在 LightCraft 中显影。</i></sub>
</p>

> [!NOTE]
> **ArtCraft 是一个由形形色色的创作者组成的社区。** 数字艺术、生成艺术、音乐、游戏 &mdash;
> 只要你在创作，你就是我们中的一员。**[来 Discord 打个招呼吧](https://discord.gg/artcraft)。**

<p align="center">
  <a href="#edit-like-you-mean-it">编辑</a> ·
  <a href="#color-grading-the-cinematic-way">颜色分级</a> ·
  <a href="#before--after">修改前后对比</a> ·
  <a href="#masking-that-goes-where-you-point">蒙版</a> ·
  <a href="#presets-profiles--the-color-mixer">预设</a> ·
  <a href="#organize-everything">图库</a> ·
  <a href="#built-for-agents">智能体与 MCP</a> ·
  <a href="#fast-native-private">性能</a> ·
  <a href="#feature-status">功能状态</a> ·
  <a href="#quick-start">快速开始</a> ·
  <a href="ROADMAP.md">路线图</a> ·
  <a href="#downloads">下载</a> ·
  <a href="#the-crafting-apps">Crafting Apps</a>
</p>

<br>

## Edit like you mean it

LightCraft 是一个装在单个原生应用里的完整暗房。每一项调整都是**非破坏性**的，原始文件永远不会被改动。
每个滑块都经过一条**场景参考、宽色域、32 位浮点的管线**渲染：高光像胶片一样柔和滚降，阴影被提亮却不产生光晕，
色彩从拍摄到导出始终保持干净。

<table>
<tr>
<td width="50%" valign="top">

### ☀️ Light
**曝光（Exposure）、对比度、高光、阴影、白色色阶、黑色色阶**，配合边缘感知的局部色调映射（对对数亮度做引导滤波）。
把高光拉到 −100，就能救回一片过曝的天空，而且不会出现用简单曲线时会有的灰色光晕。

### 🎨 Color
按**色温**和**色调**调整**白平衡**（raw 用开尔文，JPEG 用相对值），带预设、自动，以及点击即可中和的**吸管**。
保护肤色的**鲜艳度**、**饱和度**、8 个色段的**颜色混合器**（色相／饱和度／亮度），以及带混合与平衡的
三路**颜色分级**色轮。所有计算都在 OkLCh 这个现代感知色彩空间里完成。

</td>
<td width="50%" valign="top">

### ✨ Effects
让细节更细腻的**纹理（Texture）**、让中间调更有力的**清晰度**、**去朦胧**（暗通道先验加引导滤波细化；
推到负值可以增加空气感）、裁剪后带高光优先、圆度和羽化的**暗角**，以及分辨率无关、可调大小与粗糙度的
胶片**颗粒**。

### 📈 Tone Curve
带可移动分割点的参数化区域曲线，**外加** RGB、红、绿、蓝的点曲线。曲线在构造上就是单调的，
所以不会意外出现影调反转。

</td>
</tr>
</table>

<p align="center">
  <img src="docs/images/curve-tetons.jpg" alt="Light 面板下方打开的色调曲线，RGB 点曲线上是一条平缓的 S 形曲线，作用于《特顿山脉与蛇河》" width="100%">
  <br>
  <sub>RGB 点曲线上一条平缓的 S 形曲线，就在 Light 滑块正下方。<i>安塞尔·亚当斯，1942（公有领域）。</i></sub>
</p>

<br>

## Color grading, the cinematic way

用随处拖动的色轮，分别给阴影、中间调和高光做分离色调。下图中，多萝西娅·兰格的
*《移民母亲》* 只拖动两次就获得了温暖的、类似照片冲印的色调（高光 42°，阴影 28°）。

<p align="center">
  <img src="docs/images/grading-migrant-mother.jpg" alt="中间调、阴影和高光的颜色分级色轮，旁边是多萝西娅·兰格的《移民母亲》，被调成温暖的冲印色调" width="100%">
  <br>
  <sub>Color 面板中的阴影、中间调和高光色轮。<i>多萝西娅·兰格，《移民母亲》（1936）。公有领域，美国国会图书馆。</i></sub>
</p>

<br>

## Before & after

按住 <kbd>\\</kbd> 偷看原图，按 <kbd>Y</kbd> 并排对比，或按 <kbd>Shift</kbd>+<kbd>Y</kbd> 分屏查看。
下面每一张都是 LightCraft 的真实截图，由智能体通过[控制通道](#built-for-agents)自动截取。

<table>
<tr>
<td width="50%"><img src="docs/images/ba-tetons.jpg" alt="《特顿山脉与蛇河》的并排修改前后对比：修改后的云层更深，河岸边的阴影更开阔"><br><sub><b>《特顿山脉与蛇河》。</b> 高光 −45、阴影 +38、清晰度 +28、去朦胧 +18。<i>安塞尔·亚当斯，1942（公有领域）。</i></sub></td>
<td width="50%"><img src="docs/images/ba-migrant-mother.jpg" alt="《移民母亲》的并排修改前后对比：修改后更温暖，阴影被提亮"><br><sub><b>《移民母亲》。</b> 阴影 +42、纹理 +18、分离色调、暗角。<i>多萝西娅·兰格，1936（公有领域）。</i></sub></td>
</tr>
<tr>
<td width="50%"><img src="docs/images/ba-earthrise.jpg" alt="《地出》的并排修改前后对比：月球地平线上方是地球，修改后略暖、更浓郁"><br><sub><b>《地出》。</b> 去朦胧 +22、高光 −30、更暖的白平衡、鲜艳度 +22。<i>NASA／比尔·安德斯，阿波罗 8 号，1968（公有领域）。</i></sub></td>
<td width="50%"><img src="docs/images/ba-blue-marble.jpg" alt="《蓝色弹珠》的并排修改前后对比：修改后黑色更深，云层细节更结实"><br><sub><b>《蓝色弹珠》。</b> 高光 −38、黑色色阶 −20、去朦胧 +15、鲜艳度 +30。<i>NASA，阿波罗 17 号，1972（公有领域）。</i></sub></td>
</tr>
</table>

<br>

## Masking that goes where you point

用**画笔**涂抹（大小、羽化、流量、密度、擦除），放置带可拖动控制点的**线性渐变**和**径向渐变**，
或者按**亮度范围**、**颜色范围**、**天空**、**主体**和**背景**来选择。用**添加／减去／相交**组合各个组件，
任意反相，每个蒙版还能调 15 项局部调整（色温、色调、曝光、对比度、高光、阴影、白色色阶、黑色色阶、纹理、
清晰度、去朦胧、色相、饱和度、锐化、降噪）以及一个总体的数量。

<p align="center">
  <img src="docs/images/masking.jpg" alt="蒙版面板中有一个线性天空蒙版和一个径向阳光光晕蒙版；径向渐变在湖景照片上以红色叠加层画在太阳周围" width="100%">
  <br>
  <sub>叠加在线性「天空」蒙版之上的径向「阳光光晕」蒙版（色温 +40、曝光 +0.50）。<i>照片来自 LightCraft 程序生成的演示图库。</i></sub>
</p>

<br>

## Presets, profiles & the Color Mixer

内置十八个手工制作的预设（*Golden Hour、Teal & Orange、Faded Matte、Selenium Tone、Crisp
Landscape* 等等），每个都有 0 到 200 % 的**数量**滑块。你可以从任意一组设置保存自己的预设、标记收藏，
并在整个选区之间只复制、粘贴或同步你选定的那些分组。

<p align="center">
  <img src="docs/images/presets-mixer.jpg" alt="预设列按 B&amp;W、Color、Film、Landscape、Portrait 和 Style 分组，旁边是 Color 面板，8 色段颜色混合器打开在紫色上" width="100%">
  <br>
  <sub>预设列与 8 色段颜色混合器并排。<i>照片来自演示图库。</i></sub>
</p>

<table>
<tr>
<td width="50%" valign="top">

### ✂️ Crop, straighten & geometry
自由或锁定的长宽比（1:1、4:5、5:7、2:3、4:3、16:9、16:10、原始比例），拖动旋转拉直并始终把最大的裁剪框
保持在画面内，三分线／网格／黄金比例叠加，翻转和 90° 旋转，外加手动垂直、水平、旋转、长宽比、
缩放和位移变换。

</td>
<td width="50%" valign="top">

### ⚫️ Black & White
一键（<kbd>V</kbd>）转为单色，配合 8 色段**黑白混合**压暗天空或让树叶发亮。
之后再用颜色分级调出硒调、棕褐或分离色调的印相效果。

</td>
</tr>
<tr>
<td width="50%"><img src="docs/images/crop-tetons.jpg" alt="《特顿山脉与蛇河》上的裁剪工具：一个略微旋转的 16:9 裁剪框，带三分线叠加，右侧是几何滑块"><br><sub>一个 16:9 的裁剪框拉直了 +2.5°，带三分线叠加。<i>安塞尔·亚当斯，1942（公有领域）。</i></sub></td>
<td width="50%"><img src="docs/images/bw-split.jpg" alt="沙丘的分屏修改前后视图：左边是彩色，右边是带颗粒和暗角的黑白"><br><sub>分屏视图：左边是彩色原图，右边是带清晰度、暗角和颗粒的黑白。<i>演示图库。</i></sub></td>
</tr>
</table>

<br>

## Organize everything

一个不碍事的图库：**所有照片**、**最近添加**、**留用**、**按日期**、嵌套在**文件夹**里的**相册**，
以及**最近删除**。用 <kbd>0</kbd>–<kbd>5</kbd> 评星，用 <kbd>P</kbd> / <kbd>X</kbd> /
<kbd>U</kbd> 加旗标，用 <kbd>6</kbd>–<kbd>9</kbd> 加颜色标签。搜索支持字段：
`rating:>3 flag:pick iso:>800 camera:x2 date:2026-04 keyword:mountains`。每个视图都能按拍摄日期、
导入日期、编辑日期、名称、星级、大小或随机排序（一种稳定的乱序；「视图 → 排序 → 重新乱序」可换一个顺序）。
两端对齐的**照片网格**和**正方形网格**视图都做了虚拟化，
所以无论是四十张还是四万张照片都同样顺滑。

<table>
<tr>
<td width="50%"><img src="docs/images/grid-demo.jpg" alt="24 张演示照片的两端对齐照片网格，带星级和旗标，侧边栏是 Travel 2026 文件夹下嵌套的相册"><br><sub>照片网格，相册嵌套在文件夹里，带星级和旗标。<i>演示图库。</i></sub></td>
<td width="50%"><img src="docs/images/grid-pd.jpg" alt="正方形网格，显示四张公有领域展示照片及其星级和留用旗标"><br><sub>四张公有领域展示照片的正方形网格。</sub></td>
</tr>
<tr>
<td colspan="2"><img src="docs/images/info-earthrise.jpg" alt="《地出》的信息面板，显示文件名、2400 × 2400 的 JPEG 尺寸、星级、标题字段和相机元数据"><br><sub>信息面板：文件、尺寸、星级、标题、题注、版权和相机元数据。<i>NASA／比尔·安德斯，《地出》，阿波罗 8 号，1968（公有领域）。</i></sub></td>
</tr>
</table>

<br>

## Built for agents

LightCraft 里的每一个菜单项、滑块、画笔笔触、裁剪手柄和按键都是一个**命令**，拥有稳定的 ID 和
JSON 参数。界面、键盘、CLI、JSON-lines 控制通道和一个 **MCP 服务器**全都通过同一个入口分发。
智能体可以挑片、显影、给天空加蒙版并导出，而且能*看到*结果。

```sh
lightcraft --control 7980 ~/Pictures/trip
```
```jsonc
{"method": "engine.execute", "params": {"command": "photo.flag",  "params": {"flag": "pick"}}}
{"method": "engine.execute", "params": {"command": "develop.set", "params": {"values": {"light.highlights": -45, "light.shadows": 38}}}}
{"method": "engine.execute", "params": {"command": "mask.add",    "params": {"kind": "radial", "center": [0.62, 0.4], "rx": 0.2, "ry": 0.14}}}
{"method": "ui.clickWidget",   "params": {"id": "slider:effects.clarity"}}       // 按名称操作任意控件
{"method": "ui.pointer",       "params": {"events": [{"kind":"down","x":0.2,"y":0.3}, {"kind":"up","x":0.4,"y":0.3}]}}
{"method": "ui.screenshot",    "params": {"path": "after.png"}}
```

- **命令注册表。** `engine.commands` 列出可用命令；`develop.controls` 列出每个滑块的范围、
  默认值和当前值。
- **MCP 服务器。** `lightcraft-cli mcp` 把命令注册表开放给 Claude（或任何 MCP 客户端），
  同时提供导入、查询、显影、蒙版、渲染（以图像返回）和导出的辅助工具。它既能无界面运行，也能附着
  到正在运行的应用上，支持截图、点击和手势。见 [docs/mcp.md](docs/mcp.md)。

  ```sh
  cargo build --release -p lightcraft-cli
  claude mcp add lightcraft -- "$PWD/target/release/lightcraft-cli" mcp ~/Pictures/shoot          # 无界面
  claude mcp add lightcraft-app -- "$PWD/target/release/lightcraft-cli" mcp --connect 127.0.0.1:7980  # 驱动运行中的应用
  ```
- **可脚本化的 CLI：** `lightcraft-cli run --import in.dng develop.set control=light.exposure value=0.7 app.export
  path=out.jpg longEdge=2048` 可以运行任意命令链（无界面、在已保存的图库上，或对正在运行的应用），
  每条命令打印一行 JSON 结果；`lightcraft-cli render in.dng -o out.jpg --set light.exposure=0.7 --preset …`。
- **一切皆可撤销**，包括智能体的操作：一次滑块拖动（或脚本里一连串更新）算一步撤销。
- **联系表（原生应用／CLI）：** 选中照片，然后 **文件 → 联系表 PDF…**。可选择 A4 或 Letter、横向、
  行数和列数以及文件名题注；保存分页 PDF，再用你的 PDF 阅读器打印。导出使用当前编辑，每张照片完整放入
  而不再次裁剪，以 150 dpi 和 sRGB 渲染。后台导出支持取消，并保护原文件和 XMP 附属文件。从 CLI 调用：

  ```sh
  lightcraft-cli run --import ~/Pictures/shoot library.selectAll export.contactSheet path=Contact.pdf paper=a4 columns=3 rows=4 captions=true
  ```

  `export.contactSheet` 还接受显式的 `ids`、`landscape=true` 和 `paper=letter`。只有当整个 PDF 生成成功后，
  它才会替换已有的输出文件。限制：1000 张照片、100 页、256 MiB；题注使用与水印相同的字体覆盖来栅格化
  （中日韩文字需要 craft-fonts），过长的名称会在中间省略。
  这是一套联系表工作流，
  不是打印驱动或自定义拼版编辑器。
- **每个控件都可寻址**（`ui.widgets`）且可按名称点击，所以智能体操作的是真实界面，而不是绕到侧门。
- 本 README 中的截图全部由 [`docs/showcase/`](docs/showcase/) 脚本端到端生成。
  协议参考：[docs/control-protocol.md](docs/control-protocol.md)。

<br>

## Fast, native, private

- **纯 Rust，无 C。** 我们自己的 RAW 解码器（DNG、佳能 CR2/CR3、索尼 ARW、尼康 NEF、富士 RAF 含 X-Trans、
  松下 RW2／徕卡 RWL、宾得 PEF、奥林巴斯 ORF）、我们自己的色彩科学、我们自己的管线。JPEG、PNG、TIFF、WebP、
  PSD 合成图和 JPEG XL 现在就能打开；加 `--features heif` 还能打开 HEIC/HEIF 照片（HEVC 是构建时的选择，
  与 PhotoCraft 中一致）。
- **场景参考与宽色域。** 内部使用线性 Rec.2020 浮点、Bradford 适应的白平衡、用色域映射代替裁剪，
  raw 走胶片式肩部，未改动过的 JPEG 则像素级直通。
- **分辨率无关的编辑。** 半径和画笔大小都相对于图像，所以 400 px 的预览、你的 5K 显示器和 60 MP 的导出
  看起来完全一致。
- **GPU 加速，与 CPU 一致。** 整条显影管线都作为 wgpu 计算内核运行（Metal／Vulkan／DX12），
  并与 CPU 管线比对到 1/255 以内。在 24 MP raw 上（Apple M4 Pro）：一次滑块更新约 4 ms 重新渲染，
  冷启动 2.5 MP 放大视图约 30 ms，全尺寸导出约 0.3 s（含并行 JPEG 编码）。
  没有 GPU 时同一条管线在所有 CPU 核心上运行，只重做受滑块影响的那些阶段。批量导出会并排渲染多张照片，
  所以一张的解码和编码能与另一张的渲染重叠。
- **即时挑片。** 打开一张 raw 会在约 0.1 s 内显示其内嵌的相机预览或缓存渲染，完整渲染随后跟上
  （24 MP 约 0.2–0.5 s）。上一张和下一张会在后台准备好，因此翻阅一次拍摄时每张约 50 ms。
- **后台渲染。** 一个工作线程池在 UI 线程之外渲染放大视图、修改前后对比和每一个可见缩略图：
  拖动时用草稿，松手后出全质量。
- **本地优先。** 没有账号、没有云、没有遥测、没有订阅。你的目录是一份只追加的可读操作日志，
  可以 diff、备份或重放。

<br>

## Feature status

LightCraft 很年轻，而且进展很快。**我们如实所处的位置**（细节见[路线图](ROADMAP.md#where-we-stand)）：

- **按功能数量算，我们达到了 Lightroom 的约 79%**（核心功能 98%），逐行记录在
  [docs/parity.md](docs/parity.md)。
- **作为日常替代 Lightroom 的工具，我们更接近 60–70%。** 在单台机器上处理 JPEG/DNG 以及大多数尼康／索尼／
  较老佳能的 raw 都很出色。
- **最大的缺口：**
  - **相机色彩校准：** 索尼、尼康、松下、富士和佳能 CR3 的 raw 会从各自的相机 JPEG 得到受保护的估计值，内置 ILCE-7CR、ILCE-7M4、X-H2S 和 X-T4 配置文件；实测校准仍然缺失，其他 raw 或被拒绝的拟合会保留中性矩阵；
  - **压缩的奥林巴斯 raw 和不受支持的 CR3 变体：** 这些在有内嵌 JPEG 预览时使用预览。富士无损／有损压缩 RAF 现在能解码传感器数据；[验证与已有图库重新加载说明](docs/raf-compression.md)；
  - **AI 蒙版与降噪：** 主体和天空选择目前是经典启发式算法；
  - **屏幕上的 HDR 显示、视频，以及经典版式中的打印／画册／地图模块。**
- **接下来做什么：** 见[我们要去往哪里](ROADMAP.md#where-were-going)。

| 领域 | 状态 |
|---|---|
| 图库：相册、文件夹、智能相册、堆叠（含自动堆叠）、虚拟副本、星级、旗标、标签、过滤器栏、搜索、排序、网格、胶片窗格 | ✅ |
| 挑片：比较（同步缩放）和筛选视图、自动前进、即时预览 | ✅ |
| Light、Color、Effects（暗角样式）、色调曲线（+ 细化饱和度、目标调整）、颜色混合器（+ 目标调整）、点颜色、颜色分级、校准、黑白 | ✅ |
| 蒙版：画笔、线性、径向、亮度／颜色范围、添加／减去／相交 | ✅（AI 主体／天空目前使用经典启发式算法） |
| 裁剪、拉直工具 + 自动拉直、翻转、旋转、长宽比、叠加参考线 | ✅ |
| 配置文件（Color、Neutral、Vivid、Landscape、Portrait、Monochrome：我们自己的风格）、预设、版本、历史记录、复制／粘贴／同步设置 | ✅ |
| 相机色彩：DNG 文件使用其自带矩阵 | ✅ DNG · 🟡 我们自己的索尼／富士配置文件；缺少实测校准数据库 |
| 原生 macOS 菜单栏（由命令注册表生成）、控制通道 + 每个控件可寻址、无界面 UI 快照 | ✅ |
| RAW：DNG、CR2、CR3（无损 CRX Bayer 以及版本 0x100/0x200 的 C-RAW）、ARW、NEF（未压缩 + 无损／有损压缩）、富士 RAF（未压缩 + 无损／有损压缩，Bayer + X-Trans）、松下 RW2／徕卡 RWL／Panasonic RAW（所有 raw 格式，从 DMC-LX1 到 DC-S1RM2）、宾得 PEF、奥林巴斯 ORF（未压缩）、三星 SRW（未压缩）；所有格式的嵌入预览，含 CR3 | 🟡 · CR3、压缩 ORF 和压缩 SRW 解码 ⬜ |
| 细节：锐化、亮度 + 色彩降噪 | ✅ · AI 降噪、超分辨率 ⬜ |
| 污点去除／修复／仿制（自动来源）、可视化斑点、红眼和宠物眼（自动瞳孔检测、眼神光） | ✅ · 内容识别填充、污点图钉编辑 🚧 |
| 导出：JPEG／PNG／TIFF／WebP／AVIF／DNG／原始格式，尺寸调整、文件大小上限、输出锐化、命名模板、批处理、元数据策略、文字或图像水印 | ✅ |
| HDR：带余量上限的 HDR 编辑、SDR 呈现、可视化 HDR；导出为 ISO 21496-1 增益图 JPEG、PQ AVIF 或 32 位浮点 TIFF | ✅ · HDR 显示 ⬜ |
| 图库持久化（防崩溃的操作日志 + 快照、后台压缩、保存失败会上报）、磁盘缩略图缓存 | ✅ |
| 导入：在原位置添加／复制／移动、重命名与文件夹模板、设备、重复检测、监视文件夹；本地文件夹浏览 | ✅ |
| MCP 服务器（无界面或运行中的应用、持久化图库）、CLI、控制通道 | ✅ |
| XMP 附属文件（读取／写入、自动写入）、读取 `crs:` 显影设置、预设文件（`.lcpreset`、XMP 预设） | ✅ |
| 镜头校正（畸变、暗角、自动 + 手动色差、去边、DNG 文件内嵌的镜头校正以及松下／徕卡 RW2／RWL 畸变数据）、几何（变换、限制裁剪）、垂直校正（自动／水平／垂直／完整／引导式） | ✅ · 相机镜头配置文件（我们自己的）⬜ |
| 照片合并：HDR（自动对齐、去重影）、全景（球面／圆柱／透视、边界变形、自动裁剪）、HDR 全景 → DNG | ✅ |
| GPU 管线（wgpu 计算，与 CPU 一致到 1/255 以内）、设备限制／出错时回退到 CPU | ✅ · 浏览器中的 WebGPU 🚧 |
| AI：分割蒙版、AI 降噪、超分辨率、人脸；视频 | ⬜（见[路线图](ROADMAP.md#where-were-going)） |
| 网页版（通过 WASM 在浏览器中使用同一界面）：图库持久化在 OPFS/IndexedDB、Web Worker 渲染、导出下载 | ✅ · WebGPU、Safari/Firefox 测试 🚧 |

<sub>✅ 现已可用 · 🚧 进行中 · ⬜ 尚未开始</sub>

<br>

## Quick start

```sh
git clone https://github.com/storytold/lightcraft && cd lightcraft
cargo run --release -p lightcraft                       # 打开你的图库（~/Pictures/LightCraft Library；新图库会先带演示照片）
cargo run --release -p lightcraft -- ~/Pictures/trip    # 导入你的照片（扫描文件夹，跳过重复项）
cargo run --release -p lightcraft -- --memory           # 用完即弃的内存演示会话（不写入任何内容）
cargo run --release -p lightcraft -- --control 7980     # 带自动化通道
cargo xtask web --serve                                 # 在浏览器中运行同一个应用：http://127.0.0.1:8080/
cargo run --release -p lightcraft-cli -- render photo.jpg -o out.jpg --set light.exposure=0.5
cargo xtask ci                                          # fmt、clippy、测试、分层、wasm 检查
```

CI 默认使用行表调试信息，并按启动时可用内存每 1.5 GB 开一个构建任务
（每个 CPU 最多一个；读不到内存时为 4 个），所以一台 8 GB、空闲 6 GB 的机器会用 4 个任务构建，
而一台空闲内存很少的繁忙机器会用更少。测试线程数随之调整，上限为 4。否则，每个 CPU 一个的
完整调试信息链接器会在任何测试运行之前就耗尽内存。GPU 覆盖范围不变。显式设置
`CARGO_PROFILE_DEV_DEBUG`、`CARGO_BUILD_JOBS` 和 `RUST_TEST_THREADS` 会覆盖这些默认值。
日常开发命令保持它们各自的设置。

**中文和日文文字**需要共享字体仓库，这是一个可选的构建输入（所有正式发布版都会包含它）：

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
CRAFT_FONTS_DIR=../craft-fonts cargo run --release -p lightcraft
```

不指定它时，LightCraft 构建和运行的方式完全相同，只是中文和日文文字没有字形。字体从不提交到
本仓库；见 [craftrules `standards/fonts.md`](https://github.com/storytold/craftrules/blob/main/standards/fonts.md)。

**界面语言：** **编辑 → 语言**（English、简体中文、繁體中文（台灣）、日本語、Português (Brasil)、Español、Deutsch、Русский、Français）或 **设置 → 一般 →
语言**；选择立即生效并会保留。见 [docs/localization.md](docs/localization.md)。

**日志：** 桌面应用把日志写到标准错误，以及设置文件夹里的 `logs/lightcraft.log`
（Linux `$XDG_CONFIG_HOME/lightcraft/logs/`，默认 `~/.config/lightcraft/logs/`；macOS
`~/Library/Application Support/LightCraft/logs/`；Windows `%APPDATA%\LightCraft\logs\`），从不写进图库。
从桌面菜单或 Dock 启动没有终端，所以请把这个文件附在错误报告里（**帮助 ▸ 打开日志
文件夹** 会在文件管理器中显示它）。每次启动会把
上一个日志移到 `lightcraft.1.log`，再上一个移到 `lightcraft.2.log`，这样崩溃那次的日志能在下次启动后
保留下来；文件到 16 MiB 停止增长，`--version` 和 `--help` 不写日志，带
`LIGHTCRAFT_NO_PREFS` 运行时只写到标准错误。默认情况下 LightCraft 自己的 crate 记到 `info`，其他都记到
`warn`。`LIGHTCRAFT_LOG=info` 或 `debug` 的行为和以前一样（LightCraft 自己的 crate 用该级别，其余只记警告和
错误；其他值：只记警告和错误），并且优先于 `RUST_LOG`，后者会把默认值替换为
env_logger 风格的指令，例如 `RUST_LOG=debug` 或 `RUST_LOG=warn,lightcraft_pipeline=trace`（以
`*` 结尾的指令覆盖所有以它开头的目标，如 `lightcraft*=debug`）。panic 也会记录在那里，
并且仍然写在临时文件夹里的 `lightcraft-panics.log`。日志器是 `apps/lightcraft/src/logging.rs`。

网页版需要 `wasm32-unknown-unknown` target 和匹配的 `wasm-bindgen` CLI
（`cargo xtask web` 会打印确切的安装命令）；见 [docs/web.md](docs/web.md)。

**Nix** 会构建桌面应用和 `lightcraft-cli`（Nix 构建总是包含 craft-fonts 输入，所以日文
文字有字形）：

```sh
nix run github:storytold/lightcraft                    # 桌面应用
nix build github:storytold/lightcraft                  # → ./result/bin/{lightcraft,lightcraft-cli}
nix develop github:storytold/lightcraft                # rust 工具链 + 原生依赖 + 字体
```

在 flake 配置中（NixOS、home-manager、nix-darwin）：

```nix
# flake.nix
inputs.lightcraft.url = "github:storytold/lightcraft";
# 可选：使用你自己的 nixpkgs，而不是 LightCraft 锁定的那个
# inputs.lightcraft.inputs.nixpkgs.follows = "nixpkgs";

# 然后在 NixOS 或 home-manager 模块中（`inputs` 在作用域内）：
nixpkgs.overlays = [ inputs.lightcraft.overlays.default ];   # 让 `pkgs.lightcraft` 可用
environment.systemPackages = [ pkgs.lightcraft ];           # home-manager：home.packages = [ pkgs.lightcraft ];
```

`nix build` 会安装与 .deb/.rpm 相同的 desktop 文件、hicolor 图标和 AppStream 元数据，并运行
`cargo test --workspace` 作为它的检查阶段（用 `pkgs.lightcraft.overrideAttrs { doCheck = false; }` 跳过）。

**键盘：** <kbd>G</kbd> 网格 · <kbd>D</kbd> 细节 · <kbd>E</kbd> 编辑 · <kbd>C</kbd> 裁剪 · <kbd>M</kbd> 蒙版 ·
<kbd>Shift</kbd>+<kbd>P</kbd> 预设 · <kbd>\\</kbd> 原图 · <kbd>Y</kbd> 修改前后 · <kbd>Z</kbd> 缩放 ·
<kbd>J</kbd> 溢出警告 · <kbd>⌘Z</kbd> 撤销 · <kbd>⌘/</kbd> 全部快捷键。

## How it's built

一个引擎优先的 Cargo 工作区，由小而经过测试的 crate 组成，并强制分层（`cargo xtask layers`）：`geom`、
`color`、`raster`、`tiff` → `raw`、`codecs`、`meta`、`develop` → `pipeline` → `catalog` → `engine` → `ui-egui`。
egui 前端是一个可替换的 crate；它下面没有任何东西知道 UI 的存在。`mcp` 和应用（`lightcraft`、
`lightcraft-cli`）坐落在 `engine` 之上。

## Contributing

人和智能体遵循同样的规则，所以请先读 [AGENTS.md](AGENTS.md)。简短版本：

- **净室。** 绝不阅读 Adobe 的二进制文件或 GPL 的 raw／照片代码（darktable、RawTherapee、LibRaw、rawspeed、dcraw……）；
  只依据公开规范和黑盒观察来工作。
- **绝不使用 Adobe 资源：** 不使用 Adobe 产品的图标、截图、预设、配置文件、LUT 或字体。仓库中的每一个
  图像、图标和字体都是原创、公有领域、知识共享、OFL 或宽松许可的，并在同一提交中
  于 [assets/ATTRIBUTION.md](assets/ATTRIBUTION.md) 添加条目。新字体提交到
  [storytold/craft-fonts](https://github.com/storytold/craft-fonts)，而不是这里。
- **纯 Rust**、强制 crate 分层、一切皆命令，并且每次提交前 `cargo xtask ci` 必须通过
  （一个提交一个任务 ID）。
- **绝不崩溃。** 非测试代码返回错误而不是 panic：不用 `unwrap()`、`expect()`、`panic!` 或
  `unsafe`，对任何来自输入的数据都做受检索引，并且每个崩溃修复都带一个回归测试。细节见
  [AGENTS.md](AGENTS.md#never-crash-outranks-feature-work)。

有问题、想法，或者想先讨论一个 bug？来 [Discord](https://discord.gg/artcraft)。

<br>

## Downloads

**第一次用 LightCraft？** 从 [getartcraft.com 的 LightCraft 页面](https://getartcraft.com/apps/lightcraft)下载。那是最简单的安装方式。

**想要特定的构建或格式？** 在 GitHub 上，[最新发布](https://github.com/storytold/lightcraft/releases/latest)包含下面列出的所有构建，[全部发布](https://github.com/storytold/lightcraft/releases)则有更早的版本及其说明。文件名中的 `<ver>` 是版本号，`SHA256SUMS.txt` 列出了每个文件的校验和。

### Windows

| 构建 | 安装程序 | 便携版 |
|---|---|---|
| x64（64 位 Intel/AMD） | `lightcraft-<ver>-windows-x64.msi` | `lightcraft-<ver>-windows-x64-portable.zip` |
| arm64（骁龙及其他 ARM PC） | `lightcraft-<ver>-windows-arm64.msi` | `lightcraft-<ver>-windows-arm64-portable.zip` |
| x86（32 位） | `lightcraft-<ver>-windows-x86.msi` | `lightcraft-<ver>-windows-x86-portable.zip` |

安装程序和可执行文件都有代码签名。安装程序会询问安装位置（默认
`C:\Program Files\LightCraft`；升级会留在你选择的文件夹），结束时有一页确认 LightCraft 已安装，
并可选择启动它。无人值守安装：`msiexec /i lightcraft-<ver>-windows-x64.msi /qn INSTALLFOLDER="D:\Apps\LightCraft\"`。

### macOS

| 构建 | 文件 | 说明 |
|---|---|---|
| 应用，通用（Apple 芯片 + Intel） | `lightcraft-<ver>-macos-universal.dmg` | 已签名并公证 |
| 命令行工具，通用 | `lightcraft-cli-<ver>-macos-universal.zip` | 已签名并公证 |

### Linux

| 格式 | x86_64 | aarch64 (ARM64) | 说明 |
|---|---|---|---|
| AppImage | `lightcraft-<ver>-linux-x86_64.AppImage` | `lightcraft-<ver>-linux-aarch64.AppImage` | 随处可运行；用 [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate)（`.zsync` 文件）自我更新 |
| Flatpak | `lightcraft-<ver>-linux-x86_64.flatpak` | `lightcraft-<ver>-linux-aarch64.flatpak` | 沙箱化；`flatpak install --user <file>` |
| Debian/Ubuntu | `lightcraft-<ver>-linux-x86_64.deb` | `lightcraft-<ver>-linux-aarch64.deb` | |
| Fedora/RHEL/openSUSE | `lightcraft-<ver>-linux-x86_64.rpm` | `lightcraft-<ver>-linux-aarch64.rpm` | |
| 压缩包 | `lightcraft-<ver>-linux-x86_64.tar.gz` | `lightcraft-<ver>-linux-aarch64.tar.gz` | 解压到任意位置 |

### FreeBSD

| 构建 | 文件 |
|---|---|
| x86_64 | `lightcraft-<ver>-freebsd-x86_64.tar.gz` |

### 网页版（WebAssembly）

| 构建 | 文件 | 说明 |
|---|---|---|
| 静态站点 | `lightcraft-web-<ver>.zip` | 在现代浏览器中运行；托管到任意静态服务器 |

<br>

## The Crafting Apps

LightCraft 是 **Crafting Apps** 之一：来自 [ArtCraft](https://getartcraft.com/) 团队的免费开源创作工具，
每一个都用 Rust 从零编写，每一个都能独立使用。

| | 应用 | 用途 | 代码 | 了解更多 |
|:-:|---|---|---|---|
| <img src="https://raw.githubusercontent.com/storytold/photocraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.photocraft.png" alt="" width="32" height="32"> | **PhotoCraft** | 图像编辑：图层、蒙版、文字和真正的 PSD 文件 | [GitHub](https://github.com/storytold/photocraft) | [网站](https://getartcraft.com/apps/photocraft) |
| <img src="https://raw.githubusercontent.com/storytold/vectorcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.vectorcraft.png" alt="" width="32" height="32"> | **VectorCraft** | 矢量插画 | [GitHub](https://github.com/storytold/vectorcraft) | [网站](https://getartcraft.com/apps/vectorcraft) |
| <img src="https://raw.githubusercontent.com/storytold/filmcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.filmcraft.png" alt="" width="32" height="32"> | **FilmCraft** | 视频剪辑、调色与声音 | [GitHub](https://github.com/storytold/filmcraft) | [网站](https://getartcraft.com/apps/filmcraft) |
| <img src="https://raw.githubusercontent.com/storytold/lightcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.lightcraft.png" alt="" width="32" height="32"> | **LightCraft** | **照片图库与 raw 显影 · 你在这里** | [GitHub](https://github.com/storytold/lightcraft) | [网站](https://getartcraft.com/apps/lightcraft) |
| <img src="https://raw.githubusercontent.com/storytold/pdfcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.pdfcraft.png" alt="" width="32" height="32"> | **PdfCraft** | 阅读、整理和保护 PDF | [GitHub](https://github.com/storytold/pdfcraft) | [网站](https://getartcraft.com/apps/pdfcraft) |
| <img src="https://raw.githubusercontent.com/storytold/effectcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.effectcraft.png" alt="" width="32" height="32"> | **EffectCraft** | 动态图形与视觉特效 | [GitHub](https://github.com/storytold/effectcraft) | [网站](https://getartcraft.com/apps/effectcraft) |
| <img src="https://raw.githubusercontent.com/storytold/designcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.designcraft.png" alt="" width="32" height="32"> | **DesignCraft** | 页面排版与出版 | [GitHub](https://github.com/storytold/designcraft) | [网站](https://getartcraft.com/apps/designcraft) |

以及 [**ArtCraft**](https://getartcraft.com/) 本身，我们为想要真正掌控力的创作者打造的 AI 图像与视频工作室。

<br>

<p align="center">
  <a href="https://discord.gg/artcraft"><img alt="Join the ArtCraft community on Discord" src="https://img.shields.io/badge/Join%20us%20on%20Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white" height="40"></a>
</p>

<h3 align="center">Come make things with us</h3>

<p align="center">
  我们的 Discord 是各种创作者聚集的地方：画画的人、拍照的人、做动画的人、剪片子的人、
  排版的人，还有仍在摸索自己喜欢做什么的人。分享你在做的东西，
  寻求帮助，告诉我们哪里坏了，或者告诉我们你希望这些工具能做什么。
  无论你用什么媒介，无论你做了多久，这里都欢迎你。
</p>

<p align="center">
  <a href="https://discord.gg/artcraft"><b>discord.gg/artcraft</b></a> ·
  <a href="https://getartcraft.com/">getartcraft.com</a> ·
  <a href="https://getartcraft.com/apps">The Crafting Apps</a> ·
  <a href="https://getartcraft.com/apps/lightcraft">LightCraft</a>
</p>

<br>

## License and credits

LightCraft 采用 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 双许可，任你选择。
Copyright (c) 2026 ArtCraft Team and the LightCraft contributors. 必需的声明见 [NOTICE](NOTICE)。

随附的字体、图标、图像和其他资源保留各自的开放许可；每一项都在
[assets/ATTRIBUTION.md](assets/ATTRIBUTION.md) 中列出了作者、来源和许可。

展示照片是经 Wikimedia Commons 使用的公有领域作品：安塞尔·亚当斯，*《特顿山脉与蛇河》*
（1942，美国国家档案馆）；多萝西娅·兰格，*《移民母亲》*（1936，美国国会图书馆）；比尔·安德斯／NASA，
*《地出》*（1968）；NASA，*《蓝色弹珠》*（1972）。演示图库由 LightCraft 程序生成。界面字体：
Inter（SIL OFL 1.1）。用 [craft-fonts](https://github.com/storytold/craft-fonts) 构建的版本（所有正式发布版）
还会内嵌它的中文和日文字体（Noto Sans CJK SC、BIZ UDPGothic、BIZ UDMincho、Shippori Mincho；SIL OFL 1.1），列在其
[ATTRIBUTION.md](https://github.com/storytold/craft-fonts/blob/main/ATTRIBUTION.md) 中。所有图标均为原创。

[`docs/brand/`](docs/brand/) 中的 ArtCraft 名称、字标和标志是
ArtCraft 团队的商标，不在本许可范围内。它们只能在未修改的情况下，作为
本仓库和 LightCraft 的一部分，按 [`docs/brand/LICENSE-brand.txt`](docs/brand/LICENSE-brand.txt) 使用。
分支和修改版本必须移除它们。

<sub>Adobe、Photoshop、Illustrator、Premiere Pro、Lightroom、Acrobat、After Effects 和 InDesign 是 Adobe Inc. 在美国和／或其他国家的商标或注册商标。LightCraft 是一个独立的开源项目，与 Adobe Inc. 无隶属关系，也未获其赞助或认可；这些名称仅用于说明它所兼容的工作流。</sub>

<p align="center">
  <a href="https://getartcraft.com/"><img alt="ArtCraft" src="docs/brand/artcraft-mark.svg" width="28"></a><br>
  <sub>由 <a href="https://getartcraft.com/">ArtCraft</a> 团队和社区制作。</sub>
</p>

## Star history

[![Star History Chart](https://api.star-history.com/svg?repos=storytold/lightcraft&type=Date&legend=top-left)](https://www.star-history.com/?repos=storytold%2Flightcraft&type=date&legend=top-left)
