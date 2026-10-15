# Printing to a device

File ▸ Print… composes pages as before (layout, cells, colour management, resolution; see the Print module).
The **Print To** section at the top of the settings column chooses where they go:

| Destination | What happens |
|---|---|
| **PDF file** | unchanged: Print to PDF… asks for a file |
| **Printer** | a printer of this computer (craft-print's spooler: CUPS, Windows); the print-ready PDF is submitted |
| **Network printer** | WS-Print or IPP at an address you type (`http://192.168.1.50:5357/service`, `ipp://192.168.1.50:631/ipp/print`) |

## Network printer

- Only numeric private, link-local or loopback addresses are accepted (craft-wsd's URL policy); no host names, no TLS.
- **Query printer** reads what the printer is and lists (reads only, no job). Each option shows one of three states:
  supported (a list or range the printer published), unsupported (disabled, with the reason) or not confirmed
  (enabled; the value is sent as a hint).
- Nothing is sent until the box "I confirm sending this document to this printer" is ticked; the tick is spent by
  each print.
- **Document**: the Print module's PDF is sent as it is when the printer lists PDF. A printer that lists PWG raster
  (or, for a one-page print, JPEG) gets the same pages as pixels, converted by `lightcraft_print::sheets::SheetConverter`
  at the resolution you choose or the listed one nearest 300 dpi. Colour or black and white follows the Color option.
- **Quality** (Draft, Normal, High, **Photo**), two-sided, copies, resolution and paper (A3 A4 A5 Letter Legal Tabloid, 4x6,
  5x7, 8x10, 13x19 by their PWG media names) are mapped to craft-print's job options.
- **Shrink to the printer's printable area** maps to craft-print's `Scaling::ShrinkToFit`; off is `Scaling::Exact`
  (the page keeps the size it has in the preview).
- One press of Print is one tracked `PrintJob`: it is sent once, followed to its end and retried only where that cannot
  print twice. A send whose outcome is unknown says so and offers Reconcile (reads the printer's job lists) instead of
  sending again. Every status line names the printer by the name it reported, never by its address.
- `LIGHTCRAFT_TEST_EVENTS_BIND` (legacy `PDFCRAFT_TEST_EVENTS_BIND`), for example `127.0.0.1:0`, turns on the opt-in
  WS-Eventing push for WS-Print.

## Code

- `crates/print` (`lightcraft-print`, L3): `spool` (the system spooler), `sheets` (`Sheet`, `SheetConverter`), and with the
  `network-print` feature (default, not wasm32) `network` (`Station`: one printer behind a craft-print registry with both
  backends, the deny list, the events opt-in).
- `crates/ui-egui/src/network_glue.rs` and `network_glue/{caps,tracked}.rs`: the host side of `craft_ui::printing::network`.
- `crates/ui-egui/src/print_dest.rs`: the destination choice, composing the pages on a worker, delivering them.

German strings for this feature are machine-drafted (see `docs/localization-de.md`); other languages show English.
