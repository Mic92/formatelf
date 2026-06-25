//! Render the output file from an ordered list of owned spans: the ELF header,
//! the program and section header tables, and each section's data, placed at
//! the offsets the layout engine assigned. Gaps within the original are copied
//! verbatim from the input, so unchanged bytes (.text, padding, the stale image
//! of a relocated section) survive without being enumerated; the grown tail
//! stays zero. Overlapping spans signal an inconsistent layout and are rejected.

use crate::codec;
use crate::error::{Error, Result};
use crate::ir::{sht, ElfImage};

/// A changed region placed at `at` in the output.
struct Span {
    at: u64,
    bytes: Vec<u8>,
}

/// Encode the headers and section data into placed spans. The layout engine
/// must already have assigned every offset and synced section sizes.
fn owned_spans(image: &ElfImage) -> Result<Vec<Span>> {
    let enc = image.enc;
    let mut spans = Vec::new();
    let mut push = |at: u64, bytes: Vec<u8>| {
        if !bytes.is_empty() {
            spans.push(Span { at, bytes });
        }
    };

    let mut ehdr = Vec::new();
    codec::write_ehdr(
        enc,
        &image.ehdr,
        image.phdrs.len(),
        image.shdrs.len(),
        &mut ehdr,
    )?;
    push(0, ehdr);

    let mut pht = Vec::new();
    for p in &image.phdrs {
        codec::write_phdr(enc, p, &mut pht)?;
    }
    push(image.ehdr.phoff, pht);

    let mut sht = Vec::new();
    for s in &image.shdrs {
        codec::write_shdr(enc, s, &mut sht)?;
    }
    push(image.ehdr.shoff, sht);

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
        push(s.offset, data.clone());
    }
    Ok(spans)
}

pub fn write(image: &ElfImage, original: &[u8], total: u64) -> Result<Vec<u8>> {
    let mut spans = owned_spans(image)?;
    spans.sort_by_key(|s| s.at);

    let orig_len = original.len() as u64;
    let mut buf = vec![0u8; total as usize];
    let copy = |buf: &mut [u8], at: u64, len: u64, src: u64| {
        let (at, len, src) = (at as usize, len as usize, src as usize);
        buf[at..at + len].copy_from_slice(&original[src..src + len]);
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
        // Fill the preceding gap with the original bytes that live there.
        let gap_end = s.at.min(orig_len);
        if gap_end > cur {
            copy(&mut buf, cur, gap_end - cur, cur);
        }
        buf[s.at as usize..s.at as usize + s.bytes.len()].copy_from_slice(&s.bytes);
        cur = s.at + len;
    }
    // Trailing original bytes (e.g. an in-place rewrite shorter than the file).
    let tail_end = total.min(orig_len);
    if tail_end > cur {
        copy(&mut buf, cur, tail_end - cur, cur);
    }
    Ok(buf)
}
