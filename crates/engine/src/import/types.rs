use super::{Deserialize, MediaKind, Photo, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImportMode {
    /// Reference the files where they are.
    #[default]
    Add,
    /// Copy the files into the library's `Originals/` folder.
    Copy,
    /// Move the files there: as Copy, then each original (and its sidecars) is removed from the
    /// source once the copy is verified and catalogued.
    Move,
}

/// What an import does with a file the library already has in Recently Deleted.
///
/// Someone who imports such a file wants the photo in the library again, so the default brings
/// it back (issues #298, #706: before, the import skipped it as "already in the library" while
/// the grid showed it deleted, and the user was stuck). Restoring loses nothing — the edits,
/// rating and albums come back with the photo and Reset Edits gives a clean start — where
/// importing afresh would silently drop them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnDeleted {
    /// Leave it there; the report says so ([`Duplicate::existing_deleted`]).
    Skip,
    /// Bring the trashed photo back, with its edits ([`ImportReport::restored`]); a photo whose
    /// file is no longer where the library had it is relinked to the imported file.
    #[default]
    Restore,
    /// Delete the trashed record permanently and import the file as a new photo.
    Fresh,
}

impl OnDeleted {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "skip" => Some(Self::Skip),
            "restore" => Some(Self::Restore),
            "fresh" => Some(Self::Fresh),
            _ => None,
        }
    }
}

/// What an import does besides adding the photos.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// A file that is in Recently Deleted: restore (default), skip, or import afresh.
    pub on_deleted: OnDeleted,
    pub mode: ImportMode,
    /// Applied to every imported photo (one History entry).
    pub preset: Option<lightcraft_develop::Preset>,
    /// Added to every imported photo.
    pub keywords: Vec<String>,
    /// Browsing a folder: photos come in as `local` (not in the library), and a file with the
    /// same content as one already known is still listed.
    pub local: bool,
    /// Copy: where the copies go (default: the library's `Originals/`).
    pub destination: Option<String>,
    /// Copy: the folders inside the destination.
    pub organize: Organize,
    /// Copy: a file-name template for the copies ([`crate::rename::expand`] tokens: `{name}`,
    /// `{seq:N}`, `{date:%Y%m%d}`, `{camera}`…), numbered from `rename_start`.
    pub rename: Option<String>,
    pub rename_start: usize,
    /// A metadata preset (by name) applied to every imported photo.
    pub metadata_preset: Option<String>,
    /// Copy: raws are copied as DNG (Copy as DNG).
    pub convert_dng: bool,
    /// What is read from the photos themselves; a part that is `None` goes by the library's
    /// own choice ([`ImportDefaults::read`]).
    pub read: ReadChoice,
}

/// What of a photo's own an import reads into the library: the things a file, or the sidecar
/// beside it, says about the photo that a person (or another program) put there. Everything by
/// default. What the camera recorded (which camera and lens, the exposure, when it was taken,
/// which way up) is always read: the library can't work without it.
///
/// A kind that is not read is left where it is: nothing is taken out of the file or sidecar, at
/// import or by a sidecar written later (the photo remembers it: `Photo::left_in_file`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReadFromPhotos {
    /// Keywords, and the named regions of faces that go with them.
    pub keywords: bool,
    /// Star ratings, pick / reject flags and colour labels.
    pub ratings: bool,
    /// Title, caption, creator, copyright and its terms, and the accessibility texts.
    pub descriptions: bool,
    /// Where it was taken: GPS coordinates, and the place by name (location, city, state, country).
    pub location: bool,
    /// Develop settings a sidecar carries (LightCraft's own, or another editor's `crs:` ones).
    pub develop: bool,
}

impl Default for ReadFromPhotos {
    fn default() -> Self {
        ReadFromPhotos::ALL
    }
}

