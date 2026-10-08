# LightCraft en español

Elegir **Editar → Idioma → Español** o **Ajustes → General → Idioma**. El cambio se aplica al
momento y se conserva para el próximo inicio (`language: "es"` en `ui.json`). `LIGHTCRAFT_LANGUAGE`
acepta cualquier variante del español (`es`, `es-ES`, `es_MX.UTF-8`, `es-419`…).

El catálogo en español cubre los menús, los métodos abreviados de teclado, la biblioteca, el
revelado, el recorte, las máscaras, la importación y la exportación, los ajustes, los ajustes
preestablecidos y perfiles integrados, el historial, las fechas y los mensajes de progreso. Como en
los demás idiomas, los errores técnicos de los niveles inferiores siguen en inglés, y los textos
que aún no están en el catálogo se muestran en inglés.

Los nombres de archivo, los identificadores de comandos, las plantillas y los álbumes, ajustes
preestablecidos y metadatos con nombres propios no se traducen. La fuente de la interfaz, Inter,
incluye todos los caracteres del español; no hacen falta fuentes adicionales.

## Terminología

Se usan infinitivos o formas impersonales en las instrucciones («Hacer clic…», «Elegir…»), sin
tratamiento de usted. Algunos términos que conviene mantener coherentes:

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

Las correcciones de hablantes nativos son bienvenidas: abrir un issue o un pull request con el
texto inglés, la traducción actual y la propuesta.

## Mantenimiento y comprobación

Textos fijos: `crates/ui-egui/locales/es.json`. Mensajes con valores:
`crates/ui-egui/locales/es-formats.json`. Las claves en inglés no cambian; el compilador de Rust
comprueba los marcadores al compilar. Las terminaciones de plural del inglés se ocultan con `{:.0}`.

```sh
cargo test -p lightcraft-ui-egui i18n::tests -- --nocapture
LIGHTCRAFT_LANGUAGE=es lightcraft-cli snapshot --demo -o espanol.png --size 1600x1000
```
