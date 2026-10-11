//! Media export for the report.
//!
//! The archive stores media content-addressed as `<media_root>/<sha256>` with
//! no extension, so the report has to sniff the real format before writing a
//! file a person can actually open.

use std::collections::BTreeMap;
use std::path::Path;

use archive_domain::MessageType;

use super::{ReportError, ReportMessage};

/// content_hash -> path relative to the report directory, using forward slashes
/// so the value can be embedded in a Markdown link on any platform.
pub type MediaPaths = BTreeMap<String, String>;

pub fn export_media(
    conversation_dir: &Path,
    conversation_component: &str,
    media_root: Option<&Path>,
    messages: &[ReportMessage],
) -> Result<MediaPaths, ReportError> {
    let Some(media_root) = media_root else {
        return Ok(MediaPaths::new());
    };
    if !media_root.is_dir() {
        return Ok(MediaPaths::new());
    }

    let mut paths = MediaPaths::new();
    let mut ordinal = 0usize;
    let mut created_dir = false;

    for message in messages {
        for media in &message.media {
            ordinal += 1;
            let Some(hash) = media.content_hash.as_deref() else {
                continue;
            };
            if !is_safe_hash(hash) {
                continue;
            }
            let source = media_root.join(hash);
            let Ok(bytes) = std::fs::read(&source) else {
                continue;
            };
            if !created_dir {
                std::fs::create_dir_all(conversation_dir)?;
                created_dir = true;
            }
            let extension =
                detect_extension(&bytes, message.message_type.clone(), media.original_name.as_deref());
            let file_name = format!("{ordinal:02}{extension}");
            std::fs::write(conversation_dir.join(&file_name), &bytes)?;
            paths.insert(
                hash.to_owned(),
                format!("media/{conversation_component}/{file_name}"),
            );
        }
    }
    Ok(paths)
}

/// Only accept a hash shaped like the archive's own SHA-256 hex digests. This
/// keeps a crafted `content_hash` from reaching outside the media root.
fn is_safe_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn detect_extension(bytes: &[u8], message_type: MessageType, original_name: Option<&str>) -> String {
    if let Some(sniffed) = sniff_magic(bytes) {
        return sniffed.to_owned();
    }
    if let Some(original) = original_name
        .and_then(|name| std::path::Path::new(name).extension())
        .and_then(|extension| extension.to_str())
        .filter(|extension| is_plain_extension(extension))
    {
        return format!(".{}", original.to_ascii_lowercase());
    }
    match message_type {
        MessageType::Image => ".img".to_owned(),
        MessageType::Audio => ".silk".to_owned(),
        MessageType::Video => ".mp4".to_owned(),
        _ => ".bin".to_owned(),
    }
}

fn sniff_magic(bytes: &[u8]) -> Option<&'static str> {
    const SIGNATURES: &[(&[u8], &str)] = &[
        (&[0x89, 0x50, 0x4E, 0x47], ".png"),
        (&[0xFF, 0xD8, 0xFF], ".jpg"),
        (&[0x47, 0x49, 0x46, 0x38], ".gif"),
        (&[0x42, 0x4D], ".bmp"),
    ];
    SIGNATURES
        .iter()
        .find(|(magic, _)| bytes.starts_with(magic))
        .map(|(_, extension)| *extension)
        .or_else(|| {
            (bytes.len() >= 12
                && &bytes[0..4] == b"RIFF"
                && &bytes[8..12] == b"WEBP")
                .then_some(".webp")
        })
        .or_else(|| bytes.starts_with(b"#!AMR").then_some(".amr"))
        .or_else(|| bytes.starts_with(b"#!SILK").then_some(".silk"))
}

/// Extensions are written straight into a filename, so keep them to a short
/// ASCII alphanumeric run instead of trusting whatever the source claimed.
fn is_plain_extension(extension: &str) -> bool {
    !extension.is_empty()
        && extension.len() <= 10
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use archive_domain::{MediaIntegrity, MediaRefV1};

    fn media(hash: Option<&str>) -> MediaRefV1 {
        MediaRefV1 {
            content_hash: hash.map(str::to_owned),
            original_name: None,
            mime_type: None,
            size_bytes: None,
            source_locator: "unavailable".into(),
            integrity: MediaIntegrity::Missing,
            missing_reason: None,
        }
    }

    fn message(message_type: MessageType, items: Vec<MediaRefV1>) -> ReportMessage {
        ReportMessage {
            sent_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            sender_name: Some("我".into()),
            sender_id: Some("p1".into()),
            direction: archive_domain::MessageDirection::Incoming,
            message_type,
            body_text: None,
            media: items,
        }
    }

    #[test]
    fn rejects_hashes_that_could_escape_the_media_root() {
        assert!(!is_safe_hash("../../secret"));
        assert!(!is_safe_hash("aa"));
        assert!(!is_safe_hash(&"a".repeat(63)));
        assert!(!is_safe_hash(&format!("{}zz", "a".repeat(62))));
        assert!(is_safe_hash(&"a".repeat(64)));
    }

    #[test]
    fn sniffs_real_formats_before_falling_back_to_message_type() {
        let png = [0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A];
        assert_eq!(detect_extension(&png, MessageType::Image, None), ".png");
        assert_eq!(detect_extension(b"#!SILK_V3", MessageType::Audio, None), ".silk");
        assert_eq!(
            detect_extension(b"not a known format", MessageType::Audio, None),
            ".silk"
        );
        assert_eq!(
            detect_extension(b"random", MessageType::File, Some("季度报告.pdf")),
            ".pdf"
        );
        assert_eq!(detect_extension(b"random", MessageType::File, None), ".bin");
    }

    #[test]
    fn suspicious_original_extensions_are_not_reused() {
        assert_eq!(
            detect_extension(b"random", MessageType::File, Some("a.exe.bmp c")),
            ".bin"
        );
    }

    #[test]
    fn copies_stored_media_under_the_conversation_component() {
        let directory = tempfile::tempdir().unwrap();
        let media_root = directory.path().join("media");
        std::fs::create_dir_all(&media_root).unwrap();
        let hash = "c".repeat(64);
        std::fs::write(media_root.join(&hash), [0x89u8, 0x50, 0x4E, 0x47]).unwrap();

        let target = directory.path().join("report").join("media").join("张三");
        let messages = vec![message(MessageType::Image, vec![media(Some(&hash))])];
        let paths = export_media(&target, "张三", Some(&media_root), &messages).unwrap();

        assert_eq!(paths.get(&hash).map(String::as_str), Some("media/张三/01.png"));
        assert!(target.join("01.png").is_file());
    }

    #[test]
    fn missing_media_store_yields_no_paths() {
        let directory = tempfile::tempdir().unwrap();
        let messages = vec![message(MessageType::Image, vec![media(Some(&"d".repeat(64)))])];
        assert!(
            export_media(
                &directory.path().join("out"),
                "x",
                Some(&directory.path().join("absent")),
                &messages
            )
            .unwrap()
            .is_empty()
        );
    }
}