impl ReadFromPhotos {
    pub const ALL: ReadFromPhotos = ReadFromPhotos { keywords: true, ratings: true, descriptions: true, location: true, develop: true };
    /// The kinds, by the names commands know them under.
    pub const KINDS: [&'static str; 5] = ["keywords", "ratings", "descriptions", "location", "develop"];

    pub fn is_all(&self) -> bool {
        *self == ReadFromPhotos::ALL
    }

    /// What reading this much leaves in the file.
    pub fn left(&self) -> lightcraft_catalog::LeftInFile {
        lightcraft_catalog::LeftInFile {
            keywords: !self.keywords,
            ratings: !self.ratings,
            descriptions: !self.descriptions,
            location: !self.location,
            develop: !self.develop,
        }
    }

    /// Everything but what was left in a photo's file: what a later look at its sidecar reads.
    pub fn but(left: lightcraft_catalog::LeftInFile) -> ReadFromPhotos {
        ReadFromPhotos {
            keywords: !left.keywords,
            ratings: !left.ratings,
            descriptions: !left.descriptions,
            location: !left.location,
            develop: !left.develop,
        }
    }

    /// What a file said about the photo, less the kinds that are not read.
    pub fn of_the_file(&self, m: &mut lightcraft_catalog::Meta) {
        if !self.keywords {
            m.keywords.clear();
            m.regions.clear();
        }
        if !self.descriptions {
            for text in [
                &mut m.title,
                &mut m.caption,
                &mut m.alt_text,
                &mut m.extended_description,
                &mut m.copyright,
                &mut m.usage_terms,
                &mut m.copyright_url,
                &mut m.creator,
            ] {
                text.clear();
            }
            m.copyright_status = Default::default();
        }
        if !self.location {
            m.gps = None;
            for place in [&mut m.location, &mut m.city, &mut m.state, &mut m.country] {
                place.clear();
            }
        }
    }

    /// What a sidecar said about the photo, less the kinds that are not read.
    pub(crate) fn of_the_sidecar(&self, mut sc: crate::sidecar::SidecarData) -> crate::sidecar::SidecarData {
        if !self.keywords {
            (sc.keywords, sc.regions) = (None, None);
        }
        if !self.ratings {
            (sc.rating, sc.flag, sc.label, sc.label_text) = (None, None, None, None);
        }
        if !self.descriptions {
            (sc.title, sc.caption, sc.alt_text, sc.extended_description) = (None, None, None, None);
            (sc.copyright, sc.copyright_marked, sc.usage_terms, sc.copyright_url, sc.creator) = (None, None, None, None, None);
        }
        if !self.location {
            (sc.location, sc.city, sc.state, sc.country) = (None, None, None, None);
        }
        if !self.develop {
            sc.develop = None;
            // (nothing of its develop settings is carried over: there is nothing to warn of)
            sc.warnings.clear();
        }
        sc
    }

    /// Whether a sidecar says something of a kind that is not read: what an import leaves in it, and so what a Copy
    /// has to bring along with the photo ([`Self::of_the_sidecar`] leaves it out of the library).
    pub(crate) fn skipped_in(&self, sc: &crate::sidecar::SidecarData) -> bool {
        // (of the same sidecar: what it takes out is what is skipped; its warnings go with the develop settings)
        let mut read = self.of_the_sidecar(sc.clone());
        read.warnings.clone_from(&sc.warnings);
        read != *sc
    }
}

/// What one import says about what it reads: each kind, or `None` for the library's choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadChoice {
    pub keywords: Option<bool>,
    pub ratings: Option<bool>,
    pub descriptions: Option<bool>,
    pub location: Option<bool>,
    pub develop: Option<bool>,
}

impl ReadChoice {
    /// Everything, whatever the library says: for an import that brings its own account of the
    /// photos and needs the files' to go with it (a Lightroom catalog's).
    pub const ALL: ReadChoice =
        ReadChoice { keywords: Some(true), ratings: Some(true), descriptions: Some(true), location: Some(true), develop: Some(true) };

    /// The choice as a command gives it: an object of the kinds ([`ReadFromPhotos::KINDS`]),
    /// each true or false; the ones left out are not chosen.
    pub fn parse(v: &serde_json::Value) -> std::result::Result<ReadChoice, String> {
        let kinds = ReadFromPhotos::KINDS.join(", ");
        let Some(given) = v.as_object() else {
            return Err(format!("`read` is an object of what to read from the photos ({kinds}), each true or false"));
        };
        let mut choice = ReadChoice::default();
        for (kind, read) in given {
            let Some(read) = read.as_bool() else { return Err(format!("`read.{kind}` is true or false ({kinds})")) };
            match kind.as_str() {
                "keywords" => choice.keywords = Some(read),
                "ratings" => choice.ratings = Some(read),
                "descriptions" => choice.descriptions = Some(read),
                "location" => choice.location = Some(read),
                "develop" => choice.develop = Some(read),
                other => return Err(format!("`read` has no `{other}`: it is one of {kinds}")),
            }
        }
        Ok(choice)
    }

    /// A choice of every kind.
    pub fn of(read: ReadFromPhotos) -> ReadChoice {
        ReadChoice {
            keywords: Some(read.keywords),
            ratings: Some(read.ratings),
            descriptions: Some(read.descriptions),
            location: Some(read.location),
            develop: Some(read.develop),
        }
    }

    /// What is read, the parts not chosen as `otherwise` (the library) has them.
    pub fn over(&self, otherwise: ReadFromPhotos) -> ReadFromPhotos {
        ReadFromPhotos {
            keywords: self.keywords.unwrap_or(otherwise.keywords),
            ratings: self.ratings.unwrap_or(otherwise.ratings),
            descriptions: self.descriptions.unwrap_or(otherwise.descriptions),
            location: self.location.unwrap_or(otherwise.location),
            develop: self.develop.unwrap_or(otherwise.develop),
        }
    }
}

