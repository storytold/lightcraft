//! The networks LightCraft can run for the text-reading task: the registry a model set's architecture name is looked up
//! in. A new architecture is one module and one line in [`ARCHITECTURES`]. Bump an architecture's
//! `implementation_version` with any change to what its code computes: stored results will be keyed by it.

/// The task of the text-reading models.
pub const TASK: &str = "text-reader";

/// What the code can do with a model of one `architecture` name.
pub struct Architecture {
    /// The name of the architecture.
    pub id: &'static str,
    /// The task it serves.
    pub task: &'static str,
    /// Changes with what the code computes for this architecture (the detector's boxes, the reader's decoding).
    pub implementation_version: u32,
}

/// Every architecture, by the `architecture` name the model data uses.
pub const ARCHITECTURES: &[Architecture] = &[Architecture { id: "pp-ocrv6", task: TASK, implementation_version: 1 }];

/// The architecture the model data's `architecture` name stands for.
pub fn architecture(id: &str) -> Option<&'static Architecture> {
    ARCHITECTURES.iter().find(|a| a.id == id)
}

/// A loaded text reader: a photo in, the lines of text found in it out. `Reader` is the implementation.
#[cfg(not(target_arch = "wasm32"))]
pub trait TextReader {
    fn read(&self, img: &crate::image::Rgb, options: &crate::reader::Options) -> crate::Result<Vec<crate::reader::Line>>;
}

#[cfg(not(target_arch = "wasm32"))]
impl TextReader for crate::reader::Reader {
    fn read(&self, img: &crate::image::Rgb, options: &crate::reader::Options) -> crate::Result<Vec<crate::reader::Line>> {
        crate::reader::Reader::read(self, img, options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registered_architecture_is_found_by_name() {
        let a = architecture("pp-ocrv6");
        assert!(a.is_some_and(|a| a.task == TASK && a.implementation_version >= 1));
        assert!(architecture("no-such-architecture").is_none());
    }
}
