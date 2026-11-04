//! What a file is, from what's in it (its name only breaks ties), and a
//! plain message for what oxbot can't read.

use std::io::Read as _;
use std::path::Path;

use oxbot_protocol::AttachmentKind;

use super::AttachError;

/// How much of a file is looked at to tell what it is.
const HEAD: usize = 64 * 1024;

/// What a file turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Found {
    /// A picture models read as it is (`ext`: png, jpg, gif, webp).
    Image { ext: &'static str },
    /// A picture models can't read as it is (an iPhone photo, TIFF, BMP):
    /// `sips` makes a JPEG of it.
    Convertible { what: &'static str },
    /// A PDF.
    Pdf,
    /// Plain UTF-8 text.
    Text,
    /// A Word file (`.docx`).
    Word,
    /// A `PowerPoint` deck (`.pptx`).
    Slides,
    /// A spreadsheet calamine reads (`ext`: xlsx, xlsb, xls, ods).
    Sheet { ext: &'static str },
    /// Text `textutil` reads (`ext`: doc, rtf, odt, webarchive).
    Legacy { ext: &'static str },
    /// A video the Mac helper takes frames from (`ext`: mov, mp4, m4v).
    Video { ext: &'static str },
}

impl Found {
    /// The kind the chat shows.
    pub(super) fn kind(self) -> AttachmentKind {
        match self {
            Self::Image { .. } | Self::Convertible { .. } => AttachmentKind::Image,
            Self::Pdf => AttachmentKind::Pdf,
            Self::Text => AttachmentKind::Text,
            Self::Word | Self::Legacy { .. } => AttachmentKind::Document,
            Self::Slides => AttachmentKind::Slides,
            Self::Sheet { .. } => AttachmentKind::Sheet,
            Self::Video { .. } => AttachmentKind::Video,
        }
    }

    /// The extension the saved file should have, from what's in it
    /// (`None`: plain text keeps its own). So the file never gets an
    /// extension that would make the Mac run it when opened.
    pub(super) fn extension(self) -> Option<&'static str> {
        match self {
            Self::Image { ext }
            | Self::Sheet { ext }
            | Self::Legacy { ext }
            | Self::Video { ext } => Some(ext),
            Self::Convertible { what } => Some(match what {
                "HEIC" => "heic",
                "AVIF" => "avif",
                "TIFF" => "tiff",
                _ => "bmp",
            }),
            Self::Text => None,
            Self::Pdf => Some("pdf"),
            Self::Word => Some("docx"),
            Self::Slides => Some("pptx"),
        }
    }

    /// Whether it's a zip file inside.
    pub(super) fn is_zip(self) -> bool {
        matches!(
            self,
            Self::Word
                | Self::Slides
                | Self::Sheet {
                    ext: "xlsx" | "xlsb" | "ods"
                }
                | Self::Legacy { ext: "odt" }
        )
    }
}

/// What the file at `path` (named `name`) is, or why oxbot won't take it.
pub(super) fn sniff(path: &Path, name: &str) -> Result<Found, AttachError> {
    let mut head = Vec::with_capacity(HEAD);
    std::fs::File::open(path)?
        .take(HEAD as u64)
        .read_to_end(&mut head)?;
    let ext = extension(name);
    let ext = ext.as_deref().unwrap_or("");
    if head.is_empty() {
        return Err(AttachError::Unsupported(format!("“{name}” is empty.")));
    }
    if let Some(found) = by_magic(&head, ext) {
        return found.map_err(AttachError::Unsupported);
    }
    if head.starts_with(b"PK\x03\x04") {
        return super::office::sniff_zip(path, name, ext);
    }
    if is_text(&head) && !claims_binary(ext) {
        return Ok(Found::Text);
    }
    Err(AttachError::Unsupported(refusal(&head, name, ext)))
}

/// The file's extension, lower case.
fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// What the first bytes say, when they say it plainly.
fn by_magic(head: &[u8], ext: &str) -> Option<Result<Found, String>> {
    let found = if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Found::Image { ext: "png" }
    } else if head.starts_with(b"\xff\xd8\xff") {
        Found::Image { ext: "jpg" }
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Found::Image { ext: "gif" }
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Found::Image { ext: "webp" }
    } else if head.starts_with(b"II*\0") || head.starts_with(b"MM\0*") {
        Found::Convertible { what: "TIFF" }
    } else if head.starts_with(b"BM") && matches!(ext, "bmp" | "dib") {
        Found::Convertible { what: "BMP" }
    } else if head.windows(5).take(1024).any(|w| w == b"%PDF-") {
        Found::Pdf
    } else if head.starts_with(b"{\\rtf") {
        Found::Legacy { ext: "rtf" }
    } else if head.starts_with(b"bplist00") && ext == "webarchive" {
        Found::Legacy { ext: "webarchive" }
    } else if head.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1") {
        // An old Office file: which one only its name says.
        return Some(match ext {
            "doc" | "dot" => Ok(Found::Legacy { ext: "doc" }),
            "xls" | "xlt" => Ok(Found::Sheet { ext: "xls" }),
            "ppt" | "pps" => Err(
                "oxbot can't read old PowerPoint files (.ppt). Save it as .pptx or PDF and attach that."
                    .to_owned(),
            ),
            _ => Err("oxbot can't tell what this old Office file is. Save it as .docx, .xlsx or PDF and attach that.".to_owned()),
        });
    } else {
        return by_brand(head, ext);
    };
    Some(Ok(found))
}

/// An ISO media file (`ftyp` box): HEIC photos, `QuickTime` and MP4 video,
/// or audio.
fn by_brand(head: &[u8], ext: &str) -> Option<Result<Found, String>> {
    if head.len() < 12 || &head[4..8] != b"ftyp" {
        return None;
    }
    let brand = &head[8..12];
    Some(match brand {
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1" => {
            Ok(Found::Convertible { what: "HEIC" })
        }
        b"avif" | b"avis" => Ok(Found::Convertible { what: "AVIF" }),
        b"M4A " | b"M4B " | b"M4P " | b"F4A " => Err(no_audio()),
        b"qt  " => Ok(Found::Video { ext: "mov" }),
        b"M4V " | b"M4VH" | b"M4VP" => Ok(Found::Video { ext: "m4v" }),
        _ if ext == "m4a" => Err(no_audio()),
        _ => Ok(Found::Video {
            ext: if ext == "m4v" { "m4v" } else { "mp4" },
        }),
    })
}

