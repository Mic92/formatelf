//! Render the output file from an ordered list of owned spans: the ELF header,
//! the program and section header tables, and each section's data, placed at
//! the offsets the layout engine assigned. Gaps within the original are copied
//! verbatim from the input, so unchanged bytes (.text, padding, the stale image
//! of a relocated section) survive without being enumerated; the grown tail
//! stays zero. Overlapping spans signal an inconsistent layout and are rejected.

use std::borrow::Cow;
use std::io::Write;

use crate::codec;
use crate::error::{Error, Result};
use crate::ir::{sht, ElfImage};

/// A changed region placed at `at` in the output.
struct Span<'a> {
    at: u64,
    bytes: Cow<'a, [u8]>,
}

/// Encode the headers and section data into placed spans. The layout engine
/// must already have assigned every offset and synced section sizes. A region
/// that already matches the input is dropped: the gap copy reproduces it, so
/// the resulting spans are exactly the file's delta from the original.
fn owned_spans<'a>(image: &'a ElfImage<'_>, original: &[u8]) -> Result<Vec<Span<'a>>> {
    let enc = image.enc;
    let mut spans = Vec::new();
    let mut push = |at: u64, bytes: Cow<'a, [u8]>| {
        let a = at as usize;
        if bytes.is_empty() || original.get(a..a + bytes.len()) == Some(bytes.as_ref()) {
            return;
        }
        spans.push(Span { at, bytes });
    };

    let mut ehdr = Vec::new();
    codec::write_ehdr(
        enc,
        &image.ehdr,
        image.phdrs.len(),
        image.shdrs.len(),
        &mut ehdr,
    )?;
    push(0, Cow::Owned(ehdr));

    let mut pht = Vec::new();
    for p in &image.phdrs {
        codec::write_phdr(enc, p, &mut pht)?;
    }
    push(image.ehdr.phoff, Cow::Owned(pht));

    let mut sht = Vec::new();
    for s in &image.shdrs {
        codec::write_shdr(enc, s, &mut sht)?;
    }
    push(image.ehdr.shoff, Cow::Owned(sht));

    for (s, data) in image.shdrs.iter().zip(&image.section_data) {
        if s.sh_type == sht::NOBITS {
            continue;
        }
        if data.len() as u64 != s.size {
            return Err(Error::Serialize(format!(
                "section size {} != header {}; layout not applied",
                data.len(),
                s.size
            )));
        }
        push(s.offset, Cow::Borrowed(data.as_ref()));
    }
    Ok(spans)
}

/// Stream the output front to back: changed spans at their offsets, original
/// bytes (or zero past the input end) in between. No seeking required.
pub fn write_to(
    image: &ElfImage<'_>,
    original: &[u8],
    total: u64,
    out: &mut dyn Write,
) -> Result<()> {
    let mut spans = owned_spans(image, original)?;
    spans.sort_by_key(|s| s.at);

    let orig_len = original.len() as u64;
    let io = |r: std::io::Result<()>| r.map_err(|e| Error::Serialize(e.to_string()));
    // Emit `[at, at+len)` from the original, zero-filling past the input end.
    let fill = |out: &mut dyn Write, at: u64, len: u64| -> Result<()> {
        let start = at.min(orig_len);
        let avail = (orig_len - start).min(len);
        io(out.write_all(&original[start as usize..(start + avail) as usize]))?;
        let mut pad = len - avail;
        let zeros = [0u8; 4096];
        while pad > 0 {
            let n = pad.min(zeros.len() as u64);
            io(out.write_all(&zeros[..n as usize]))?;
            pad -= n;
        }
        Ok(())
    };

    let mut cur = 0u64;
    for s in &spans {
        let len = s.bytes.len() as u64;
        if s.at < cur {
            return Err(Error::Serialize(format!(
                "overlapping span at offset {}",
                s.at
            )));
        }
        if s.at + len > total {
            return Err(Error::Serialize(format!(
                "span at {} extends past file end {total}",
                s.at
            )));
        }
        if s.at > cur {
            fill(out, cur, s.at - cur)?;
        }
        io(out.write_all(&s.bytes))?;
        cur = s.at + len;
    }
    if total > cur {
        fill(out, cur, total - cur)?;
    }
    Ok(())
}

/// Collect the streamed output into a buffer.
pub fn write(image: &ElfImage<'_>, original: &[u8], total: u64) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(total as usize);
    write_to(image, original, total, &mut buf)?;
    Ok(buf)
}
