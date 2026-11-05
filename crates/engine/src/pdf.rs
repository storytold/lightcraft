//! A minimal PDF 1.4 writer for image pages: one JPEG XObject per page, explicit byte lengths and
//! a classic cross-reference table. Used by the contact sheet ([`crate::contact_sheet`]) and the
//! slideshow PDF export ([`crate::slideshow_export`]). It streams into any writer, so a long
//! slideshow is never held in memory; the page tree and catalog are written last, once the
//! number of pages is known.

use std::io::Write;

/// Objects 1 and 2 are the catalog and the page tree; pages take three objects each from 3.
const FIRST_PAGE: usize = 3;

pub(crate) struct Pdf<W: Write> {
    w: W,
    /// Bytes written so far.
    pos: usize,
    /// Each object's offset, by object number (index 0 unused).
    offsets: Vec<usize>,
    pages: Vec<usize>,
}

fn io(e: std::io::Error) -> String {
    format!("PDF: {e}")
}

impl<W: Write> Pdf<W> {
    pub(crate) fn new(w: W) -> Result<Self, String> {
        let mut p = Pdf { w, pos: 0, offsets: vec![0; FIRST_PAGE], pages: Vec::new() };
        p.put(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")?;
        Ok(p)
    }

    /// Bytes written so far.
    pub(crate) fn len(&self) -> usize {
        self.pos
    }

    /// Pages added so far.
    pub(crate) fn pages(&self) -> usize {
        self.pages.len()
    }

    fn put(&mut self, b: &[u8]) -> Result<(), String> {
        self.w.write_all(b).map_err(io)?;
        self.pos = self.pos.saturating_add(b.len());
        Ok(())
    }

    fn object(&mut self, id: usize, body: &[u8]) -> Result<(), String> {
        if self.offsets.len() <= id {
            self.offsets.resize(id + 1, 0);
        }
        if let Some(o) = self.offsets.get_mut(id) {
            *o = self.pos;
        }
        self.put(format!("{id} 0 obj\n").as_bytes())?;
        self.put(body)?;
        self.put(b"\nendobj\n")
    }

    fn stream(&mut self, id: usize, dictionary: &str, bytes: &[u8]) -> Result<(), String> {
        let mut body = format!("<< {dictionary} /Length {} >>\nstream\n", bytes.len()).into_bytes();
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\nendstream");
        self.object(id, &body)
    }

    /// A page of `points` (width, height) filled by `jpg` (`width × height` pixels, RGB).
    /// `extra` goes into the page dictionary (e.g. a display duration and transition).
    pub(crate) fn image_page(&mut self, jpg: &[u8], width: usize, height: usize, points: (f64, f64), extra: &str) -> Result<(), String> {
        let id = FIRST_PAGE + self.pages.len() * 3;
        let (w, h) = points;
        self.object(
            id,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w:.2} {h:.2}] /Resources << /XObject << /Im {} 0 R >> >> /Contents {} 0 R{extra} >>",
                id + 2,
                id + 1
            )
            .as_bytes(),
        )?;
        self.stream(id + 1, "", format!("q {w:.2} 0 0 {h:.2} 0 0 cm /Im Do Q").as_bytes())?;
        self.stream(
            id + 2,
            &format!("/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode"),
            jpg,
        )?;
        self.pages.push(id);
        Ok(())
    }

    /// Write the page tree, the catalog (with `catalog_extra` in its dictionary, e.g. a page
    /// mode), the cross-reference table and the trailer.
    pub(crate) fn finish(mut self, catalog_extra: &str) -> Result<W, String> {
        if self.pages.is_empty() {
            return Err("PDF: no pages".into());
        }
        let kids = self.pages.iter().map(|i| format!("{i} 0 R")).collect::<Vec<_>>().join(" ");
        self.object(2, format!("<< /Type /Pages /Count {} /Kids [{kids}] >>", self.pages.len()).as_bytes())?;
        self.object(1, format!("<< /Type /Catalog /Pages 2 0 R{catalog_extra} >>").as_bytes())?;
        let start = self.pos;
        let mut xref = format!("xref\n0 {}\n0000000000 65535 f \n", self.offsets.len()).into_bytes();
        for offset in self.offsets.iter().skip(1) {
            xref.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        xref.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n", self.offsets.len()).as_bytes());
        self.put(&xref)?;
        self.w.flush().map_err(io)?;
        Ok(self.w)
    }
}
