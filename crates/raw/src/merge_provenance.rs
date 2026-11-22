//! Where a pixel-shift merge came from, as an XMP block inside the merged DNG.
//!
//! The block lives in the DNG's own XMP packet (a namespace of its own, [`ns`], prefix `lcps`), so it travels with the
//! file: a copy, a move or a re-import elsewhere keeps it. It says which frames were merged and how, and where the
//! stored frames sat on the camera's sensor, which a later Vignetting Calibration needs (the DNG has no maker note).
//!
//! ```xml
//! <rdf:Description rdf:about="" xmlns:lcps="<homepage>/ns/pixelshift/1.0/"
//!     lcps:Engine="1" lcps:ShotCount="16" lcps:Reference="1" lcps:Scale="2"
//!     lcps:SensorWidth="8280" lcps:SensorHeight="5520"
//!     lcps:WindowX="0" lcps:WindowY="0" lcps:WindowWidth="8280" lcps:WindowHeight="5520">
//!   <lcps:Frames><rdf:Seq>
//!     <rdf:li rdf:parseType="Resource"><lcps:Index>1</lcps:Index><lcps:FileName>…</lcps:FileName>
//!       <lcps:Captured>…</lcps:Captured><lcps:Counter>…</lcps:Counter></rdf:li> …
//!   </rdf:Seq></lcps:Frames>
//! </rdf:Description>
//! ```
//!
//! `Index` is the frame's number in the series (1-based) and `Reference` the number of the frame the merged image
//! belongs to (always 1: the frame whose metadata, colour and lens data the merge carries). `Scale` is merged
//! pixels per sensor pixel. The `Window*` fields are the stored frame's place on the sensor (as
//! [`crate::sensor_window::SensorWindow`]); the merged image covers that window at `Scale` times its resolution.

/// The XMP namespace of the block, under the app's homepage like the other namespaces of its own (`<homepage>/ns/pixelshift/1.0/`).
pub fn ns() -> String {
    format!("{}/ns/pixelshift/1.0/", lcc_brand::HOMEPAGE.trim_end_matches('/'))
}
/// What the merge engine's output is called in the merged file's metadata (`xmp:CreatorTool`), after the app's name.
pub const SOFTWARE_SUFFIX: &str = "Pixel Shift Merge";
/// Version of the merge engine that wrote the file (bumped when its output changes).
pub const ENGINE: u32 = 1;

/// One merged frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceFrame {
    /// Number in the series, 1-based.
    pub index: u16,
    pub file_name: String,
    /// Capture time as the metadata gives it (ISO 8601, local), empty when unknown.
    pub captured: String,
    /// The frame's exposure counter (maker note).
    pub counter: u32,
}

/// The stored frames' place on the sensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub sensor: [usize; 2],
    pub origin: [usize; 2],
    pub size: [usize; 2],
}

/// The provenance block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub engine: u32,
    pub shot_count: u16,
    pub reference: u16,
    pub scale: u16,
    pub window: Option<Window>,
    pub frames: Vec<SourceFrame>,
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&")
}

impl Provenance {
    /// The `rdf:Description` element, to go inside an XMP packet's `rdf:RDF`.
    pub fn to_description(&self) -> String {
        let ns = ns();
        let mut s = format!(
            "<rdf:Description rdf:about=\"\" xmlns:lcps=\"{ns}\" lcps:Engine=\"{}\" lcps:ShotCount=\"{}\" lcps:Reference=\"{}\" lcps:Scale=\"{}\"",
            self.engine, self.shot_count, self.reference, self.scale
        );
        if let Some(w) = &self.window {
            s += &format!(
                " lcps:SensorWidth=\"{}\" lcps:SensorHeight=\"{}\" lcps:WindowX=\"{}\" lcps:WindowY=\"{}\" lcps:WindowWidth=\"{}\" lcps:WindowHeight=\"{}\"",
                w.sensor[0], w.sensor[1], w.origin[0], w.origin[1], w.size[0], w.size[1]
            );
        }
        s += "><lcps:Frames><rdf:Seq>";
        for f in &self.frames {
            s += &format!(
                "<rdf:li rdf:parseType=\"Resource\"><lcps:Index>{}</lcps:Index><lcps:FileName>{}</lcps:FileName><lcps:Captured>{}</lcps:Captured><lcps:Counter>{}</lcps:Counter></rdf:li>",
                f.index,
                escape(&f.file_name),
                escape(&f.captured),
                f.counter
            );
        }
        s + "</rdf:Seq></lcps:Frames></rdf:Description>"
    }