fn no_audio() -> String {
    "oxbot can't listen to audio files. Use the microphone button to say it, or attach a transcript.".to_owned()
}

/// Whether the start of a file is UTF-8 text (a character cut off at the
/// end of what was read is fine) with no NUL bytes.
fn is_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && head.len() == HEAD,
    }
}

/// Extensions of formats that are never plain text: a file called that
/// which turns out to be text (or anything else) isn't what it says.
fn claims_binary(ext: &str) -> bool {
    matches!(
        ext,
        "pdf"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "heic"
            | "heif"
            | "tif"
            | "tiff"
            | "docx"
            | "xlsx"
            | "pptx"
            | "doc"
            | "xls"
            | "ppt"
            | "odt"
            | "ods"
            | "mov"
            | "mp4"
            | "m4v"
            | "zip"
            | "exe"
            | "dmg"
            | "app"
    )
}

/// What to tell the person about a file oxbot won't take.
fn refusal(head: &[u8], name: &str, ext: &str) -> String {
    let what = infer::get(head).map(|t| (t.matcher_type(), t.mime_type()));
    match what {
        Some((infer::MatcherType::Audio, _)) => no_audio(),
        Some((infer::MatcherType::Video, _)) => format!(
            "oxbot takes still frames from .mov, .mp4 and .m4v videos only. Save “{name}” as one of those and attach it again."
        ),
        Some((infer::MatcherType::Archive, _)) => format!(
            "oxbot doesn't open archives like “{name}”. Unzip it and attach the files inside."
        ),
        Some((infer::MatcherType::App, _)) => {
            format!("“{name}” is a program. oxbot doesn't take programs.")
        }
        Some((infer::MatcherType::Image, _)) => format!(
            "oxbot can't read this kind of picture. Save “{name}” as PNG or JPEG and attach that."
        ),
        _ if claims_binary(ext) => {
            format!("“{name}” isn't what its name says inside, so oxbot won't read it.")
        }
        _ => format!(
            "oxbot can't read “{name}”. It reads pictures, PDFs, text, Word, Excel and PowerPoint files, and short videos."
        ),
    }
}

/// Whether a PDF is locked with a password (no bot can read those).
pub(super) fn pdf_is_locked(path: &Path) -> Result<bool, AttachError> {
    const NEEDLE: &[u8] = b"/Encrypt";
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(false);
        }
        carry.extend_from_slice(&buf[..n]);
        if carry.windows(NEEDLE.len()).any(|w| w == NEEDLE) {
            return Ok(true);
        }
        let keep = carry.len().saturating_sub(NEEDLE.len() - 1);
        carry.drain(..keep);
    }
}
