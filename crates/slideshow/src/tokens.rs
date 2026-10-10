//! Text tokens for slide overlays: `{Filename}`, `{Title}`, `{Caption}`, `{Rating}`, `{Date}`,
//! `{Camera}`, `{Lens}`, `{ISO}`, `{Exposure}`, `{Aperture}`, `{Focal}`, `{Creator}`, `{Copyright}`,
//! `{Location}`, `{Sequence}`, `{Total}`. Unknown tokens stay as written so a typo is visible.

use dac_catalog::Photo;

/// The per-photo values the tokens read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlideInfo {
    pub filename: String,
    pub title: String,
    pub caption: String,
    pub rating: u8,
    pub date: String,
    pub camera: String,
    pub lens: String,
    pub iso: String,
    pub exposure: String,
    pub aperture: String,
    pub focal: String,
    pub creator: String,
    pub copyright: String,
    pub location: String,
    /// 1-based position in the show.
    pub sequence: usize,
    pub total: usize,
}

impl SlideInfo {
    pub fn from_photo(p: &Photo, sequence: usize, total: usize) -> SlideInfo {
        let m = &p.meta;
        let location = [m.location.as_str(), m.city.as_str(), m.state.as_str(), m.country.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        SlideInfo {
            filename: p.file_name.clone(),
            title: m.title.clone(),
            caption: m.caption.clone(),
            rating: p.rating.min(5),
            date: p.captured.as_deref().map(|d| d.split('T').next().unwrap_or(d).to_string()).unwrap_or_default(),
            camera: m.camera.clone(),
            lens: m.lens.clone(),
            iso: m.iso.map(|i| i.to_string()).unwrap_or_default(),
            exposure: m.shutter.clone(),
            aperture: m.aperture.filter(|a| a.is_finite()).map(|a| format!("f/{a:.1}")).unwrap_or_default(),
            focal: m.focal_mm.filter(|f| f.is_finite()).map(|f| format!("{f:.0} mm")).unwrap_or_default(),
            creator: m.creator.clone(),
            copyright: m.copyright.clone(),
            location,
            sequence,
            total,
        }
    }

    fn value(&self, token: &str) -> Option<String> {
        Some(match token.to_ascii_lowercase().as_str() {
            "filename" => self.filename.clone(),
            "title" => self.title.clone(),
            "caption" => self.caption.clone(),
            "rating" => "★".repeat(usize::from(self.rating.min(5))),
            "date" => self.date.clone(),
            "camera" => self.camera.clone(),
            "lens" => self.lens.clone(),
            "iso" => self.iso.clone(),
            "exposure" => self.exposure.clone(),
            "aperture" => self.aperture.clone(),
            "focal" => self.focal.clone(),
            "creator" => self.creator.clone(),
            "copyright" => self.copyright.clone(),
            "location" => self.location.clone(),
            "sequence" => self.sequence.to_string(),
            "total" => self.total.to_string(),
            _ => return None,
        })
    }
}

/// The token names, for the UI's token menu.
pub const TOKENS: &[&str] = &[
    "Filename",
    "Title",
    "Caption",
    "Rating",
    "Date",
    "Camera",
    "Lens",
    "ISO",
    "Exposure",
    "Aperture",
    "Focal",
    "Creator",
    "Copyright",
    "Location",
    "Sequence",
    "Total",
];

/// Fill the `{Token}`s of `template` from `info`. Separators around empty values are left as written.
pub fn expand(template: &str, info: &SlideInfo) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(rest.get(..open).unwrap_or_default());
        let after = rest.get(open + 1..).unwrap_or_default();
        match after.find('}') {
            Some(close) => {
                let name = after.get(..close).unwrap_or_default();
                match info.value(name) {
                    Some(v) => out.push_str(&v),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = after.get(close + 1..).unwrap_or_default();
            }
            None => {
                out.push_str(rest.get(open..).unwrap_or_default());
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_known_tokens_and_keeps_unknown() {
        let info = SlideInfo { filename: "a.jpg".into(), rating: 3, sequence: 2, total: 9, ..Default::default() };
        assert_eq!(expand("{Filename} {rating} {Sequence}/{Total} {Nope}", &info), "a.jpg ★★★ 2/9 {Nope}");
        assert_eq!(expand("open { brace", &info), "open { brace");
        assert_eq!(expand("ünï{Filename}çødé", &info), "ünïa.jpgçødé");
        assert_eq!(expand("{", &info), "{");
    }
}
