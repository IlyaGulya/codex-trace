use std::path::Path;

// zstd frame magic: 0xFD2FB528 stored little-endian => bytes [0x28, 0xB5, 0x2F, 0xFD]
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Read a session file that may be plain text or zstd-compressed.
/// Detects the zstd frame magic and decompresses transparently.
pub fn read_session_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.starts_with(&ZSTD_MAGIC) {
        let decompressed = zstd::decode_all(bytes.as_slice()).map_err(|e| format!("zstd: {e}"))?;
        String::from_utf8(decompressed).map_err(|e| format!("zstd utf-8: {e}"))
    } else {
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn compress_zstd(data: &[u8]) -> Vec<u8> {
        zstd::encode_all(data, 3).expect("zstd compress failed")
    }

    #[test]
    fn read_plain_text_unchanged() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("plain.jsonl");
        let content = r#"{"type":"session_meta","payload":{"id":"abc"}}"#;
        std::fs::write(&path, content).unwrap();
        let result = read_session_file(&path).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn read_zstd_compressed_decompresses() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("compressed.jsonl");
        let content = r#"{"type":"session_meta","payload":{"id":"compressed-session"}}"#;
        let compressed = compress_zstd(content.as_bytes());
        std::fs::write(&path, &compressed).unwrap();
        let result = read_session_file(&path).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn read_zstd_multiline_jsonl() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("multi.jsonl");
        let content = [
            r#"{"timestamp":"2026-06-04T00:00:00Z","type":"session_meta","payload":{"id":"zstd-session","timestamp":"2026-06-04T00:00:00Z"}}"#,
            r#"{"timestamp":"2026-06-04T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
            r#"{"timestamp":"2026-06-04T00:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","completed_at":1748995202.0}}"#,
        ]
        .join("\n");
        let compressed = compress_zstd(content.as_bytes());
        std::fs::write(&path, &compressed).unwrap();
        let result = read_session_file(&path).unwrap();
        assert_eq!(result, content);
    }
}
