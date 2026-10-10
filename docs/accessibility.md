# Accessibility

The app's UI is drawn by egui, which publishes an [AccessKit](https://accesskit.dev) tree: VoiceOver
(macOS), Narrator (Windows) and Orca (AT-SPI, Linux) read it. The desktop binary enables it through
eframe's `accesskit` feature (`apps/app/Cargo.toml`); the web build does not (egui's web backend has
no screen-reader bridge).

## What a screen reader gets

- egui's own widgets (buttons, checkboxes, radio buttons, combo boxes, sliders, text fields) name
  themselves from their text and report their value and state.
- The app's custom widgets name themselves too: develop sliders (control name and value), icon
  buttons (their tooltip), section headers, grid thumbnails (file name, rating, flag, label) — see
  `crates/ui-egui/src/widgets.rs`.
- Widgets painted by hand (`ui.interact`, `allocate_*`) and text fields that only show a hint get
  their name from the helpers in `crates/ui-egui/src/access.rs` (`button`, `choice`, `named`,
  `label`), which translate it with the UI language. This covers the module picker, the identity
  plate, the Map (canvas, pins, zoom buttons, search and location fields), Book (templates, page
  cells, its filmstrip, text fields), Slideshow, Print and Web side panels (text, password and
  colour fields), the Metadata panel (fields, rating stars, colour labels) and the Navigator.
- Keyboard focus moves through each module's side panels with Tab / Shift+Tab in layout order
  (egui's focus order is the order widgets are drawn: top to bottom, left to right).

## The test

`crates/ui-egui/src/tests_access.rs` runs the demo library headless with AccessKit on, visits every
module, and fails on an interactive node (a button, field, slider, combo box, colour well or
anything clickable) without a label, value, description or `labelled_by` relation. It covers the
fork's own UI: the Map, Book, Slideshow, Print and Web modules, and in Library the module picker,
Navigator and Metadata panels. Widgets shared with upstream are left out (below).

## Upstream candidates

Shared upstream widgets the test leaves out because they have no accessible name yet. Each is a
small fix (a `widget_info` / `access::label` call) best made upstream:

- egui's scroll bars (`Role::ScrollBar` without a label): egui itself.
- The filmstrip cells (`crates/ui-egui/src/panels/detail.rs`, `film` ids): name each cell after its
  photo's file name, as the grid thumbnails do.
- The top bar's search field (`panels/topbar.rs`, `field:search`).
- Keywording (`panels/keywording.rs`): the keyword entry and keyword painter fields.
- Keyword List (`panels/keyword_list.rs`): the filter field, each keyword row's checkbox and its
  "show photos" arrow.
- Quick Develop (`panels/quick_develop.rs`): the stepper buttons (`qd-…-0..3`) are painted
  arrows without names.
- The text field widget (`crates/ui-egui/src/text_field.rs`): takes no accessible label; callers
  name it with `access::label(&response.response, …)`.

The rating stars (`widgets::stars`) were named in this fork with a one-line call (each star reads
"Rating N"); that line is an upstream candidate as well.
