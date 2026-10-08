# LightCraft em português do Brasil

O LightCraft pode ser exibido em inglês, chinês simplificado, chinês tradicional (Taiwan), japonês e
português do Brasil. A preferência de idioma é gravada em `language` no `ui.json`.

- Troque em **Editar → Idioma** ou em **Configurações → Geral → Idioma**. A escolha é mantida nas
  próximas inicializações.
- Traduzimos menus, edição de fotos, máscaras, recorte, configurações, importação, exportação e as
  principais mensagens de progresso.
- Menus, telas e o texto com valores variáveis usam as fontes Inter (regular e semibold). Texto em
  chinês e japonês exibido nos nomes de idioma (por exemplo, 日本語) continua dependendo do repositório
  de fontes [storytold/craft-fonts](https://github.com/storytold/craft-fonts).
- A tradução fica na camada de apresentação. IDs de comandos, nomes de arquivos de fotos e metadados
  digitados pelo usuário nunca são alterados.
- Erros técnicos, informações complementares e notas de versão não traduzidos aparecem em inglês.
- A tradução para chinês está em [`localization-zh-hans.md`](localization-zh-hans.md); para japonês,
  em [`localization-ja.md`](localization-ja.md). Como adicionar outro idioma está em
  [`localization.md`](localization.md).

## Manutenção da tradução

Os textos fixos ficam em `crates/ui-egui/locales/pt-br.json` e os textos com valores variáveis em
`crates/ui-egui/locales/pt-br-formats.json`. O inglês é usado como chave.

Toda tradução de formato é verificada por `format!` do Rust no build. Referencie os valores por nome
(`{n}`) ou por posição (`{0}`, `{1}`) e use as mesmas especificações de formato do inglês
(`{:.1}`, `{d:+}`, `{:.0}%`). Quando o plural inglês (`"s"` / `""`) não for necessário em português,
consuma o argumento correspondente com `{:.0}` — um texto com precisão 0 não imprime nada.

Aparência, fontes, troca de idioma, persistência da preferência e estabilidade dos IDs de comando são
verificadas por `cargo test -p lightcraft-ui-egui i18n::tests` (a checagem de glifos só roda com
`CRAFT_FONTS_DIR`). Para visualizar o idioma, renderize sem interface gráfica:

```sh
LIGHTCRAFT_LANGUAGE=pt-br lightcraft-cli snapshot --demo --script tour.jsonl -o out.png --size 1600x1000
```
