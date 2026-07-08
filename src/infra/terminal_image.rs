//! `ImageRenderer` adapter (`CLAUDE.md` §2.4, §3.3). Reads the raw image bytes
//! and reports dimensions + MIME. The UI layer (ratatui-image) does the actual
//! full decode + terminal rendering; this keeps `ratatui-image` and the `image`
//! decode out of the domain/ports, and avoids a decode→re-encode→decode
//! round-trip (the bytes pass through untouched).

use std::ffi::OsStr;
use std::io::Cursor;
use std::path::Path;

use crate::domain::errors::AttachmentError;
use crate::ports::{ImageRenderer, LoadedImage};

/// Terminal-image adapter. Only reads bytes + probes dimensions/mime; full
/// decode + rendering is the UI's job (`CLAUDE.md` §2.4).
#[derive(Debug, Default, Clone, Copy)]
pub struct TerminalImage;

impl TerminalImage {
    pub fn new() -> Self {
        Self
    }

    /// Best-effort MIME from the file extension; unknown extensions fall back to
    /// `image/png` as a reasonable default for pasted screenshots.
    fn mime_for(path: &Path) -> String {
        let mime = match path
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("png") => "image/png",
            Some("jpg") | Some("jpeg") => "image/jpeg",
            Some("gif") => "image/gif",
            Some("webp") => "image/webp",
            Some("bmp") => "image/bmp",
            _ => "image/png",
        };
        mime.to_owned()
    }
}

impl ImageRenderer for TerminalImage {
    fn load(&self, abs_path: &Path) -> Result<LoadedImage, AttachmentError> {
        if !abs_path.exists() {
            return Err(AttachmentError::NotFound(abs_path.display().to_string()));
        }
        // Pass the raw file bytes through unchanged; the UI decodes once.
        let bytes = std::fs::read(abs_path)
            .map_err(|e| AttachmentError::Decode(format!("read {}: {e}", abs_path.display())))?;
        let size_bytes = bytes.len() as u64;
        let mime = Self::mime_for(abs_path);

        // Header-only dimension probe (no full pixel decode → cheap, and not a
        // decompression-bomb vector since most formats read w/h from headers).
        let (width, height) = match image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|e| AttachmentError::Decode(format!("format guess: {e}")))?
            .into_dimensions()
        {
            Ok(d) => d,
            Err(e) => {
                // Can't probe dims (truncated/corrupt header). Still return the
                // bytes so the caller can attempt a full decode / show fallback.
                tracing::warn!(error = %e, "image dimension probe failed");
                (0, 0)
            }
        };

        Ok(LoadedImage {
            bytes,
            mime,
            width,
            height,
            size_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_png_with_dims_mime_and_size() {
        let img = image::RgbaImage::new(2, 2);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tiny.png");
        img.save(&path).expect("save png");

        let loaded = TerminalImage::new().load(&path).expect("load");
        assert_eq!(loaded.width, 2);
        assert_eq!(loaded.height, 2);
        assert_eq!(loaded.mime, "image/png");
        // Raw bytes pass through: still a PNG.
        assert_eq!(
            &loaded.bytes[0..8],
            &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
        );
        assert!(loaded.size_bytes > 0);
    }

    #[test]
    fn missing_file_returns_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing.png");

        let err = TerminalImage::new().load(&path).unwrap_err();
        assert!(
            matches!(err, AttachmentError::NotFound(_)),
            "expected NotFound, got {err:?}"
        );
    }
}