/// How copies are filed in the destination. The date is the capture time, else the time of the
/// import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Organize {
    /// `YYYY/YYYY-MM-DD/` by capture date.
    #[default]
    ByDay,
    /// `YYYY/YYYY-MM/`.
    ByMonth,
    /// All in the destination itself.
    Flat,
    /// A folder template, e.g. `{date:%Y}/{date:%Y%m%d}` → `2026/20260114/`: the template's own
    /// `/` make the levels, each expanded with the file-name tokens ([`crate::rename::expand_folder`];
    /// never outside the destination).
    Template(String),
}

impl Organize {
    /// `date` | `month` | `flat`, or a folder template (anything with a `{`, `/` or `\`).
    pub fn parse(s: &str) -> Option<Organize> {
        match s {
            "date" | "day" | "byDay" => Some(Organize::ByDay),
            "month" | "byMonth" => Some(Organize::ByMonth),
            "flat" | "none" | "intoOneFolder" => Some(Organize::Flat),
            t if t.contains(['{', '/', '\\']) => Some(Organize::Template(t.trim().to_string())),
            _ => None,
        }
    }

    /// The folders (inside the destination) a copy of `p` goes to.
    pub fn folders(&self, p: &Photo) -> Vec<String> {
        let date = p.date();
        let day = date.get(..10).filter(|d| d.len() == 10).unwrap_or("undated");
        let year = day.get(..4).unwrap_or("undated");
        match self {
            Organize::ByDay => vec![year.into(), day.into()],
            Organize::ByMonth => vec![year.into(), day.get(..7).unwrap_or("undated").into()],
            Organize::Flat => Vec::new(),
            Organize::Template(t) => crate::rename::expand_folder(t, p, 1),
        }
    }
}

/// A file found by [`scan`], for the import review.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportCandidate {
    pub path: String,
    pub name: String,
    pub format: String,
    pub kind: MediaKind,
    pub width: u32,
    pub height: u32,
    pub file_size: u64,
    pub captured: Option<String>,
    /// `"path"` (already in the library), `"content"` (same bytes in the library or earlier in this
    /// list): skipped by the import.
    pub duplicate: Option<String>,
    /// The photo that already has it.
    pub existing: Option<u64>,
    /// Not readable.
    pub error: Option<String>,
    /// A raw variant that can't be decoded yet (why): it imports as preview only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_only: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Duplicate {
    pub path: String,
    /// The photo that already has these bytes (or this path).
    pub existing: Option<u64>,
    /// `"path"` (already imported from there) or `"content"` (same bytes elsewhere).
    pub reason: &'static str,
    /// That photo is in Recently Deleted (see [`OnDeleted`]).
    #[serde(rename = "existingDeleted", skip_serializing_if = "std::ops::Not::not")]
    pub existing_deleted: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImportReport {
    pub imported: Vec<u64>,
    pub duplicates: Vec<Duplicate>,
    /// [`OnDeleted::Restore`]: photos brought back from Recently Deleted instead of imported.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restored: Vec<u64>,
    /// Move: the originals that were moved (and removed from the source).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub moved: Vec<Moved>,
    /// Move: originals (or sidecars) left at the source, and why — the photo may still have been
    /// imported (from its copy, or in place).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<Kept>,
    /// (path, error)
    pub failed: Vec<(String, String)>,
    /// Files found after expanding folders.
    pub scanned: usize,
    /// Photos whose metadata/develop settings were read from an XMP sidecar (or embedded XMP).
    pub sidecars: usize,
    /// Per-photo diagnostics from foreign XMP develop settings that could not be fully carried over.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ImportWarning>,
}

/// A structured diagnostic from foreign XMP develop settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportWarningCode {
    /// A named camera profile was found but cannot be applied as Lightroom does.
    CameraProfile,
    /// Lightroom geometry data was found but its saved transform is not retained.
    UprightGeometry,
    /// One or more other XMP develop fields have no LightCraft mapping.
    UnmappedXmp,
}

/// The saved Lightroom Upright mode associated with a foreign geometry transform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportUprightMode {
    Off,
    Auto,
    Level,
    Vertical,
    Full,
    Guided,
    Unknown,
}

/// One actionable XMP import diagnostic. `path` is filled when its photo is committed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportWarning {
    pub path: String,
    pub code: ImportWarningCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ImportUprightMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
}

/// A file moved by an import.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Moved {
    pub from: String,
    pub to: String,
    /// Its sidecars' new paths.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<String>,
}

/// A source a Move left where it was.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Kept {
    pub path: String,
    pub reason: String,
}
