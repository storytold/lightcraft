# Printing

> Names in angle brackets (`<app>`, `<PREFIX>`, `<settings_dir>`, …) are the values set in
> [`brand.toml`](../brand.toml); see the README.

The Print module (`crates/ui-egui/src/print_ui.rs`, back end `crates/print`) lays pages out with
`crates/layout`, writes them as PDF or JPEG files, or sends the PDF to a printer over IPP (CUPS on
Linux and macOS).

## Settings that persist

User templates, the current print settings and template, the chosen printer URI and the CUPS
server are saved in `<settings_dir>/print.json` whenever they change (`printui.*` commands, or edits
in the panels once the pointer is released). A damaged file is logged and ignored: the module starts
from the default template.

## Printer paper sizes

Choosing a printer in the Print Job panel (or `printui.media {uri}`) asks it for its paper sizes
(IPP Get-Printer-Attributes: `media-col-database`, else the PWG names of `media-supported`). They
appear under "Printer paper" in the Page Setup menu; `printui.pageSetup {media}` takes one by name,
or any PWG self-describing name (`iso_a5_148x210mm`).

## CUPS test printer

A real print queue is tested only on request: `crates/print/src/tests.rs`
(`cups_test_printer_receives_a_job`) reads the printer's paper sizes and prints a contact sheet PDF
to the IPP URI in `<PREFIX>_CUPS_TEST_PRINTER`. Without the variable the test passes without doing
anything.

Locally (Debian / Ubuntu):

```sh
sudo apt-get install cups printer-driver-cups-pdf
sudo lpadmin -p PDF -E -v cups-pdf:/ -m lsb/usr/cups-pdf/CUPS-PDF_opt.ppd
<PREFIX>_CUPS_TEST_PRINTER=ipp://localhost:631/printers/PDF cargo test -p dac-print cups_test_printer
ls ~/PDF/   # the printed job
```

Opt-in CI job (not part of `cargo xtask ci`; add it to a workflow that runs on `ubuntu-latest` when
printing changes, e.g. `paths: [crates/print/**, crates/ui-egui/src/print_ui.rs]`):

```yaml
cups:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - run: sudo apt-get update && sudo apt-get install -y cups printer-driver-cups-pdf
    - run: sudo systemctl start cups && sudo lpadmin -p PDF -E -v cups-pdf:/ -m lsb/usr/cups-pdf/CUPS-PDF_opt.ppd
    - run: cargo test -p dac-print cups_test_printer -- --nocapture
      env:
        <PREFIX>_CUPS_TEST_PRINTER: ipp://localhost:631/printers/PDF
    - run: test -n "$(ls ~/PDF/ /var/spool/cups-pdf/*/ 2>/dev/null)"
```

(Replace `<PREFIX>` with `env_prefix` from `brand.toml`.)
