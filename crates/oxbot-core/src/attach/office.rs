//! Word, `PowerPoint` and spreadsheets as text, in Rust: `.docx` and `.pptx`
//! read with `zip` and `quick-xml`, spreadsheets with `calamine`.
//!
//! Office files are zip files, and a zip can unpack to far more than it
//! is (a "zip bomb"): every zip is checked first, by unpacking it to
//! nowhere, and refused if it holds too many files or too much.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read, Seek};
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};

use super::AttachError;
use super::sniff::Found;

/// The most a zip may unpack to.
const MAX_UNZIPPED: u64 = 50 * 1024 * 1024;
/// The most files a zip may hold.
const MAX_ENTRIES: usize = 5000;
/// The most rows of one sheet written out.
pub(super) const MAX_SHEET_ROWS: usize = 5000;
/// The most sheets written out.
const MAX_SHEETS: usize = 50;
/// The most text written for one file.
pub(super) const MAX_TEXT_OUT: usize = 2 * 1024 * 1024;

fn unreadable(what: &str) -> AttachError {
    AttachError::Unreadable(format!(
        "oxbot couldn't read this {what}; it may be damaged."
    ))
}

fn too_much() -> AttachError {
    AttachError::TooBig(
        "This file unpacks to far more than its size (over 50 MB), so oxbot won't open it."
            .to_owned(),
    )
}

/// Opens a zip, checking how much it really unpacks to.
pub(super) fn check_zip_file(path: &Path) -> Result<zip::ZipArchive<std::fs::File>, AttachError> {
    check_zip(std::fs::File::open(path)?)
}

fn check_zip<R: Read + Seek>(reader: R) -> Result<zip::ZipArchive<R>, AttachError> {
    let mut zip = zip::ZipArchive::new(reader).map_err(|_| unreadable("zip file"))?;
    if zip.len() > MAX_ENTRIES {
        return Err(too_much());
    }
    let mut left = MAX_UNZIPPED;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|_| unreadable("zip file"))?;
        if entry.encrypted() {
            return Err(AttachError::Unsupported(
                "This file is locked with a password, so oxbot can't read it.".to_owned(),
            ));
        }
        if entry.size() > left {
            return Err(too_much());
        }
        // What it says it holds may be a lie: count what really comes out.
        let n = std::io::copy(&mut (&mut entry).take(left + 1), &mut std::io::sink())
            .map_err(|_| unreadable("zip file"))?;
        if n > left {
            return Err(too_much());
        }
        left -= n;
    }
    Ok(zip)
}

/// One file inside a checked zip, as text (`None` if it isn't there).
fn entry_text<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<String>, AttachError> {
    let Ok(mut entry) = zip.by_name(name) else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    (&mut entry)
        .take(MAX_UNZIPPED)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable("file"))?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// What a zip file is: Word, `PowerPoint`, Excel or `OpenDocument`, or why
/// oxbot won't take it.
pub(super) fn sniff_zip(path: &Path, name: &str, ext: &str) -> Result<Found, AttachError> {
    let archive = || {
        AttachError::Unsupported(format!(
            "oxbot doesn't open archives like `{name}`. Unzip it and attach the files inside."
        ))
    };
    let Ok(zip) = zip::ZipArchive::new(std::fs::File::open(path)?) else {
        return Err(archive());
    };
    let has = |n: &str| zip.index_for_name(n).is_some();
    if has("word/document.xml") {
        return Ok(Found::Word);
    }
    if has("ppt/presentation.xml") {
        return Ok(Found::Slides);
    }
    if has("xl/workbook.xml") {
        return Ok(Found::Sheet { ext: "xlsx" });
    }
    if has("xl/workbook.bin") {
        return Ok(Found::Sheet { ext: "xlsb" });
    }
    if has("Index/Document.iwa") || matches!(ext, "key" | "pages" | "numbers") {
        return Err(AttachError::Unsupported(format!(
            "oxbot can't read Keynote, Pages or Numbers files like `{name}`. Export it as PDF, Word, Excel or PowerPoint and attach that."
        )));
    }
    if has("mimetype") {
        let mut zip = check_zip_file(path)?;
        let kind = entry_text(&mut zip, "mimetype")?.unwrap_or_default();
        match kind.trim() {
            "application/vnd.oasis.opendocument.text" => return Ok(Found::Legacy { ext: "odt" }),
            "application/vnd.oasis.opendocument.spreadsheet" => {
                return Ok(Found::Sheet { ext: "ods" });
            }
            "application/vnd.oasis.opendocument.presentation" => {
                return Err(AttachError::Unsupported(format!(
                    "oxbot can't read OpenDocument slides like `{name}`. Export it as PDF or PowerPoint and attach that."
                )));
            }
            _ => {}
        }
    }
    Err(archive())
}

