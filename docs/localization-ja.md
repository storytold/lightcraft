# 日本語表示

> `<PREFIX>`, `<cli>`, `<binary>`: [`brand.toml`](../brand.toml).

本アプリは日本語・簡体字中国語・英語（English）の表示に対応しています。
言語設定は `ui.json` の `language` に保存します。

- 「編集 → 言語」または「設定 → 一般 → 言語」で切り替えます。選択は次回起動にも引き継ぎます。
- メニュー、写真編集、マスク、切り抜き、設定、読み込み、書き出し、主な処理状況を翻訳しています。
- 通常表示と太字の双方に、SIL OFLのBIZ UDPGothicを使います。日本語フォントはこのリポジトリではなく
  [storytold/craft-fonts](https://github.com/storytold/craft-fonts) にあり、任意のビルド入力
  `CRAFT_FONTS_DIR=../craft-fonts` を指定したビルド（公式リリースはすべて）に組み込まれます。
  指定しない場合も動作しますが、日本語の文字は表示されません。
- 翻訳は表示層で行います。操作コマンドのID、写真のファイル名、入力したメタデータを変更しません。
- 翻訳されていない技術的なエラー、追加情報、リリースノートは英語で表示します。
- 中国語表示は [`localization-zh-hans.md`](localization-zh-hans.md) を参照してください。
- 別の言語の追加方法は [`localization.md`](localization.md) を参照してください。

## 翻訳の保守

静的な表示文言は `crates/ui-egui/locales/ja.json`、可変値を含む表示文言は
`crates/ui-egui/locales/ja-formats.json` にあります。英文をキーにして翻訳を管理します。
製品名は訳文に書かず、プレースホルダー `{app}`（`brand.toml` の表示名）を使います。コマンドライン名は `{cli}`、
環境変数の接頭辞は `{env}` です。訳文でもそのまま残します（例: `About {app}` → `{app}について`）。

可変文言の訳文はすべてビルド時にRustの `format!` 検査を受けます。値は名前（`{n}`）または
位置（`{0}`、`{1}`）で参照し、`{:.1}` などの書式指定は英文と同じものを使います。
英語の単複数語尾を日本語で省略する場合、対応する文字列引数は `{:.0}` で空にします。
`<PREFIX>_LANGUAGE=ja <cli> snapshot ...` で日本語の画面を描画できます。

スマートアルバムの「はい／いいえ」で答える条件（編集、切り抜きなど）の値は、文脈付きキー
`"smart album value|Yes"`・`"smart album value|No"` で「あり／なし」と表示します。共有の
`"Yes"`・`"No"`（はい／いいえ）は他の画面でそのまま使います（[`localization.md`](localization.md) の文脈の説明を参照）。

表示・フォント・言語の切り替え・設定の保存・コマンドIDの保持は
`cargo test -p dac-ui-egui i18n::tests` で検証します（グリフの検査は `CRAFT_FONTS_DIR` 指定時のみ実行）。
