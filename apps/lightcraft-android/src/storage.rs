//! SAF document identity policy, independent of Android's VM for testing.
#![forbid(unsafe_code)]

pub fn validate_uri(uri: &str) -> Result<(), String> {
    let authority = uri.strip_prefix("content://").and_then(|s| s.split('/').next()).unwrap_or_default();
    if authority.is_empty() || uri.chars().any(char::is_control) {
        return Err("A valid Android content URI is required".into());
    }
    Ok(())
}

pub fn export_name(path: &str) -> Result<&str, String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or_default();
    if name.is_empty() || name == "." || name == ".." || name.chars().any(char::is_control) {
        return Err("Invalid export filename".into());
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_content_uri_without_interpreting_document_id_as_path() {
        assert!(validate_uri("content://com.android.externalstorage.documents/tree/1234%3ADCIM/document/1234%3ADCIM%2Fphoto.NEF").is_ok());
        assert!(validate_uri("/sdcard/DCIM/photo.NEF").is_err());
        assert!(validate_uri("file:///private/photo.NEF").is_err());
        assert!(validate_uri("content://").is_err());
    }

    #[test]
    fn destination_name_cannot_escape_or_overwrite_original_path() {
        assert_eq!(export_name("/virtual/export/edited.jpg").unwrap(), "edited.jpg");
        assert!(export_name("../..").is_err());
        assert!(export_name("").is_err());
        assert!(export_name("photo\0.jpg").is_err());
    }
}
