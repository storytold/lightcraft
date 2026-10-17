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

## Sending to a printer

`printui.print {destination: "printer"}` (Print Job ▸ Print to: Printer) renders the pages and hands
them to `lcc_print::send::send_to_printer`, which:

- asks the printer which formats it takes (Get-Printer-Attributes) and sends one PDF, or one JPEG job
  per page when the printer takes JPEG but not PDF; a printer that takes neither is an error;
- asks for `copies` (Print Job ▸ Copies, `settings.job.copies`, 1–999) on the job;
- follows the job with Get-Job-Attributes for up to 20 s: an aborted or canceled job is an error,
  `printui.state` → `last.state` holds the final state (`completed`, or `pending` / `processing` when
  the printer is still busy).

## Printer options (paper type, tray, quality, two-sided)

When a printer is chosen, its attributes are read together with its paper sizes:
`media-type-supported`, `media-source-supported` and `media-col-default` (paper types and trays,
with its defaults), `print-quality-supported` / `-default` (3 Draft, 4 Normal, 5 High) and
`sides-supported` / `-default`. The Print Job panel shows a Paper Type, Paper Tray, Two-Sided and
Print Quality menu for each one the printer offers. Each menu starts at *Printer default*.
`printui.driver {mediaType, mediaSource, printQuality, sides}` sets them; `null` gives one back to
the printer. They are saved with the print settings (`settings.job.driver`).

They go out with the job as `media-col {media-type, media-source}`, `print-quality` and `sides`
(PWG 5100.7). An option the printer says it doesn't offer is left out, with a warning in
`last.warnings`. Keywords with spaces or control characters are never sent.

## Encrypted printing (`ipps://`)

An `ipps://` (or `https://`) printer address prints over TLS (RFC 7472). It uses `lcc-net`, the
same pure-Rust rustls client as the other network features. A certificate is trusted when:

- it chains to the Mozilla roots;
- its SHA-256 fingerprint was trusted for that printer (`host:port`) earlier;
- or *Trust a printer's certificate the first time* is on (`printui.trustOnFirstUse {on: true}`)
  and nothing is trusted for that printer yet. This works like an SSH host key: a changed
  certificate later is still refused.

Printers usually make their own certificates, so the usual first result is a refusal:
`PrintError::UntrustedCertificate`. Its message gives the fingerprint and what to do, and the
Print Job panel shows the fingerprint with a **Trust Certificate** button. Compare it with the
fingerprint on the printer's status or web page, then trust it (`printui.trustCertificate
{fingerprint?, uri?}`; with no fingerprint, the one just refused is used). Trusted fingerprints and
the first-use setting are saved in `print.json` (`trustedCertificates`, `trustOnFirstUse`).
Verification is never turned off: the handshake is always checked against the trusted certificate.

Choosing one of a printer's own paper sizes in Page Setup (`printui.pageSetup {media}`) also widens
the page margins to at least the unprintable margins the printer reports for that paper.

## Colour management, Print Adjustment and 16-bit output

Print Job ▸ Color Management (`settings.job.color`, `lcc_print::color`):

- **Profile**: *Managed by Printer* (the default) sends sRGB and leaves colour to the driver. A
  printer/paper ICC profile (`printui.profile {path}`; `{path: null}` goes back to Managed by
  Printer; `{}` opens a file dialog; the panel also offers the profile chosen for soft proofing)
  converts the photos into the printer's device RGB before they are placed. The PDF then tags the
  photos with that profile (ICCBased) and carries it as the document's OutputIntent; JPEG pages
  embed it. Only RGB (and grey) profiles can be printed with; a CMYK profile is refused with an
  error that says so (it can still be soft-proofed). The conversion is a 33³ table built through
  moxcms (`lcc_codecs::printer`), the same CMS the soft proof uses.
- **Intent**: Perceptual or Relative (colorimetric). With Relative, **Black Point Compensation**
  (on by default) scales the photo's blacks onto the printer's darkest black so shadows compress
  instead of clipping.
- **Print Adjustment**: Brightness and Contrast (−100–100) change the print only, not the photo's
  develop settings or exports: a power curve for brightness and a blend towards an S-curve for
  contrast, both keeping black and white fixed. Applied after print sharpening and before the
  profile.
- **16 Bit Output**: photos are rendered at 16 bits per channel and embedded as 16-bit (Flate)
  images in PDF output and in printer jobs sent as PDF. JPEG holds 8 bits: a 16-bit job to JPEG files
  or a JPEG-only printer prints 8-bit pages and says so in the warnings.

Windows: the app prints over IPP only. A printer named the Windows way (`\\server\printer`, or a
plain printer name) is refused with an error that explains this and asks for the printer's `ipp://`
address, because the Windows print spooler can only be reached through the native print API.

## Virtual IPP printer (every CI run)

`lcc_print::testprinter::TestPrinter` is an IPP Everywhere-style printer on `127.0.0.1` (Get-Printer-
Attributes with a `media-col-database`, Print-Job, Get-Job-Attributes) that keeps what it receives.
The `virtual_*` tests in `crates/print/src/tests.rs` and
`print_ui::tests::prints_to_a_virtual_ipp_printer_with_its_paper_margins_and_copies` print through it
in `cargo xtask ci`: paper sizes and margins, copies, PDF, the JPEG fallback, an aborting printer, and
malformed requests. The virtual printer also offers paper types, trays, qualities and two-sided
modes and records what each job asked for: `tests_ipps.rs` and
`print_ui::tests::printer_options_are_offered_drawn_and_sent` check that they are read, drawn
and sent. In `tests_ipps.rs`, a TLS front (rustls, a test-only certificate) relays to the virtual
printer. That tests `ipps://` refused with the fingerprint, trusted by fingerprint, trusted on
first use, and a changed certificate refused again.

## CUPS test printer

A real CUPS queue is tested by `.github/workflows/print.yml` (on changes to the print code, weekly,
and on demand): it installs CUPS with the cups-pdf backend, runs
`cups_test_printer_receives_a_job` (paper sizes from the queue, a 2×2 contact sheet PDF, the job
followed until CUPS finishes it) and checks that a PDF came out of cups-pdf. The test reads the
printer URI from `<PREFIX>_CUPS_TEST_PRINTER`; without it, it passes without doing anything.

Locally (Debian / Ubuntu):

```sh
sudo apt-get install cups printer-driver-cups-pdf
sudo lpadmin -p PDF -E -v cups-pdf:/ -m lsb/usr/cups-pdf/CUPS-PDF_opt.ppd
<PREFIX>_CUPS_TEST_PRINTER=ipp://localhost:631/printers/PDF cargo test -p lcc-print cups_test_printer
ls ~/PDF/ /var/spool/cups-pdf/*/   # the printed job
```

(Replace `<PREFIX>` with `env_prefix` from `brand.toml`.)