/// Writes `from` (a Word file, deck or spreadsheet) to `to` as text.
pub(super) fn to_text(kind: Found, from: &Path, to: &Path) -> Result<(), AttachError> {
    let text = match kind {
        Found::Word => {
            let mut zip = check_zip_file(from)?;
            let xml = entry_text(&mut zip, "word/document.xml")?
                .ok_or_else(|| unreadable("Word file"))?;
            docx_text(&xml)?
        }
        Found::Slides => {
            let mut zip = check_zip_file(from)?;
            pptx_text(&mut zip)?
        }
        Found::Sheet { ext } => {
            if ext != "xls" {
                drop(check_zip_file(from)?);
            }
            sheet_text(from, ext)?
        }
        _ => return Err(unreadable("file")),
    };
    std::fs::write(to, cap(text))?;
    Ok(())
}

/// At most [`MAX_TEXT_OUT`] bytes, cut at a character, saying so.
fn cap(mut text: String) -> String {
    if text.len() > MAX_TEXT_OUT {
        let mut end = MAX_TEXT_OUT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n… (oxbot cut the rest: it's too long)\n");
    }
    text
}

/// The text an entity reference stands for.
fn reference(r: &BytesRef<'_>) -> Option<char> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c);
    }
    Some(match &**r {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    })
}

fn local(name: &[u8]) -> &[u8] {
    name.rsplit(|b| *b == b':').next().unwrap_or(name)
}

/// A Word document's text: paragraphs on their own lines, table cells
/// split by tabs and rows by lines, in the order they're in.
pub(super) fn docx_text(xml: &str) -> Result<String, AttachError> {
    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    let mut in_text = false;
    let mut cells = 0usize;
    loop {
        match reader.read_event().map_err(|_| unreadable("Word file"))? {
            Event::Start(e) => match local(e.name().as_ref().as_bytes()) {
                b"t" => in_text = true,
                b"tc" => cells += 1,
                _ => {}
            },
            Event::Empty(e) => match local(e.name().as_ref().as_bytes()) {
                b"tab" => out.push('\t'),
                b"br" | b"cr" => out.push('\n'),
                _ => {}
            },
            Event::End(e) => match local(e.name().as_ref().as_bytes()) {
                b"t" => in_text = false,
                // Inside a table cell, paragraphs share its line.
                b"p" => out.push(if cells > 0 { ' ' } else { '\n' }),
                b"tc" => {
                    cells = cells.saturating_sub(1);
                    trim_end_spaces(&mut out);
                    out.push('\t');
                }
                b"tr" => {
                    trim_end_tabs(&mut out);
                    out.push('\n');
                }
                _ => {}
            },
            Event::Text(t) if in_text => out.push_str(&t.xml10_content()),
            Event::CData(t) if in_text => out.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_text => out.extend(reference(&r)),
            Event::Eof => break,
            _ => {}
        }
        if out.len() > MAX_TEXT_OUT {
            break;
        }
    }
    Ok(out.trim_end().to_owned() + "\n")
}

fn trim_end_spaces(s: &mut String) {
    while s.ends_with(' ') {
        s.pop();
    }
}

fn trim_end_tabs(s: &mut String) {
    while s.ends_with('\t') || s.ends_with(' ') {
        s.pop();
    }
}

/// Placeholders on a slide or notes page that aren't its words: the slide
/// number, the date, the footer, and the picture of the slide on a notes
/// page.
const SKIPPED_PLACEHOLDERS: [&str; 6] = ["sldNum", "dt", "ftr", "hdr", "sldImg", "sldNumber"];

