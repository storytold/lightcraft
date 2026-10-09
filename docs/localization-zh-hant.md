# 繁體中文（台灣）介面

> `<PREFIX>`, `<cli>`, `<binary>`: [`brand.toml`](../brand.toml).

從「編輯 → 語言 → 繁體中文（台灣）」切換，或開啟「設定 → 一般 → 語言」選擇。
macOS 的設定也可由「應用程式選單 → 設定…」或 `⌘,` 開啟。
語言立即生效，並儲存在應用程式設定的 `ui.json`，下次啟動會沿用。
英文、簡體中文、繁體中文與日文可隨時互相切換。

繁中翻譯涵蓋 macOS 原生選單、程式內選單與子選單、鍵盤快速鍵說明、照片編輯、
裁切、遮色片、匯入／匯出、設定、41 個內建編輯預設集與分類、內建描述檔、
曲線與匯出預設集、歷史紀錄／版本面板、內建照片來源標題、日期群組與拍攝時間、
照片庫開啟失敗提示及主要處理進度。
切換介面不會變更操作指令 ID、照片檔名、使用者命名的相簿、預設集或中繼資料。
未翻譯的技術錯誤、發行說明及部分新增文案仍使用英文。

## 建置與字型

使用既有 [craft-fonts](https://github.com/storytold/craft-fonts) 字型作為建置輸入：

```sh
CRAFT_FONTS_DIR=../craft-fonts cargo run --release -p dac-app
```

craft-fonts 目前沒有繁體中文專用字型：桌面版先以 Noto Sans CJK SC 顯示繁中文字（字形為大陸規範寫法），其次才是日文字型；不在此儲存庫新增字型檔。
未指定 `CRAFT_FONTS_DIR` 的建置缺少 CJK 字形。
Web 建置在 WASM 中嵌入 BIZ UDPGothic Regular，並另外載入 Noto Sans CJK SC；繁中文字形仍採用簡中規範寫法。

## 自動化與維護

`<PREFIX>_LANGUAGE=zh-hant <cli> snapshot --demo -o zh-hant.png` 可繪製繁中畫面。
接受 `zh-hant`、`zh-Hant`、`zh-TW`、`zh_TW.UTF-8` 與 `zh-HK`；儲存時統一使用 `zh-hant`。
控制通道可切換語言：

```json
{"method":"ui.set","params":{"language":"zh-hant"}}
{"method":"ui.menu.invoke","params":{"command":"app.language.traditionalChinese"}}
```

靜態文案位於 `crates/ui-egui/locales/zh-hant.json`，含變數的訊息位於
`crates/ui-egui/locales/zh-hant-formats.json`。動態訊息由 Rust 在建置時檢查格式參數，
所有 `*-formats.json` 須保持相同英文鍵。產品名稱不寫入譯文，改用 `{app}`（`brand.toml` 的顯示名稱）、`{cli}`（命令列程式名稱）與 `{env}`（環境變數前綴），譯文原樣保留，例如 `About {app}` 譯為 `關於 {app}`。新增語言的方法見 [`localization.md`](localization.md)。`{:.0}` 消耗英文複數字尾引數，繁中不顯示該字尾。

```sh
CRAFT_FONTS_DIR=../craft-fonts cargo test -p dac-ui-egui i18n::tests
```

測試涵蓋翻譯目錄、指令／選單文字、語言切換、設定保存、資料名稱保持原樣及桌面字形。
