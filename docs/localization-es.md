# LightCraft en español

Elegir **Ajustes → General → Idioma → Español**. El cambio se aplica al
momento y se conserva para el próximo inicio (`language: "es"` en `ui.json`). `LIGHTCRAFT_LANGUAGE`
acepta cualquier variante del español (`es`, `es-ES`, `es_MX.UTF-8`, `es-419`…).

El catálogo en español cubre todos los textos de la interfaz: los menús, los métodos abreviados
de teclado, la biblioteca, el revelado, el recorte, las máscaras, la importación y la exportación,
los ajustes, los ajustes preestablecidos y perfiles integrados, el historial, la impresión, las
presentaciones, las fechas y los mensajes de progreso. Como en los demás idiomas, los errores
técnicos de los niveles inferiores siguen en inglés, y los textos que aún no están en el catálogo
se muestran en inglés.

Los nombres de archivo, los identificadores de comandos y controles, las plantillas y los álbumes,
ajustes preestablecidos y metadatos con nombres propios no se traducen. La fuente de la interfaz,
Inter, incluye todos los caracteres del español; no hacen falta fuentes adicionales.

## Cobertura

Recuento del informe `i18n::tests::translation_gaps_report` al completar la cobertura de la
interfaz:

| | Antes | Después |
|---|---|---|
| Mensajes sin traducir | 367 | 2 |
| Mensajes con valores sin traducir | 9 | 0 |
| Líneas de Novedades sin traducir | 1821 | 1821 |

Los dos mensajes restantes son líneas antiguas de Novedades que otros catálogos conservan y que
el informe sigue contando; no son texto de la interfaz y no se traducen. Por tanto, no falta
ningún mensaje de la interfaz ni ningún mensaje con valores. Las líneas de **Novedades** se dejan
en inglés a propósito: son notas de una versión concreta y cambian con cada publicación.

## Terminología

Las cadenas nuevas usan la segunda persona del singular (tú) en las instrucciones y los mensajes
directos: «Haz clic…», «Elige…», «Arrastra…». Las cadenas antiguas que usan formas impersonales o
infinitivos («Hacer clic…») se conservan hasta que se revisen. Algunos términos que conviene
mantener coherentes:

| Inglés | Español |
|---|---|
| Highlights / Shadows | Iluminaciones / Sombras |
| Dehaze | Eliminar neblina |
| Vibrance | Intensidad |
| Tint | Matiz |
| Hue (HSL) | Tono |
| Color Grading | Gradación de color |
| Preset | Ajuste preestablecido |
| Pick / Reject | Seleccionada / Rechazada |
| Upright | Vertical |
| Lens Corrections | Correcciones de lente |
| Loupe | Ampliación |
| Print cell | Celda de impresión |
| Slideshow | Presentación |
| Mask effect | Efecto de máscara |
| Background preparation | Preparación en segundo plano |
| HDR headroom | Margen HDR |
| What's New | Novedades |

Se unificó «Highlights» como «Iluminaciones» en todo el catálogo, incluidas las etiquetas del
mezclador de color y el estilo de viñeteado, para usar el término del Lightroom actual en español.
Las versiones antiguas de Lightroom usaban «Altas luces».

Se mantienen sin traducir las marcas y los nombres de producto (LightCraft, PhotoCraft, Lightroom,
lensfun, intoPIX TicoRAW, Adobe RGB), los formatos y códecs (JPEG, PNG, TIFF, WebP, DNG, RAW, AVIF,
ONNX, ICC, LUT, TRC), las extensiones de archivo y los especificadores de formato (`{name}`, `{n}`,
`{:.1}`, `{d:+}`, `{:.0}%`). «B&W» se traduce como «ByN», como en el resto del catálogo.

Las correcciones de hablantes nativos son bienvenidas: abrir un issue o un pull request con el
texto inglés, la traducción actual y la propuesta.

## Mantenimiento y comprobación

Textos fijos: `crates/ui-egui/locales/es.json`. Mensajes con valores:
`crates/ui-egui/locales/es-formats.json`. Las claves en inglés no cambian; el compilador de Rust
comprueba los marcadores al compilar. Las terminaciones de plural del inglés se ocultan con `{:.0}`.

```sh
cargo test -p lightcraft-ui-egui i18n::tests -- --nocapture
cargo test -p lightcraft-ui-egui --lib -- --exact i18n::tests::translation_gaps_report --nocapture
LIGHTCRAFT_LANGUAGE=es lightcraft-cli snapshot --demo -o espanol.png --size 1600x1000
```

Las pruebas de glifos se omiten si no se compila con `CRAFT_FONTS_DIR`; el resto del catálogo se
comprueba igual.