/// A slide's (or notes page's) words: each paragraph on its own line.
fn slide_words(xml: &str) -> Result<String, AttachError> {
    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    // The shape being read, and whether it's a placeholder to skip.
    let mut shape = String::new();
    let mut skip = false;
    let mut in_shape = 0usize;
    let mut in_text = false;
    let mut line = String::new();
    loop {
        match reader
            .read_event()
            .map_err(|_| unreadable("PowerPoint file"))?
        {
            Event::Start(e) => match local(e.name().as_ref().as_bytes()) {
                b"sp" => {
                    in_shape += 1;
                    if in_shape == 1 {
                        shape.clear();
                        skip = false;
                    }
                }
                b"t" => in_text = true,
                b"ph" => skip |= skipped(&e),
                _ => {}
            },
            Event::Empty(e) => match local(e.name().as_ref().as_bytes()) {
                b"ph" => skip |= skipped(&e),
                b"br" => line.push('\n'),
                _ => {}
            },
            Event::End(e) => match local(e.name().as_ref().as_bytes()) {
                b"t" => in_text = false,
                b"p" => {
                    let l = line.trim();
                    if !l.is_empty() {
                        let target = if in_shape > 0 { &mut shape } else { &mut out };
                        target.push_str(l);
                        target.push('\n');
                    }
                    line.clear();
                }
                b"sp" => {
                    in_shape = in_shape.saturating_sub(1);
                    if in_shape == 0 && !skip {
                        out.push_str(&shape);
                    }
                }
                _ => {}
            },
            Event::Text(t) if in_text => line.push_str(&t.xml10_content()),
            Event::CData(t) if in_text => line.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_text => line.extend(reference(&r)),
            Event::Eof => break,
            _ => {}
        }
        if out.len() > MAX_TEXT_OUT {
            break;
        }
    }
    Ok(out)
}

fn skipped(ph: &quick_xml::events::BytesStart<'_>) -> bool {
    ph.attributes().flatten().any(|a| {
        local(a.key.as_ref().as_bytes()) == b"type" && SKIPPED_PLACEHOLDERS.contains(&&*a.value)
    })
}

