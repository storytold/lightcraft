## Building

- **Single-language builds:** building from source with `CRAFT_FONTS_LANGUAGE=<code>` embeds only the fonts that language's interface needs and offers only that language and English. Release builds are unchanged.
- **Release size check:** each release job compares its downloads with a stored size budget and fails on more than 10 % growth.
