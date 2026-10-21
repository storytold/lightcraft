<p align="center">
  <a href="https://getartcraft.com/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="docs/brand/artcraft-logo-white.svg">
      <img alt="ArtCraft" src="docs/brand/artcraft-logo.svg" width="200">
    </picture>
  </a>
</p>


<h1 align="center">LightCraft</h1>

<h3 align="center">صورك أنت. بكسلاتك أنت. جهازك أنت.</h3>

<p align="center">
  <a href="README.md">English</a> ·
  <a href="README.zh-hans.md">简体中文</a> ·
  <a href="README.ar.md">العربية</a>
</p>

<p align="center">
  <b>مكتبة صور وتظهير ملفات raw؛ إعادة تنفيذ مفتوحة المصدر لبرنامج Adobe Lightroom وفق منهجية الغرفة النظيفة، مبنية بلغة Rust خالصة.</b><br>
  يعمل أصلياً على macOS وWindows وLinux، وفي المتصفح عبر WebAssembly، ويمكن قيادة أدائه كاملاً عبر وكلاء الذكاء الاصطناعي من خلال MCP.
</p>

<p align="center">
  <img alt="Rust خالصة" src="https://img.shields.io/badge/pure-Rust-f2a516?style=flat-square&logo=rust&logoColor=white">
  <img alt="macOS · Windows · Linux · الويب" src="https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux%20%C2%B7%20Web-8a5800?style=flat-square">
  <img alt="خادم MCP مدمج" src="https://img.shields.io/badge/MCP-ready-8a5800?style=flat-square">
  <img alt="الترخيص: MIT أو Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-8a5800?style=flat-square">
  <a href="ROADMAP.md"><img alt="الحالة: فتيّ ويتقدّم بسرعة" src="https://img.shields.io/badge/status-young%20%26%20moving%20fast-f2a516?style=flat-square"></a>
</p>

<p align="center">
  <a href="https://discord.gg/artcraft"><img alt="انضم إلى مجتمع ArtCraft على Discord" src="https://img.shields.io/badge/Join%20us%20on%20Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white" height="40"></a>
</p>

<p align="center">
  <a href="https://getartcraft.com/apps/lightcraft"><b>LightCraft على getartcraft.com</b></a> ·
  <a href="https://getartcraft.com/">ArtCraft</a> ·
  <a href="https://getartcraft.com/apps">كل تطبيقات Crafting Apps</a>
</p>

<br>

<p align="center">
  <img src="docs/images/hero-tetons.jpg" alt="واجهة تحرير LightCraft مع صورة أنسل آدمز «تيتونز ونهر الأفعى» في العدسة، ولوحتا الضوء والتأثيرات مفتوحتان على اليمين، وصور العرض الأربع في شريط الفيلم" width="100%">
  <br>
  <sub><i>أنسل آدمز، «التيتونز ونهر الأفعى» (1942). ملكية عامة، الأرشيف الوطني الأمريكي. مُظهّرة في LightCraft.</i></sub>
</p>