/// The `Id → Target` relationships in a `.rels` file, with the type.
fn relationships(xml: &str) -> HashMap<String, (String, String)> {
    let mut reader = Reader::from_str(xml);
    let mut out = HashMap::new();
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e)
                if local(e.name().as_ref().as_bytes()) == b"Relationship" =>
            {
                let mut id = String::new();
                let mut target = String::new();
                let mut kind = String::new();
                for a in e.attributes().flatten() {
                    let value = quick_xml::escape::unescape(&a.value)
                        .map(std::borrow::Cow::into_owned)
                        .unwrap_or_default();
                    match a.key.as_ref() {
                        "Id" => id = value,
                        "Target" => target = value,
                        "Type" => kind = value,
                        _ => {}
                    }
                }
                out.insert(id, (target, kind));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// `../notesSlides/notesSlide1.xml` from `ppt/slides/` → `ppt/notesSlides/notesSlide1.xml`.
fn resolve(base_dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_owned();
    }
    let mut parts: Vec<&str> = base_dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in target.split('/') {
        match part {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// The slides' file names in the deck's order (the order the deck lists
/// them, else by number).
fn slide_order<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Result<Vec<String>, AttachError> {
    let rels = entry_text(zip, "ppt/_rels/presentation.xml.rels")?
        .map(|x| relationships(&x))
        .unwrap_or_default();
    let mut ordered = Vec::new();
    if let Some(xml) = entry_text(zip, "ppt/presentation.xml")? {
        let mut reader = Reader::from_str(&xml);
        while let Ok(event) = reader.read_event() {
            match event {
                Event::Start(e) | Event::Empty(e)
                    if local(e.name().as_ref().as_bytes()) == b"sldId" =>
                {
                    let rid = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref().ends_with(":id"))
                        .map(|a| a.value.into_owned());
                    if let Some((target, _)) = rid.and_then(|r| rels.get(&r)) {
                        ordered.push(resolve("ppt", target));
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
    }
    if ordered.is_empty() {
        let mut found: Vec<(u32, String)> = zip
            .file_names()
            .filter_map(Result::ok)
            .filter_map(|n| {
                let num = n
                    .strip_prefix("ppt/slides/slide")?
                    .strip_suffix(".xml")?
                    .parse()
                    .ok()?;
                Some((num, n.into_owned()))
            })
            .collect();
        found.sort();
        ordered = found.into_iter().map(|(_, n)| n).collect();
    }
    Ok(ordered)
}

/// A deck's text: `## Slide n`, its words, then its speaker notes.
fn pptx_text<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> Result<String, AttachError> {
    let mut out = String::new();
    for (n, slide) in slide_order(zip)?.iter().enumerate() {
        let Some(xml) = entry_text(zip, slide)? else {
            continue;
        };
        let _ = writeln!(out, "## Slide {}", n + 1);
        out.push_str(&slide_words(&xml)?);
        let (dir, file) = slide.rsplit_once('/').unwrap_or(("", slide));
        let rels_name = format!("{dir}/_rels/{file}.rels");
        let notes = entry_text(zip, &rels_name)?
            .map(|x| relationships(&x))
            .unwrap_or_default()
            .into_values()
            .find(|(_, kind)| kind.ends_with("/notesSlide"))
            .map(|(target, _)| resolve(dir, &target));
        if let Some(notes) = notes
            && let Some(xml) = entry_text(zip, &notes)?
        {
            let words = slide_words(&xml)?;
            if !words.trim().is_empty() {
                out.push_str("Notes:\n");
                out.push_str(&words);
            }
        }
        out.push('\n');
        if out.len() > MAX_TEXT_OUT {
            break;
        }
    }
    Ok(out)
}

/// A spreadsheet's text: each sheet as CSV under `## Sheet: name`, at
/// most [`MAX_SHEET_ROWS`] rows each.
fn sheet_text(path: &Path, ext: &str) -> Result<String, AttachError> {
    use calamine::Reader as _;
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let bad = |_| unreadable("spreadsheet");
    let mut book: calamine::Sheets<_> = match ext {
        "xls" => calamine::Sheets::Xls(
            calamine::open_workbook_from_rs(file)
                .map_err(|e: calamine::XlsError| bad(e.to_string()))?,
        ),
        "xlsb" => calamine::Sheets::Xlsb(
            calamine::open_workbook_from_rs(file)
                .map_err(|e: calamine::XlsbError| bad(e.to_string()))?,
        ),
        "ods" => calamine::Sheets::Ods(
            calamine::open_workbook_from_rs(file)
                .map_err(|e: calamine::OdsError| bad(e.to_string()))?,
        ),
        _ => calamine::Sheets::Xlsx(
            calamine::open_workbook_from_rs(file)
                .map_err(|e: calamine::XlsxError| bad(e.to_string()))?,
        ),
    };
    let mut out = String::new();
    let names = book.sheet_names();
    for name in names.iter().take(MAX_SHEETS) {
        let Ok(range) = book.worksheet_range(name) else {
            continue;
        };
        let _ = writeln!(out, "## Sheet: {name}");
        let rows = range.height();
        for row in range.rows().take(MAX_SHEET_ROWS) {
            let mut cells: Vec<String> = row.iter().map(|c| csv_field(&c.to_string())).collect();
            while cells.last().is_some_and(String::is_empty) {
                cells.pop();
            }
            out.push_str(&cells.join(","));
            out.push('\n');
            if out.len() > MAX_TEXT_OUT {
                return Ok(out);
            }
        }
        if rows > MAX_SHEET_ROWS {
            let _ = writeln!(out, "… {} more rows", rows - MAX_SHEET_ROWS);
        }
        out.push('\n');
    }
    if names.len() > MAX_SHEETS {
        let _ = writeln!(out, "… {} more sheets", names.len() - MAX_SHEETS);
    }
    Ok(out)
}

/// One CSV field, quoted when it needs to be.
fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
pub(super) mod build {
    //! Small Office files made in memory, for tests.

    use std::io::Write as _;

    /// A zip holding `files` (name, contents), stored as they are.
    pub(in crate::attach) fn zip(files: &[(&str, &str)]) -> Vec<u8> {
        with(files, zip::CompressionMethod::Stored)
    }

    /// Like [`zip`], compressed.
    pub(in crate::attach) fn zip_deflated(files: &[(&str, &str)]) -> Vec<u8> {
        with(files, zip::CompressionMethod::Deflated)
    }

    fn with(files: &[(&str, &str)], method: zip::CompressionMethod) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, body) in files {
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(method)
                .large_file(body.len() > u32::MAX as usize);
            zip.start_file(*name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }
}
