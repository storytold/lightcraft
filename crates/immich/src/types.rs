//! Request and response bodies of the Immich endpoints the app calls, written by hand from the
//! public API documentation (plan/immich.md). Every field the app does not need is left out, and
//! every field is optional or defaulted where the server may omit it, so a newer server that adds
//! or drops a field never breaks parsing.

use serde::{Deserialize, Serialize};

/// `GET /server/version`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ServerVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for ServerVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The oldest server the app talks to (plan/immich.md → Versions).
pub const MIN_VERSION: ServerVersion = ServerVersion { major: 3, minor: 0, patch: 0 };

/// `GET /users/me`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_admin: bool,
}

/// `GET /api-keys/me`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// The part of `exifInfo` the app reads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExifInfo {
    #[serde(default)]
    pub file_size_in_byte: Option<u64>,
    #[serde(default)]
    pub date_time_original: Option<String>,
    #[serde(default)]
    pub rating: Option<i32>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub make: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    #[serde(default)]
    pub id: String,
    /// The full hierarchical value (`parent/child`).
    #[serde(default)]
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonRef {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// `AssetResponseDto` (the fields the app uses).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub id: String,
    /// SHA-1 of the original, base64.
    #[serde(default)]
    pub checksum: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub original_file_name: String,
    #[serde(default)]
    pub original_path: String,
    #[serde(default)]
    pub original_mime_type: Option<String>,
    #[serde(default)]
    pub file_created_at: Option<String>,
    #[serde(default)]
    pub local_date_time: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub is_favorite: bool,
    #[serde(default)]
    pub is_trashed: bool,
    #[serde(default)]
    pub is_offline: bool,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub library_id: Option<String>,
    #[serde(default)]
    pub exif_info: Option<ExifInfo>,
    #[serde(default)]
    pub tags: Vec<Tag>,
    #[serde(default)]
    pub people: Vec<PersonRef>,
}

impl Asset {
    pub fn is_image(&self) -> bool {
        self.kind.is_empty() || self.kind.eq_ignore_ascii_case("IMAGE")
    }
    pub fn file_size(&self) -> Option<u64> {
        self.exif_info.as_ref().and_then(|e| e.file_size_in_byte)
    }
    /// Capture time as the camera wrote it (local, no zone), `YYYY-MM-DDTHH:MM:SS`.
    pub fn local_capture(&self) -> Option<String> {
        self.local_date_time.as_deref().map(|t| t.chars().take(19).collect())
    }
    /// The SHA-1 as lower-case hex, when the checksum is one.
    pub fn sha1_hex(&self) -> Option<String> {
        dac_hash::Sha1Digest::parse(&self.checksum).map(|d| d.to_hex())
    }
}

/// `POST /search/metadata` body (the filters the app uses).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataSearch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub with_exif: Option<bool>,
    /// Also trashed assets (IMM-SYNC sees deletions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub with_deleted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_favorite: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
    /// Text recognised in the image (servers with OCR).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPage {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub items: Vec<Asset>,
    /// The next page number as a string, or `null` at the end.
    #[serde(default)]
    pub next_page: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub assets: AssetPage,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    #[serde(default)]
    pub album_name: String,
    #[serde(default)]
    pub asset_count: u64,
    #[serde(default)]
    pub album_thumbnail_asset_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_hidden: bool,
    #[serde(default)]
    pub birth_date: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct People {
    #[serde(default)]
    pub people: Vec<Person>,
}

/// An external library (`GET /libraries`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub import_paths: Vec<String>,
    #[serde(default)]
    pub exclusion_patterns: Vec<String>,
    #[serde(default)]
    pub asset_count: u64,
}

/// One item of `POST /assets/bulk-upload-check`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadCheck {
    pub id: String,
    /// SHA-1, base64 or hex.
    pub checksum: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadCheckResult {
    pub id: String,
    /// `accept` or `reject`.
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub asset_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UploadCheckResponse {
    #[serde(default)]
    pub results: Vec<UploadCheckResult>,
}

/// `PUT /assets/{id}` body (IMM-SYNC): only the fields that change are sent. `rating: Some(None)`
/// clears the rating (Immich refuses 0); `-1` is "rejected".
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<Option<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_favorite: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_time_original: Option<String>,
    /// `timeline` or `archive` (never `locked` / `hidden`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
}

impl AssetUpdate {
    pub fn is_empty(&self) -> bool {
        *self == AssetUpdate::default()
    }
}

/// A tag as `GET /tags` / `PUT /tags` return it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagInfo {
    pub id: String,
    #[serde(default)]
    pub value: String,
}

/// One face of `GET /faces?id=<assetId>`: a box in pixels of an image `image_width` × `image_height`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Face {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub bounding_box_x1: f64,
    #[serde(default)]
    pub bounding_box_y1: f64,
    #[serde(default)]
    pub bounding_box_x2: f64,
    #[serde(default)]
    pub bounding_box_y2: f64,
    #[serde(default)]
    pub image_width: f64,
    #[serde(default)]
    pub image_height: f64,
    #[serde(default)]
    pub person: Option<Person>,
    #[serde(default)]
    pub source_type: Option<String>,
}

/// `POST /search/smart` body: a natural-language query (CLIP), plus paging.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartSearch {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub kind: Option<String>,
}
