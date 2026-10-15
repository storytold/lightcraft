//! The system print spooler, supplied by craft-print (CUPS and Windows). The PDF is composed by the
//! Print module; these are craft-print's own types, re-exported so the app has one place to look.

pub use craft_print::{Duplex, HAS_PRINTER_PREFERENCES, Job, Printer, Scaling, open_printer_preferences, printers, submit};

/// The printer to preselect: the system default, else the first listed.
pub fn default_printer(printers: &[Printer]) -> Option<&Printer> {
    printers.iter().find(|p| p.default).or(printers.first())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printer(name: &str, default: bool) -> Printer {
        Printer { name: name.to_owned(), default, two_sided: None }
    }

    #[test]
    fn the_default_printer_wins_else_the_first() {
        let list = [printer("A", false), printer("B", true)];
        assert_eq!(default_printer(&list).map(|p| p.name.as_str()), Some("B"));
        let list = [printer("A", false), printer("C", false)];
        assert_eq!(default_printer(&list).map(|p| p.name.as_str()), Some("A"));
        assert!(default_printer(&[]).is_none());
    }
}