    /// `packet` (an XMP packet) with this block added inside its `rdf:RDF`; the packet unchanged when it has none.
    pub fn add_to_packet(&self, packet: &str) -> String {
        match packet.rfind("</rdf:RDF>") {
            Some(at) => format!("{}{}{}", &packet[..at], self.to_description(), &packet[at..]),
            None => packet.to_string(),
        }
    }

    /// Read the block from an XMP packet; `None` when the packet has none or it is unusable.
    pub fn from_packet(packet: &str) -> Option<Provenance> {
        let at = packet.find("xmlns:lcps=")?;
        let start = packet[..at].rfind("<rdf:Description")?;
        let end = start + packet[start..].find("</rdf:Description>")?;
        let d = &packet[start..end];
        let head_end = d.find('>')?;
        let head = &d[..head_end];
        let attr = |name: &str| -> Option<String> {
            let key = format!("lcps:{name}=\"");
            let i = head.find(&key)? + key.len();
            Some(unescape(&head[i..i + head[i..].find('"')?]))
        };
        let num = |name: &str| attr(name)?.parse::<usize>().ok();
        let window = (|| {
            Some(Window {
                sensor: [num("SensorWidth")?, num("SensorHeight")?],
                origin: [num("WindowX")?, num("WindowY")?],
                size: [num("WindowWidth")?, num("WindowHeight")?],
            })
        })();
        let mut frames = Vec::new();
        let mut rest = &d[head_end..];
        while let Some(i) = rest.find("<rdf:li") {
            let j = i + rest[i..].find("</rdf:li>")?;
            let item = &rest[i..j];
            let tag = |name: &str| -> Option<String> {
                let open = format!("<lcps:{name}>");
                let a = item.find(&open)? + open.len();
                let b = a + item[a..].find(&format!("</lcps:{name}>"))?;
                Some(unescape(&item[a..b]))
            };
            frames.push(SourceFrame {
                index: tag("Index")?.parse().ok()?,
                file_name: tag("FileName")?,
                captured: tag("Captured").unwrap_or_default(),
                counter: tag("Counter")?.parse().ok()?,
            });
            rest = &rest[j..];
        }
        Some(Provenance {
            engine: num("Engine")? as u32,
            shot_count: u16::try_from(num("ShotCount")?).ok()?,
            reference: u16::try_from(num("Reference")?).ok()?,
            scale: u16::try_from(num("Scale")?).ok()?,
            window,
            frames,
        })
    }
}

/// Whether an XMP packet carries the block (cheap: no parsing).
pub fn is_marked(packet: &[u8]) -> bool {
    let ns = ns();
    let needle = ns.as_bytes();
    packet.windows(needle.len()).any(|w| w == needle)
}

/// The block of a merged DNG (`bytes` is the whole file, or at least its headers and XMP).
pub fn from_dng(bytes: &[u8]) -> Option<Provenance> {
    let tiff = lcc_tiff::Tiff::parse(bytes).ok()?;
    let packet = tiff.ifds.first()?.bytes(lcc_tiff::tags::XMP)?;
    Provenance::from_packet(&String::from_utf8_lossy(packet))
}

/// The `xmp:CreatorTool` a merged file gets and is recognised by.
pub fn software() -> String {
    format!("{} {SOFTWARE_SUFFIX}", lcc_brand::DISPLAY_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Provenance {
        Provenance {
            engine: ENGINE,
            shot_count: 4,
            reference: 1,
            scale: 2,
            window: Some(Window { sensor: [800, 600], origin: [100, 0], size: [600, 600] }),
            frames: (1..=4)
                .map(|i| SourceFrame {
                    index: i,
                    file_name: format!("test <{i}> & \"x\".nef"),
                    captured: format!("2030-01-02T03:04:0{i}"),
                    counter: 900 + u32::from(i),
                })
                .collect(),
        }
    }

    #[test]
    fn the_block_round_trips_through_a_packet() {
        let packet = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";
        let p = sample();
        let with = p.add_to_packet(packet);
        assert!(is_marked(with.as_bytes()) && !is_marked(packet.as_bytes()));
        assert_eq!(Provenance::from_packet(&with), Some(p.clone()));
        // no window, no frames
        let bare = Provenance { window: None, frames: vec![], ..p };
        assert_eq!(Provenance::from_packet(&bare.add_to_packet(packet)), Some(bare));
        assert_eq!(Provenance::from_packet(packet), None);
        assert_eq!(sample().add_to_packet("no rdf here"), "no rdf here");
    }
}