> [!NOTE]
> **ArtCraft مجتمعٌ يجمع الفنانين من كل المشارب.** رقمي، توليدي، موسيقى،
> ألعاب — إن كنت تصنع شيئاً فأنت منّا. **[تعال وقل مرحباً على Discord](https://discord.gg/artcraft).**

<p align="center">
  <a href="#حرّر-بجدية">التحرير</a> ·
  <a href="#معايرة-الألوان-على-الطريقة-السينمائية">معايرة الألوان</a> ·
  <a href="#قبل-وبعد">قبل وبعد</a> ·
  <a href="#أقنعة-تتبع-إصبعك">الأقنعة</a> ·
  <a href="#الإعدادات-المسبقة-والملفات-التعريفية-ومازج-الألوان">الإعدادات المسبقة</a> ·
  <a href="#نظّم-كل-شيء">المكتبة</a> ·
  <a href="#مبني-لوكلاء-الذكاء-الاصطناعي">الوكلاء وMCP</a> ·
  <a href="#سريع-أصلي-خاص">الأداء</a> ·
  <a href="#حالة-المزايا">الحالة</a> ·
  <a href="#البداية-السريعة">البداية السريعة</a> ·
  <a href="ROADMAP.md">خريطة الطريق</a> ·
  <a href="#التنزيلات">التنزيلات</a> ·
  <a href="#تطبيقات-crafting-apps">تطبيقات Crafting Apps</a>
</p>

<br>

<div dir="rtl">

## حرّر بجدية

LightCraft غرفة تحميم كاملة في تطبيق أصلي واحد. كل تعديل **غير إتلافي**، فأصولك الأصلية لا تُمسّ أبداً. كل منزلق يُصيَّر عبر **مسار عرض مرجعي مشهدي واسع المدى بفاصلة عائمة 32-بت**: الإضاءات المرتفعة تنحدر كالفيلم، والظلال تنفتح بلا هالات ضوئية، ويبقى اللون نقياً من لحظة الالتقاط حتى التصدير.

<table>
<tr>
<td width="50%" valign="top">

### ☀️ الضوء
**التعريض، التباين، الإضاءات المرتفعة، الظلال، الأبيض، الأسود**، مع رسم نبرة موضعي واعٍ بالحواف (مرشح موجَّه على إضاءة لوغاريتمية). سحب الإضاءات المرتفعة إلى ‎−100‎ يستعيد سماءً محترقة دون الهالات الرمادية التي يخلفها منحنى ساذج.

### 🎨 اللون
**موازنة اللون الأبيض** بالحرارة والتدرّج (كلفن لملفات raw، وقيم نسبية لـ JPEG) مع إعدادات مسبقة وخيار تلقائي و**قطارة** تنقر لتحييد اللون. **الاهتمام (Vibrance)** الذي يحمي درجات البشرة، **التشبّع**، **مازج ألوان** بثماني نطاقات (درجة لون / تشبّع / إضاءة) وعجلات **معايرة ألوان** ثلاثية الاتجاه مع مزج وتوازن. يحسب كل ذلك في مساحة OkLCh، مساحة ألوان إدراكية حديثة.

</td>
<td width="50%" valign="top">

### ✨ التأثيرات
**النسج** للتفاصيل الدقيقة، **الوضوح** لقسمة النبرات الوسطى، **إزالة الغباش** (أولوية القناة المظلمة مع تنقية موجّهة؛ ادفعه سالباً لإضافة أجواء)، **تظليل الحواف** بعد القص بأولوية الإضاءات مع استدارة وترييق، و**حبيبات** الفيلم المستقلة عن الدقة مع تحكّم في الحجم والخشونة.

### 📈 منحنى النبرة
منحنى مناطق بارامتري بحواف قابلة للنقل **بالإضافة إلى** منحنيات نقطية لـ RGB والأحمر والأخضر والأزرق. المنحنيات أحادية الاتجاه بحكم بنيتها، فلا يحدث انقلاب نبرة بالمحاولة.

</td>
</tr>
</table>

<p align="center">
  <img src="docs/images/curve-tetons.jpg" alt="منحنى النبرة مفتوح تحت لوحة الضوء، مع منحنى نقطي RGB بشكل S خفيف مطبق على التيتونز ونهر الأفعى" width="100%">
  <br>
  <sub>انحناءة S خفيفة على منحنى RGB النقطي، تحت منزلقات الضوء مباشرة. <i>أنسل آدمز، 1942 (ملكية عامة).</i></sub>
</p>

<br>

## معايرة الألوان على الطريقة السينمائية

فاصِل نبرة الظلال والنبرات الوسطى والإضاءات المرتفعة كلٌّ على حدة بعجلات ألوان تسحب في أي اتجاه. أدناه، تحصل صورة دوروثيا لانج *الأم المهاجرة* على نبرة دافئة تشبه المطبوعات (الإضاءات عند 42°، والظلال عند 28°) بسحبتين اثنتين.

<p align="center">
  <img src="docs/images/grading-migrant-mother.jpg" alt="عجلات معايرة الألوان للنبرات الوسطى والظلال والإضاءات بجانب صورة الأم المهاجرة لدوروثيا لانج، بنبرة دافئة كالمطبوعات" width="100%">
  <br>
  <sub>عجلات الظلال والنبرات الوسطى والإضاءات في لوحة اللون. <i>دوروثيا لانج، «الأم المهاجرة» (1936). ملكية عامة، مكتبة الكونغرس.</i></sub>
</p>

<br>

## قبل وبعد

استمر بالضغط على <kbd>\\</kbd> لالتقاط نظرة على الأصل، أو اضغط <kbd>Y</kbd> لعرض متجاور، أو <kbd>Shift</kbd>+<kbd>Y</kbd> لعرض مقسوم. كل صورة أدناه لقطة شاشة حقيقية من LightCraft، التقطها وكيل ذكاء اصطناعي تلقائياً عبر [قناة التحكم](#مبني-للوكلاء).

<table>
<tr>
<td width="50%"><img src="docs/images/ba-tetons.jpg" alt="قبل وبعد متجاوران للتيتونز ونهر الأفعى: النتيجة بسحب أعمق وظلال أكثر انفتاحاً على ضفاف النهر"><br><sub><b>التيتونز ونهر الأفعى.</b> الإضاءات المرتفعة ‎−45‎، الظلال ‎+38‎، الوضوح ‎+28‎، إزالة الغباش ‎+18‎. <i>أنسل آدمز، 1942 (ملكية عامة).</i></sub></td>
<td width="50%"><img src="docs/images/ba-migrant-mother.jpg" alt="قبل وبعد متجاوران للأم المهاجرة: النتيجة أدفأ مع ظلال مرفوعة"><br><sub><b>الأم المهاجرة.</b> الظلال ‎+42‎، النسج ‎+18‎، فصل نبرة، تظليل حواف. <i>دوروثيا لانج، 1936 (ملكية عامة).</i></sub></td>
</tr>
<tr>
<td width="50%"><img src="docs/images/ba-earthrise.jpg" alt="قبل وبعد متجاوران لصورة شروق الأرض: الأرض فوق أفق القمر، والنتيجة أدفأ وأغنى قليلاً"><br><sub><b>شروق الأرض.</b> إزالة الغباش ‎+22‎، الإضاءات المرتفعة ‎−30‎، موازنة أبيض أدفأ، الاهتمام ‎+22‎. <i>ناسا / بيل أندرس، أبولو 8، 1968 (ملكية عامة).</i></sub></td>
<td width="50%"><img src="docs/images/ba-blue-marble.jpg" alt="قبل وبعد متجاوران للرخامة الزرقاء: النتيجة بأسود أعمق وتفاصيل سحب أمتن"><br><sub><b>الرخامة الزرقاء.</b> الإضاءات المرتفعة ‎−38‎، الأسود ‎−20‎، إزالة الغباش ‎+15‎، الاهتمام ‎+30‎. <i>ناسا، أبولو 17، 1972 (ملكية عامة).</i></sub></td>
</tr>
</table>

<br>

## أقنعة تتبع إصبعك

ارسم بـ **فرشاة** (حجم، ترييق، تدفق، كثافة، ممحاة)، أو أنزِل **تدرجات خطية** و**شعاعية** بدبابيس قابلة للسحب، أو حدّد بـ **نطاق الإضاءة**، **نطاق اللون**، **السماء**، **الموضوع** و**الخلفية**. ادمج المكونات بـ **إضافة / طرح / تقاطع**، اعكس أيّاً منها، واضبط 15 تعديلاً موضعياً لكل قناع (الحرارة، التدرّج، التعريض، التباين، الإضاءات المرتفعة، الظلال، الأبيض، الأسود، النسج، الوضوح، إزالة الغباش، درجة اللون، التشبّع، الحدة، الضوضاء) إضافة إلى مقدار إجمالي شامل.

<p align="center">
  <img src="docs/images/masking.jpg" alt="لوحة الأقنعة مع قناع سماء خطي وقناع توهج شمس شعاعي؛ التدرج الشعاعي مرسوم كطبقة حمراء حول الشمس في مشهد بحيرة" width="100%">
  <br>
  <sub>قناع «توهج الشمس» الشعاعي (الحرارة ‎+40‎، التعريض ‎+0.50‎) فوق قناع «السماء» الخطي. <i>صورة من مكتبة العرض التوليدية في LightCraft.</i></sub>
</p>

<br>

## الإعدادات المسبقة والملفات التعريفية ومازج الألوان

ثمانية عشر إعداداً مسبقاً مصنوعاً بعناية في الصندوق (*الساعة الذهبية، الأزرق الداكن والبرتقالي، المطفي الباهت، نبرة السيلينيوم، مشهد حاد* وغيرها)، لكلٍّ منها منزلق **مقدار** من 0 إلى 200٪. احفظ إعداداتك الخاصة من أي مجموعة إعدادات، وعلّم المفضلة، وانسخ التعديلات أو الصقها أوزامنها عبر تحديد كامل بالمجموعات التي تختارها بالضبط.

<p align="center">
  <img src="docs/images/presets-mixer.jpg" alt="عمود الإعدادات المسبقة مقسّماً إلى أبيض وأسود، ألوان، فيلم، مشاهد، بورتريه، ونمط، بجانب لوحة اللون مع مازج الألوان ثماني النطاقات مفتوحاً على البنفسجي" width="100%">
  <br>
  <sub>عمود الإعدادات المسبقة بجانب مازج الألوان ثماني النطاقات. <i>صورة من مكتبة العرض.</i></sub>
</p>

<table>
<tr>
<td width="50%" valign="top">

### ✂️ القص والتعديل والهندسة
نِسَب عرض حرة أو مقفلة (1:1، 4:5، 5:7، 2:3، 4:3، 16:9، 16:10، الأصلية)، تعديل بالسحب للتدوير يحافظ دائماً على أكبر قص داخل الصورة، شبكات الأثلاث والشبكة والنسبة الذهبية، انعكاسات وتدويرات 90°، إضافة إلى تحويلات رأسية وأفقية وتدوير ونسبة وحجم وإزاحة يدوية.

</td>
<td width="50%" valign="top">

### ⚫️ الأبيض والأسود
نقرة واحدة (<kbd>V</kbd>) إلى أحادي اللون، مع **مازج أبيض وأسود** ثماني النطاقات يتيح لك تعتيم السماء أو إشعاع أوراق الشجر. ثم امنحه نبرة بمعايرة الألوان للحصول على مطبوعات سيلينيوم أو سيبيا أو فاصلة النبرة.

</td>
</tr>
<tr>
<td width="50%"><img src="docs/images/crop-tetons.jpg" alt="أداة القص على التيتونز ونهر الأفعى: قص 16×9 مُدار قليلاً، مع شبكة أثلاث ومنزلقات الهندسة على اليمين"><br><sub>قص 16:9 مُعدَّل التدوير ‎+2.5°‎ مع شبكة الأثلاث. <i>أنسل آدمز، 1942 (ملكية عامة).</i></sub></td>
<td width="50%"><img src="docs/images/bw-split.jpg" alt="عرض مقسوم قبل وبعد لكثبان رملية: اللون على اليسار، والأبيض والأسود بحبيبات وتظليل حواف على اليمين"><br><sub>عرض مقسوم: الأصل ملوّن يساراً، وأبيض وأسود بالوضوح وتظليل الحواف والحبيبات يميناً. <i>مكتبة العرض.</i></sub></td>
</tr>
</table>

<br>

## نظّم كل شيء

مكتبة لا تعترض طريقك: **كل الصور**، **أُضيفت حديثاً**، **المختارات**، **حسب التاريخ**، **تجميعات** متداخلة في **مجلدات**، و**المحذوفات الأخيرة**. قيّم بـ <kbd>0</kbd>–<kbd>5</kbd>، علّم بـ <kbd>P</kbd> / <kbd>X</kbd> / <kbd>U</kbd>، وألصق تسميات لونية بـ <kbd>6</kbd>–<kbd>9</kbd>. البحث يفهم الحقول:
`rating:>3 flag:pick iso:>800 camera:x2 date:2026-04 keyword:mountains`. كل عرض يُرتَّب حسب تاريخ الالتقاط أو الاستيراد أو التعديل أو الاسم أو التقييم أو الحجم أو عشوائياً (خلطة ثابتة؛ عرض → ترتيب → إعادة خلط للحصول على خلطة جديدة). عرضا **شبكة الصور** المبررة و**الشبكة المربعة** افتراضيان، فيبقيان سلسين سواء كان لديك أربعون صورة أم أربعون ألفاً.

<table>
<tr>
<td width="50%"><img src="docs/images/grid-demo.jpg" alt="شبكة صور مبررة بـ 24 صورة عرض مع تقييمات وأعلام، وشريط جانبي بتجميعات متداخلة تحت مجلد سفر 2026"><br><sub>شبكة الصور مع تجميعات متداخلة في مجلد، مع تقييمات وأعلام. <i>مكتبة العرض.</i></sub></td>
<td width="50%"><img src="docs/images/grid-pd.jpg" alt="شبكة مربعة تعرض صور العرض الأربع ملكية العامة مع تقييمات نجمية وأعلام اختيار"><br><sub>شبكة مربعة لصور العرض الأربع ملكية العامة.</sub></td>
</tr>
<tr>
<td colspan="2"><img src="docs/images/info-earthrise.jpg" alt="لوحة المعلومات لصورة شروق الأرض تعرض اسم الملف وأبعاد JPEG ‏2400×2400 والتقييم وحقل العنوان وبيانات الكاميرا"><br><sub>لوحة المعلومات: الملف والأبعاد والتقييم والعنوان والتسمية التوضيحية وحقوق النشر وبيانات الكاميرا. <i>ناسا / بيل أندرس، «شروق الأرض»، أبولو 8، 1968 (ملكية عامة).</i></sub></td>
</tr>
</table>

<br>

## مبني لوكلاء الذكاء الاصطناعي

كل عنصر قائمة ومنزلق وضربة فرشاة ومقبض قص وضغطة مفتاح في LightCraft هو **أمر** بمعرّف ثابت ومعاملات JSON. الواجهة ولوحة المفاتيح وواجهة سطر الأوامر وقناة تحكم JSON-lines و**خادم MCP** كلها تُوزَّع عبر نقطة الدخول ذاتها. يستطيع الوكيل غربلة جلسة تصوير وتظهيرها وتقنيع سماء وتصديرها، ثم *رؤية* النتيجة.

```sh
lightcraft --control 7980 ~/Pictures/trip
```
```jsonc
{"method": "engine.execute", "params": {"command": "photo.flag",  "params": {"flag": "pick"}}}
{"method": "engine.execute", "params": {"command": "develop.set", "params": {"values": {"light.highlights": -45, "light.shadows": 38}}}}
{"method": "engine.execute", "params": {"command": "mask.add",    "params": {"kind": "radial", "center": [0.62, 0.4], "rx": 0.2, "ry": 0.14}}}
{"method": "ui.clickWidget",   "params": {"id": "slider:effects.clarity"}}       // قُد أي عنصر بالاسم
{"method": "ui.pointer",       "params": {"events": [{"kind":"down","x":0.2,"y":0.3}, {"kind":"up","x":0.4,"y":0.3}]}}
{"method": "ui.screenshot",    "params": {"path": "after.png"}}
```

- **سجل الأوامر.** يعرض `engine.commands` الأوامر المتاحة؛ ويعرض `develop.controls` نطاق كل منزلق وقيمته الافتراضية والحالية.
- **خادم MCP.** يعرض `lightcraft-cli mcp` سجل الأوامر لـ Claude (أو أي عميل MCP)، إلى جانب مساعدات للاستيراد والاستعلام والتظهير والتقنيع والتصيير (يُعاد كصورة) والتصدير. يعمل بلا واجهة أو متصلاً بالتطبيق الجاري بلقطات شاشة ونقرات وإيماءات. انظر [docs/mcp.md](docs/mcp.md).

  ```sh
  cargo build --release -p lightcraft-cli
  claude mcp add lightcraft -- "$PWD/target/release/lightcraft-cli" mcp ~/Pictures/shoot          # بلا واجهة
  claude mcp add lightcraft-app -- "$PWD/target/release/lightcraft-cli" mcp --connect 127.0.0.1:7980  # التطبيق الحي
  ```
- **واجهة سطر قابلة للبرمجة:** يُشغّل `lightcraft-cli run --import in.dng develop.set control=light.exposure value=0.7 app.export path=out.jpg longEdge=2048` أياً كان تسلسل الأوامر (بلا واجهة، أو على مكتبة محفوظة، أو ضد التطبيق الجاري) ويطبع نتيجة JSON واحدة لكل أمر؛ كذلك `lightcraft-cli render in.dng -o out.jpg --set light.exposure=0.7 --preset …`.
- **تراجع عن كل شيء**، بما في ذلك أفعال الوكلاء: سحب منزلق (أو دفعة تحديثات مبرمجة) خطوة تراجع واحدة.
- **أوراق الاتصال (التطبيق الأصلي / واجهة السطر):** حدّد الصور ثم **ملف → ورقة اتصال PDF…**. اختر A4 أو Letter، وأفقيّاً، وعدد الصفوف/الأعمدة وتسميات أسماء الملفات؛ واحفظ PDF مرقّم الصفحات واطبعه من عارض PDF لديك. تستخدم التصديرات التعديلات الحالية، وتلائم كل صورة دون قص إضافي، وتصيَّر بدقة 150 نقطة/بوصة في sRGB. يدعم التصدير في الخلفية الإلغاء ويحمي الأصول الأصلية وملفات XMP المرافقة. من واجهة السطر:

  ```sh
  lightcraft-cli run --import ~/Pictures/shoot library.selectAll export.contactSheet path=Contact.pdf paper=a4 columns=3 rows=4 captions=true
  ```

  يقبل `export.contactSheet` أيضاً `ids` صريحة، و`landscape=true`، و`paper=letter`. ويستبدل المخرجَ الموجود فقط بعد نجاح كامل ملف PDF. الحدود: 1,000 صورة، و100 صفحة، و256 ميبيبايت؛ تُرسَم التسميات بتغطية الخطوط ذاتها المستخدمة للعلامات المائية (الصينية واليابانية تتطلبان craft-fonts)، مع تقصير الأسماء الطويلة من المنتصف. هذا سير عمل أوراق اتصال،
  لا محرر تخطيطات طباعة أو حزم صور مخصصة.
- **كل عنصر قابل للعنونة** (`ui.widgets`) وقابل للنقر بالاسم، فيشغّل الوكلاء الواجهة الحقيقية لا باباً جانبياً.
- لقطات الشاشة في هذا الملف أُنتجت كاملةً بسكربتات [`docs/showcase/`](docs/showcase/).
  مرجع البروتوكول: [docs/control-protocol.md](docs/control-protocol.md).

<br>

## سريع، أصلي، خاص

- **Rust خالصة، لا C.** فواك raw خاصة بنا (DNG، وCanon CR2/CR3، وSony ARW، وNikon NEF، وFujifilm RAF بما فيها X-Trans، وPanasonic RW2 / Leica RWL، وPentax PEF، وOlympus ORF)، وعلم ألوان خاص بنا، ومسار خاص بنا. تفتح JPEG وPNG وTIFF وWebP وتركيبات PSD وJPEG XL اليوم؛ وصور HEIC/HEIF أيضاً مع `--features heif`‏ (‏HEVC خيار وقت البناء، كما في PhotoCraft).
- **مرجعي مشهدي وواسع المدى.** داخلياً: Rec.2020 خطية بفاصلة عائمة، وموازنة بيضاء بتكيّف برادفورد، وتخطيط نطاق بدلاً من القص، وكتف فيلمي لملفات raw وعبور مطابق للبكسل لملفات JPEG التي لم تلمسها.
- **تعديلات مستقلة عن الدقة.** أنصاف الأقطار وأحجام الفرش نسبية إلى الصورة، فتبدو معاينة 400 بكسل وشاشتك 5K وتصدير 60 ميغابكسل واحداً سواء.
- **تسارع GPU مع دقة CPU.** يعمل مسار التظهير كله كانوية حساب wgpu ‏(Metal / Vulkan / DX12)، مطابَقاً لمسار CPU إلى حد 1/255. على ملف raw بدقة 24 ميغابكسل (Apple M4 Pro): إعادة تصيير تحديث منزلق تستغرق ~4 ملّي ثانية، وعدسة 2.5 ميغابكسل باردة ~30 ملّي ثانية، وتصدير كامل الحجم ~0.3 ثانية شاملاً ترميز JPEG متوازٍ.
  من دون GPU يعمل المسار نفسه على كل أنوية CPU، معيداً حساب المراحل التي يمسّها المنزلق فقط. تصديرات الدفعات تصيّر عدة صور جنباً إلى جنب، فيتراكب فك ترميز إحداها وترميزها مع تصيير أخرى.
- **غربلة فورية.** فتح ملف raw يعرض معاينته الكاميرا المدمجة أو تصييره المخزّن خلال ~0.1 ثانية بينما يتبع التصيير الكامل (~0.2–0.5 ثانية لـ 24 ميغابكسل). تُجهَّز الصورتان التالية والسابقة في الخلفية، فتستغرق الإدارة عبر جلسة التصوير ~50 ملّي ثانية للصورة.
- **تصيير في الخلفية.** مجموعة عمال تصيّر العدسة و«قبل/بعد» وكل مصغّر مرئي خارج خيط الواجهة: مسودات أثناء السحب وجودة كاملة عند الإفلات.
- **محلي أولاً.** لا حساب، لا سحابة، لا قياس عن بعد، لا اشتراك. الفهرس الخاص بك سجل عمليات مقروءة بالحرف لا يُمسح منه شيء، يمكنك فروقه أو نسخه احتياطياً أو إعادة تشغيله.

<br>

## حالة المزايا

LightCraft فتيّ ويتقدّم بسرعة: **ألفا**. **أين نقف بصدق** (التفاصيل في [خريطة الطريق](ROADMAP.md) و[docs/target-app-parity.md](docs/target-app-parity.md)):

- **بعدد المزايا بلغنا ~81٪** من Lightroom ‏(المزايا الجوهرية 98٪)، متتبعين صفاً صفاً في
  [docs/parity-checklist.md](docs/parity-checklist.md).
- **كبديل يومي عن Lightroom نحن أقرب إلى 61٪ للمنتج كاملاً، و~49٪ لمصوّر محترف نموذجي، و~64٪ للاستخدام العابر.** التطبيق ممتاز مع JPEG/DNG ومعظم ملفات raw من Sony وPanasonic و
  Fujifilm وNikon وCanon الأقدم على جهاز واحد؛ انظر [docs/raw-parity.md](docs/raw-parity.md) لكاميراك.
- **أكبر الفجوات:**
  - **معايرة ألوان الكاميرات:** ملفات raw من Sony وNikon وPanasonic وFujifilm وCanon CR3 تستخدم تقديرات محروسة من JPEG كاميراتها، مع ملفات تعريفية مدمجة لـ ILCE-7CR وILCE-7M4 وX-H2S وX-T4؛ المعايرة المقيسة غائبة، وتبقى ملفات raw الأخرى أو المطابقات المرفوضة على مصفوفة محايدة؛
  - **ملفات raw المضغوطة من Olympus ومتغيرات CR3 غير المدعومة:** تستخدم معاينات JPEG المدمجة إن وُجدت. ملفات RAF المضغوطة (بلا فقد / بفقد) من Fujifilm تفك بيانات المستشعر الآن؛ [تعليمات التحقق وإعادة تحميل المكتبات القائمة](docs/raf-compression.md)؛
  - **أقنعة AI وإزالة الضوضاء:** تحديد الموضوع والسماء قواعد استدلالية تقليدية؛
  - **مكتبات الشبكة:** المعاينة تقرأ ترويسة ملف raw ومعاينة JPEG المدمجة فقط ‏(‏56.5 ميغابايت → 5.9 ميغابايت، و10 ثوانٍ → ثانية على مشاركة الاختبار)، لكن الصورة *المُظهّرة* ما زالت تحتاج بيانات المستشعر عبر الشبكة؛ انظر [docs/raw-preview-reads.md](docs/raw-preview-reads.md)؛
  - **عرض HDR على الشاشة، والفيديو، ووحدات Classic ‏Print / Book / Map.**
- **ما التالي:** انظر [الفجوات مرتبة](docs/gaps.md) و[خريطة الطريق](docs/roadmap.md).

| المجال | الحالة |
|---|---|
| المكتبة: تجميعات، مجلدات، تجميعات ذكية، رزم (بما فيها الرزم التلقائية)، نسخ افتراضية، تقييمات، أعلام، تسميات، شريط ترشيح، بحث، ترتيب، شبكات، شريط فيلم | ✅ |
| الغربلة: عرضا المقارنة (تكبير متزامن) والاستعراض، تقدّم تلقائي، معاينات فورية | ✅ |
| الضوء واللون والتأثيرات (أنماط تظليل الحواف)، منحنى النبرة (+ تنقية التشبّع والتعديل الموجّه)، مازج الألوان (+ موجّه)، لون نقطي، معايرة الألوان، أبيض وأسود | ✅ |
| الأقنعة: فرشاة، خطي، شعاعي، نطاق إضاءة/لون، إضافة/طرح/تقاطع | ✅ (موضوع/سماء AI بقواعد استدلالية تقليدية حالياً) |
| القص، أداة التعديل + تعديل تلقائي، انعكاس، تدوير، نسب عرض، شبكات | ✅ |
| ملفات تعريفية (ألوان، محايد، حيوي، مشاهد، بورتريه، أحادي: مظاهر من صنعنا)، إعدادات مسبقة، إصدارات، سجل، نسخ/لصق/مزامنة | ✅ |
| ألوان الكاميرات: ملفات DNG تستخدم مصفوفاتها الخاصة | ✅ DNG · 🚧 ملفات Sony/Fujifirm خاصة بنا؛ قاعدة معايرة مقيسة غائبة |
| شريط قوائم macOS الأصلي (مولَّد من سجل الأوامر)، قناة تحكم + كل عنصر قابل للعنونة، لقطات واجهة بلا واجهة | ✅ |
| RAW: DNG، CR2، CR3 (‏CRX باير بلا فقد وC-RAW إصدار 0x100/0x200)، ARW، NEF (غير مضغوط + مضغوط بلا/بفقد، بما فيه المضغوط بفقد بعد الفصل)، Fujifilm RAF (غير مضغوط + مضغوط بلا/بفقد، باير + X-Trans)، Panasonic RW2 / Leica RWL / Panasonic RAW (كل صيغ raw، من DMC-LX1 إلى DC-S1RM2)، Pentax PEF، Olympus ORF (غير مضغوط، 12.8 بت/بكسل)، Samsung SRW (غير مضغوط)؛ معاينات مدمجة لكل صيغة بما فيها CR3 | 🚧 · متغيرات CR3 الأخرى، وORF / SRW المضغوطة وNikon HE NEF ⬜ ([تعادل raw](docs/raw-parity.md)) |
| التفاصيل: الحدة، خفض ضوضاء الإضاءة + اللون | ✅ · إزالة ضوضاء AI 🚧 (باير، أوزان يثبّتها المستخدم)، الدقة الفائقة ⬜ |
| إزالة / مداواة / استنساخ البقع (مصدر تلقائي)، تصوّر البقع، عين حمراء وعين حيوانات (اكتشاف حدقة تلقائي، لمعة عين) | ✅ · تعبئة واعية بالمحتوى، تحرير دبابيس البقع 🚧 |
| التصدير: JPEG / PNG / TIFF / WebP / AVIF / DNG / الأصل، تحجيم، حد حجم ملف، حدة إخراج، قوالب تسمية، دفعات، سياسة بيانات وصفية، علامة مائية نصية أو صورية | ✅ |
| HDR: تحرير HDR مع سقف مساحة، مسودة SDR، تصوّر HDR؛ تصدير كخريطة كسب ISO 21496-1 JPEG أو PQ AVIF أو TIFF فاصلة عائمة 32-بت | ✅ · عرض HDR ⬜ |
| ثبات المكتبة (سجل عمليات آمن ضد الانهيار + لقطات، ضغط في الخلفية، إبلاغ عن عمليات الحفظ الفاشلة)، ذاكرة مصغرات على القرص | ✅ |
| الاستيراد: إضافة في مكانها / نسخ / نقل، قوالب إعادة تسمية ومجلدات، أجهزة، كشف التكرارات، مجلدات مراقَبة؛ استعراض مجلد محلي | ✅ |
| خادم MCP (بلا واجهة أو تطبيق حي، مكتبات ثابتة)، واجهة سطر، قناة تحكم | ✅ |
| ملفات XMP مرافقة (قراءة/كتابة، كتابة تلقائية)، قراءة إعدادات تظهير `crs:`، ملفات إعدادات مسبقة (`.lcpreset`، إعدادات XMP مسبقة) | ✅ |
| بصريات (تشويه، تظليل حواف، زغب كروماتي تلقائي + يدوي، إزالة الحواف الملوّنة، تصحيحات عدسات مدمجة في DNG وبيانات تشويه Panasonic / Leica RW2 / RWL)، هندسة (تحويلات، قص مقيَّد)، Upright ‏(تلقائي/مستوٍ/رأسي/كامل/موجَّه) | ✅ · ملفات تعريفية لعدسات الكاميرات (من صنعنا) ⬜ |
| دمج الصور: HDR (محاذاة تلقائية، إزالة الأشباح)، بانوراما (كروية/أسطوانية/منظورية، انبعاج الحواف، قص تلقائي)، بانوراما HDR ← DNG | ✅ |
| مسار GPU (حساب wgpu، مطابق لـ CPU إلى 1/255)، احتياط CPU عند حدود/أخطاء الجهاز | ✅ · WebGPU في المتصفح 🚧 |
| AI: أقنعة تجزئة، إزالة ضوضاء AI، دقة فائقة، وجوه؛ فيديو | 🚧 (انظر [الفجوات](docs/gaps.md)) |
| بناء الويب (الواجهة نفسها في المتصفح عبر WASM): مكتبة ثابتة في OPFS/IndexedDB، تصيير في Web Worker، تنزيل المخرجات | ✅ · WebGPU، واختبار Safari/Firefox 🚧 |

<sub>✅ يعمل اليوم · 🚧 جارٍ العمل عليه · ⬜ لم يبدأ</sub>

<br>

## البداية السريعة

بناء مساحة العمل، بما فيها تطبيقات سطح المكتب والويب وواجهة السطر، يتطلب Rust ‏1.95 أو أحدث.

```sh
git clone https://github.com/storytold/lightcraft && cd lightcraft
cargo run --release -p lightcraft                       # opens your library (~/Pictures/LightCraft Library; a new one starts with demo photos)
cargo run --release -p lightcraft -- ~/Pictures/trip    # import your photos (folders are scanned, duplicates skipped)
cargo run --release -p lightcraft -- --memory           # a throwaway in-memory demo session (writes nothing)
cargo run --release -p lightcraft -- --control 7980     # with the automation channel
cargo xtask web --serve                                 # the same app in the browser: http://127.0.0.1:8080/
cargo run --release -p lightcraft-cli -- render photo.jpg -o out.jpg --set light.exposure=0.5
cargo xtask ci                                          # fmt, clippy, tests, layering, wasm checks
```

تقصّ CI افتراضياً معلومات تصحيح جداول الأسطر ووظيفة بناء واحدة لكل 1.5 ميغابايت ذاكرة
متاحة عند بدئه (واحدة لكل معالج على الأكثر؛ و4 إن تعذّرت قراءة الذاكرة)، فجهاز
بذاكرة 8 ميغابايت فيها 6 متاحة يبني بـ 4 وظائف، والجهاز المزدحم قليل الفضاء يبني
بأقل. تتبع خيوط الاختباعات ذلك بحد أقصى 4. روابط معلومات التصحيح الكاملة، واحد
لكل معالج، تستنفد الذاكرة قبل تشغيل أي اختبار وإلا. تغطية GPU كما هي. تتجاوز
إعدادات `CARGO_PROFILE_DEV_DEBUG` و`CARGO_BUILD_JOBS` و`RUST_TEST_THREADS` الصريحة
هذه الافتراضيات. أوامر التطوير المعتادة تحتفظ بإعداداتها الخاصة.

**النصوص العربية والصينية واليابانية** تحتاج مستودع الخطوط المشترك، وهو مدخل بناء اختياري (الإصدارات الرسمية تتضمنه دائماً):

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
CRAFT_FONTS_DIR=../craft-fonts cargo run --release -p lightcraft
```

من دونه يُبنى LightCraft ويعمل سواء بسواء، لكن النصوص العربية والصينية واليابانية بلا محارف. لا تُودَع الخطوط أبداً في هذا
المستودع — معيار ما يدخل هناك يعيش معه: انظر [README‏ craft-fonts](https://github.com/storytold/craft-fonts#readme).

**لغة الواجهة:** **تحرير → لغة** (English، 简体中文， 繁體中文（台灣）， 日本語، Português (Brasil)، Español، Deutsch، Русский، Français، العربية) أو **إعدادات → عام →
لغة**؛ يُطبَّق الاختيار فوراً ويُحفظ. انظر [docs/localization.md](docs/localization.md).

**السجلات:** يكتب تطبيق سطح المكتب سجله إلى الخرج القياسي وإلى `logs/lightcraft.log` في مجلد إعداداته
‏(‏Linux: `$XDG_CONFIG_HOME/lightcraft/logs/` افتراضياً `~/.config/lightcraft/logs/`؛ وmacOS:
`~/Library/Application Support/LightCraft/logs/`؛ وWindows: `%APPDATA%\LightCraft\logs\`)، ولا يكتب شيئاً في المكتبة أبداً. إطلاق
من قائمة سطح المكتب أو Dock بلا طرفية، فأرفق هذا الملف بتقرير الخطأ (**مساعدة ▸ فتح مجلد السجلات**
يعرضه في مدير الملفات). كل تشغيل ينقل السجل السابق إلى
`lightcraft.1.log` وذاك إلى `lightcraft.2.log`، فينجو سجل جلسة انهارت من
التشغيل التالي؛ يتوقف الملف عن النمو عند 16 ميغابايت، ولا يكتب `--version` و`--help` شيئاً، والجلسات
بـ `LIGHTCRAFT_NO_PREFS` تسجل إلى الخرج القياسي فقط. افتراضياً تسجل صناديق LightCraft نفسها بمستوى `info` وكل ما عداها
بمستوى `warn`. يعمل `LIGHTCRAFT_LOG=info` أو `debug` كالسابق (ذاك المستوى لصناديق LightCraft نفسها، وتحذيرات
وأخطاء من البقية؛ أي قيمة أخرى: تحذيرات وأخطاء فقط) ويتقدم على `RUST_LOG`، الذي يستبدل الافتراضي وإلا بتوجيهات بنمط env_logger مثل
`RUST_LOG=debug` أو `RUST_LOG=warn,lightcraft_pipeline=trace` (توجيه ينتهي بـ
`*` يغطي كل هدف يبدأ به، كما في `lightcraft*=debug`). تُسجَّل حالات الهلع (panics) هناك
أيضاً، وتبقى في `lightcraft-panics.log` في مجلد المؤقتات. المسجِّل هو `apps/lightcraft/src/logging.rs`.

يحتاج بناء الويب الهدف `wasm32-unknown-unknown` وأداة `wasm-bindgen` CLI المطابقة
‏(‏`cargo xtask web` تطبع أمر التثبيت الدقيق)؛ انظر [docs/web.md](docs/web.md).

يبني **Nix** تطبيق سطح المكتب و`lightcraft-cli` (بناء Nix يتضمن دائماً مدخل craft-fonts، فالنصوص
اليابانية لها محارف):

```sh
nix run github:storytold/lightcraft                    # the desktop app
nix build github:storytold/lightcraft                  # → ./result/bin/{lightcraft,lightcraft-cli}
nix develop github:storytold/lightcraft                # rust toolchain + native deps + fonts
```

في إعداد flake ‏(‏NixOS، home-manager، nix-darwin):

```nix
# flake.nix
inputs.lightcraft.url = "github:storytold/lightcraft";
# optional: build against your own nixpkgs instead of the one LightCraft pins
# inputs.lightcraft.inputs.nixpkgs.follows = "nixpkgs";

# then, in a NixOS or home-manager module (where `inputs` is in scope):
nixpkgs.overlays = [ inputs.lightcraft.overlays.default ];   # makes `pkgs.lightcraft` available
environment.systemPackages = [ pkgs.lightcraft ];           # home-manager: home.packages = [ pkgs.lightcraft ];
```

يثبّت `nix build` ملف سطح المكتب ذاته وأيقونات hicolor وبيانات AppStream مثل ‎.deb/.rpm، ويشغّل
`cargo test --workspace` في مرحلة التحقق (تخطَّها بـ `pkgs.lightcraft.overrideAttrs { doCheck = false; }`).

**لوحة المفاتيح:** <kbd>G</kbd> الشبكة · <kbd>D</kbd> التفاصيل · <kbd>E</kbd> التحرير · <kbd>C</kbd> القص · <kbd>M</kbd> الأقنعة ·
<kbd>Shift</kbd>+<kbd>P</kbd> الإعدادات المسبقة · <kbd>\\</kbd> الأصل · <kbd>Y</kbd> قبل/بعد · <kbd>Z</kbd> تكبير ·
<kbd>J</kbd> اقتصاص · <kbd>⌘Z</kbd> تراجع · <kbd>⌘/</kbd> كل الاختصارات.

## كيف يُبنى

مساحة عمل Cargo محورية حول المحرك، من صناديق صغيرة مختبَرة مع فرض طبقات (`cargo xtask layers`): ‏`geom`،
`color`، `raster`، `tiff` → `raw`، `codecs`، `meta`، `develop` → `pipeline` → `catalog` → `engine` → `ui-egui`.
واجهة egui صندوق قابل للتبديل؛ لا شيء تحته يعلم بوجود واجهة. يعلو `mcp` والتطبيقات ‏(`lightcraft`،
`lightcraft-cli`) فوق `engine`.

## المساهمة

البشر والوكلاء يتبعون القواعد نفسها، فاقرأ [AGENTS.md](AGENTS.md) أولاً. باختصار:

- **الغرفة النظيفة.** لا تقرأ أبداً ملفات Adobe الثنائية أو شيفرة raw/صور بترخيص GPL ‏(‏darktable، RawTherapee، LibRaw، rawspeed، dcraw…)؛
  اعمل من المواصفات العامة والملاحظة الصندوقية السوداء.
- **لا أصول Adobe إطلاقاً:** لا أيقونات أو لقطات شاشة أو إعدادات مسبقة أو ملفات تعريفية أو LUTs أو خطوط من منتجات Adobe. كل
  صورة وأيقونة وخط في المستودع أصلية أو ملكية عامة أو بترخيص Creative Commons أو OFL أو ترخيص متساهل، ولها
  مدخل في [assets/ATTRIBUTION.md](assets/ATTRIBUTION.md) يضاف في الالتزام نفسه. الخطوط الجديدة تذهب إلى
  [storytold/craft-fonts](https://github.com/storytold/craft-fonts)، لا إلى هنا.
- **Rust خالصة**، فرض طبقات الصناديق، كل شيء أمر، و`cargo xtask ci` أخضر قبل كل التزام
  (معرّف مهمة واحد لكل التزام).
- **لا انهيار أبداً.** تُعيد الشيفرة غير الاختبارية أخطاءً بدل الهلع: لا `unwrap()` ولا `expect()` ولا
  `panic!` ولا `unsafe`، وفهرسة مفحوصة لأي شيء مشتق من مدخلات، واختبار انحدار مع كل إصلاح انهيار. التفاصيل في
  [AGENTS.md](AGENTS.md#never-crash-outranks-feature-work).

أسئلة أو أفكار أو خطأ تحب مناقشته أولاً؟ اطرحها على [Discord](https://discord.gg/artcraft).

<br>

## التنزيلات

**جديد على LightCraft؟** نزّله من [صفحة LightCraft على getartcraft.com](https://getartcraft.com/apps/lightcraft). تلك أسهل طريقة للتثبيت.

**تريد بناءً أو صيغة بعينها؟** على GitHub، [آخر إصدار](https://github.com/storytold/lightcraft/releases/latest) يعرض كل البناءات أدناه، و[كل الإصدارات](https://github.com/storytold/lightcraft/releases) تعرض الإصدارات السابقة وملاحظاتها. `<ver>` في أسماء الملفات هو رقم الإصدار، و`SHA256SUMS.txt` يسرد بصمة تحقق لكل ملف.

### Windows

| البناء | المثبِّت | المحمول |
|---|---|---|
| x64 ‏(‏64-بت Intel/AMD) | `lightcraft-<ver>-windows-x64.msi` | `lightcraft-<ver>-windows-x64-portable.zip` |
| arm64 ‏(‏Snapdragon وحواسيب ARM الأخرى) | `lightcraft-<ver>-windows-arm64.msi` | `lightcraft-<ver>-windows-arm64-portable.zip` |
| x86 ‏(‏32-بت) | `lightcraft-<ver>-windows-x86.msi` | `lightcraft-<ver>-windows-x86-portable.zip` |

المثبِّتات والملفات التنفيذية موقّعة شيفرياً. يسأل المثبِّت عن مكان التثبيت (`C:\Program Files\LightCraft`
افتراضياً؛ الترقيات تبقى في المجلد الذي اخترته) وينتهي بصفحة تؤكد تثبيت LightCraft مع خيار
تشغيله. للتثبيت غير المراقَب: `msiexec /i lightcraft-<ver>-windows-x64.msi /qn INSTALLFOLDER="D:\Apps\LightCraft\"`.

### macOS

| البناء | الملف | ملاحظات |
|---|---|---|
| التطبيق، شامل (سيليكون Apple + Intel) | `lightcraft-<ver>-macos-universal.dmg` | موقّع وموثّق لدى Apple |
| أداة سطر الأوامر، شامل | `lightcraft-cli-<ver>-macos-universal.zip` | موقّعة وموثّقة لدى Apple |

### Linux

| الصيغة | x86_64 | aarch64 ‏(‏ARM64) | ملاحظات |
|---|---|---|---|
| AppImage | `lightcraft-<ver>-linux-x86_64.AppImage` | `lightcraft-<ver>-linux-aarch64.AppImage` | يعمل في أي مكان؛ يحدّث نفسه عبر [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate) (ملفات `.zsync`) |
| Flatpak | `lightcraft-<ver>-linux-x86_64.flatpak` | `lightcraft-<ver>-linux-aarch64.flatpak` | معزول؛ `flatpak install --user <file>` |
| Debian/Ubuntu | `lightcraft-<ver>-linux-x86_64.deb` | `lightcraft-<ver>-linux-aarch64.deb` | |
| Fedora/RHEL/openSUSE | `lightcraft-<ver>-linux-x86_64.rpm` | `lightcraft-<ver>-linux-aarch64.rpm` | |
| Tarball | `lightcraft-<ver>-linux-x86_64.tar.gz` | `lightcraft-<ver>-linux-aarch64.tar.gz` | فُكّ في أي مكان |

‏RISC-V ‏(‏riscv64): ‏`lightcraft-<ver>-linux-riscv64.tar.gz`، حزمة tar فقط (‏glibc ‏2.39 أو أحدث، مثل Ubuntu ‏24.04+).

على سطح مكتب Wayland يعمل LightCraft عبر XWayland إن توفر، لأن X11 وحده يمنح النوافذ
سحب ملفات وإفلاتها؛ عيّن `LIGHTCRAFT_WAYLAND=1` لاستخدام Wayland أصلي (تحجيم كسري أحد، دون سحب).

### FreeBSD

| البناء | الملف |
|---|---|
| x86_64 | `lightcraft-<ver>-freebsd-x86_64.tar.gz` |

### الويب (‏WebAssembly)

| البناء | الملف | ملاحظات |
|---|---|---|
| موقع ثابت | `lightcraft-web-<ver>.zip` | يعمل في متصفح حديث؛ استضِفه على أي خادم ثابت |

<br>

## تطبيقات Crafting Apps

LightCraft أحد **تطبيقات Crafting Apps**: أدوات إبداعية حرة مفتوحة المصدر من فريق
[ArtCraft](https://getartcraft.com/)، كلٌّ مكتوب من الصفر بـ Rust وقادر على الوقوف وحده.

| | التطبيق | وظيفته | الشيفرة | اعرف أكثر |
|:-:|---|---|---|---|
| <img src="https://raw.githubusercontent.com/storytold/photocraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.photocraft.png" alt="" width="32" height="32"> | **PhotoCraft** | تحرير الصور: طبقات وأقنعة وكتابة وملفات PSD حقيقية | [GitHub](https://github.com/storytold/photocraft) | [الموقع](https://getartcraft.com/apps/photocraft) |
| <img src="https://raw.githubusercontent.com/storytold/vectorcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.vectorcraft.png" alt="" width="32" height="32"> | **VectorCraft** | الرسم المتجهي | [GitHub](https://github.com/storytold/vectorcraft) | [الموقع](https://getartcraft.com/apps/vectorcraft) |
| <img src="https://raw.githubusercontent.com/storytold/filmcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.filmcraft.png" alt="" width="32" height="32"> | **FilmCraft** | تحرير الفيديو واللون والصوت | [GitHub](https://github.com/storytold/filmcraft) | [الموقع](https://getartcraft.com/apps/filmcraft) |
| <img src="https://raw.githubusercontent.com/storytold/lightcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.lightcraft.png" alt="" width="32" height="32"> | **LightCraft** | **مكتبة الصور وتظهير raw · أنت هنا** | [GitHub](https://github.com/storytold/lightcraft) | [الموقع](https://getartcraft.com/apps/lightcraft) |
| <img src="https://raw.githubusercontent.com/storytold/pdfcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.pdfcraft.png" alt="" width="32" height="32"> | **PdfCraft** | قراءة ملفات PDF وتنظيمها وحمايتها | [GitHub](https://github.com/storytold/pdfcraft) | [الموقع](https://getartcraft.com/apps/pdfcraft) |
| <img src="https://raw.githubusercontent.com/storytold/effectcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.effectcraft.png" alt="" width="32" height="32"> | **EffectCraft** | الرسوم المتحركة والمؤثرات البصرية | [GitHub](https://github.com/storytold/effectcraft) | [الموقع](https://getartcraft.com/apps/effectcraft) |
| <img src="https://raw.githubusercontent.com/storytold/designcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.designcraft.png" alt="" width="32" height="32"> | **DesignCraft** | تخطيط الصفحات والنشر | [GitHub](https://github.com/storytold/designcraft) | [الموقع](https://getartcraft.com/apps/designcraft) |

و[**ArtCraft**](https://getartcraft.com/) نفسه، استوديو صور وفيديو بالذكاء الاصطناعي للفنانين الذين يريدون تحكماً حقيقياً.

## الترخيص والإسنادات

LightCraft مرخّص مزدوجاً بموجب [MIT](LICENSE-MIT) أو [Apache-2.0](LICENSE-APACHE)، بحسب اختيارك.
حقوق النشر (c) 2026 لفريق ArtCraft ومساهمي LightCraft. الإشعارات المطلوبة في [NOTICE](NOTICE).

الخطوط والأيقونات والصور والأصول الأخرى المدمجة تحتفظ بتراخيصها المفتوحة الخاصة؛ كلٌّ منها مُدرج
مع مؤلفه ومصدره وترخيصه في [assets/ATTRIBUTION.md](assets/ATTRIBUTION.md).

صور العرض أعمال ملكية عامة، مستخدمة عبر ويكيميديا كومنز: أنسل آدمز، *التيتونز ونهر الأفعى*
‏(‏1942، الأرشيف الوطني الأمريكي)؛ دوروثيا لانج، *الأم المهاجرة* ‏(‏1936، مكتبة الكونغرس)؛ بيل أندرس / ناسا،
*شروق الأرض* ‏(‏1968)؛ ناسا، *الرخامة الزرقاء* ‏(‏1972). مكتبة العرض مولّدة إجرائياً بـ LightCraft. خط الواجهة:
‏Inter ‏(‏SIL OFL 1.1). البناءات المصنوعة بـ [craft-fonts](https://github.com/storytold/craft-fonts) ‏(‏كل الإصدارات الرسمية)
تضمّن أيضاً خطوطه العربية والصينية واليابانية ‏(‏Noto Sans Arabic، Noto Sans CJK SC، BIZ UDPGothic، BIZ UDMincho، Shippori Mincho؛ SIL OFL 1.1)، المدرجة في
[ATTRIBUTION.md](https://github.com/storytold/craft-fonts/blob/main/ATTRIBUTION.md) عنده. كل الأيقونات أصلية.

اسم ArtCraft وعلامته الكتابية وشعاراته في [`docs/brand/`](docs/brand/) علامات تجارية لفريق
ArtCraft ولا يشملها هذا الترخيص. يجوز استخدامها دون تعديل فقط، وفقط كجزء
من هذا المستودع وLightCraft، بموجب [`docs/brand/LICENSE-brand.txt`](docs/brand/LICENSE-brand.txt).
على النسخ المشتقة والنسخ المعدلة إزالتها.

<sub>‏Adobe وPhotoshop وIllustrator وPremiere Pro وLightroom وAcrobat وAfter Effects وInDesign علامات تجارية أو علامات تجارية مسجلة لشركة Adobe Inc. في الولايات المتحدة و/أو دول أخرى. LightCraft مشروع مستقل مفتوح المصدر، لا يتبع Adobe Inc. ولا ترعاه ولا تصادق عليه؛ هذه الأسماء مستخدمة فقط لوصف مسارات العمل المتوافق معها.</sub>

<p align="center">
  <a href="https://getartcraft.com/"><img alt="ArtCraft" src="docs/brand/artcraft-mark.svg" width="28"></a><br>
  <sub>من صنع فريق <a href="https://getartcraft.com/">ArtCraft</a> والمجتمع.</sub>
</p>

## تاريخ النجوم

[![Star History Chart](https://api.star-history.com/svg?repos=storytold/lightcraft&type=Date&legend=top-left)](https://www.star-history.com/?repos=storytold%2Flightcraft&type=date&legend=top-left)

</div>

<br>

<p align="center">
  <a href="https://discord.gg/artcraft"><img alt="Join the ArtCraft community on Discord" src="https://img.shields.io/badge/Join%20us%20on%20Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white" height="40"></a>
</p>

<h3 align="center">تعال اصنع الأشياء معنا</h3>

<div dir="rtl">

<p align="center">
  Discord عندنا هو المكان الذي يتسكع فيه فنانو كل الأنواع: من يرسمون، ويصوّرون، وينقّطون،
  يقطعون الفيلم، ويضبطون الحروف، ومن ما زالوا يكتشفون ما يحبون صنعه. شارك ما تعمل عليه،
  اطلب المساعدة، وأخبرنا بما تعطّل، أو بما تتمنى أن تستطيع هذه الأدوات فعله.
  مهما كانت وساطتك ومهما طال وقتك فيها، لك مكان هنا.
</p>

<p align="center">
  <a href="https://discord.gg/artcraft"><b>discord.gg/artcraft</b></a> ·
  <a href="https://getartcraft.com/">getartcraft.com</a> ·
  <a href="https://getartcraft.com/apps">تطبيقات Crafting Apps</a> ·
  <a href="https://github.com/storytold">GitHub</a>
</p>

</div>
