//! Cross-platform clipboard adapter built on `arboard` (`CLAUDE.md` §2.9, §3.3
//! Clipboard). Named `clipboard` (not `macos_clipboard`) because `arboard`
//! works across macOS, Linux, and Windows — a Linux/Windows contributor should
//! not be misled by the filename.
//!
//! Implements `crate::ports::Clipboard` for the local machine. The clipboard
//! handle is constructed fresh inside each call rather than held for the life of
//! the adapter: `arboard::Clipboard` owns OS resources (and a lock on some
//! platforms), and re-acquiring it per operation is cheap and avoids Send/Sync
//! headaches when the trait object crosses threads. `ArboardClipboard` itself is
//! a zero-sized marker — it only proves at construction time that a handle can
//! be obtained.
//!
//! Rich-text HTML writes go through `arboard::Clipboard::set_html`, which
//! places the HTML flavor alongside a plain-text companion so pasting into
//! both rich- and plain-text targets works. RTF is not supported; a native
//! pasteboard adapter (`NSPasteboard` via `objc2` on macOS) remains the planned
//! path if RTF is ever required.

use std::io::Cursor;

use arboard;
use image::ImageBuffer;

use crate::domain::attachment::ImageData;
use crate::domain::errors::ClipboardError;
use crate::ports::Clipboard;

/// Concrete `Clipboard` impl backed by `arboard`.
///
/// See the module docs for why the underlying handle is reconstructed per call.
pub struct ArboardClipboard;

impl ArboardClipboard {
    /// Construct the adapter. Validation is deferred to first use so that
    /// commands which never touch the clipboard (e.g. `onote backup`) don't fail
    /// on a headless session where no pasteboard is available.
    pub fn new() -> Result<Self, ClipboardError> {
        Ok(ArboardClipboard)
    }

    /// Acquire a fresh `arboard` clipboard handle.
    fn handle() -> Result<arboard::Clipboard, ClipboardError> {
        arboard::Clipboard::new().map_err(|e| ClipboardError::Unavailable(e.to_string()))
    }
}

impl Clipboard for ArboardClipboard {
    fn read_text(&self) -> Result<Option<String>, ClipboardError> {
        let mut cb = Self::handle()?;
        match cb.get_text() {
            Ok(s) if s.is_empty() => Ok(None),
            Ok(s) => Ok(Some(s)),
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("not available")
                    || msg.contains("no content")
                    || msg.contains("empty")
                {
                    Ok(None)
                } else {
                    Err(ClipboardError::Unavailable(e.to_string()))
                }
            }
        }
    }

    fn read_image(&self) -> Result<Option<ImageData>, ClipboardError> {
        let mut cb = Self::handle()?;
        match cb.get_image() {
            Ok(img) => {
                let width = img.width as u32;
                let height = img.height as u32;
                let rgba: Vec<u8> = img.bytes.into_owned();
                let buffer: image::RgbaImage = ImageBuffer::from_raw(width, height, rgba)
                    .ok_or_else(|| {
                        ClipboardError::Unavailable(format!(
                            "rgba buffer did not match {width}x{height}"
                        ))
                    })?;
                let mut cursor = Cursor::new(Vec::new());
                buffer
                    .write_to(&mut cursor, image::ImageFormat::Png)
                    .map_err(|e| ClipboardError::Unavailable(e.to_string()))?;
                Ok(Some(ImageData {
                    bytes: cursor.into_inner(),
                    mime: "image/png".to_string(),
                    width,
                    height,
                }))
            }
            // Many clipboards report an error rather than "empty" when no image
            // is present; treat those as "no image" so callers can fall through
            // to text reads.
            Err(_) => Ok(None),
        }
    }

    fn write_text(&self, text: &str) -> Result<(), ClipboardError> {
        let mut cb = Self::handle()?;
        cb.set_text(text.to_string())
            .map_err(|e| ClipboardError::Unavailable(e.to_string()))
    }

    fn write_html(&self, html: &str, plain_text: &str) -> Result<(), ClipboardError> {
        let mut cb = Self::handle()?;
        // `arboard::Clipboard::set_html` (arboard-3.6.1 src/lib.rs:110) writes
        // the HTML flavor and a plain-text companion atomically, so a paste
        // into a plain-text target still yields readable text. Pass the
        // fallback as the `alt_text` companion.
        cb.set_html(html.to_string(), Some(plain_text.to_string()))
            .map_err(|e| ClipboardError::Unavailable(e.to_string()))
    }

    fn write_image(&self, image: &ImageData) -> Result<(), ClipboardError> {
        let mut cb = Self::handle()?;
        // Our `ImageData` carries encoded bytes (PNG/JPEG/...), but `arboard`
        // wants raw RGBA. Decode, then hand over the raw frame.
        let decoded = image::load_from_memory(&image.bytes)
            .map_err(|e| ClipboardError::Unavailable(e.to_string()))?;
        let rgba = decoded.to_rgba8();
        let (width, height) = rgba.dimensions();
        let aid = arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: rgba.into_raw().into(),
        };
        cb.set_image(aid)
            .map_err(|e| ClipboardError::Unavailable(e.to_string()))
    }
}
