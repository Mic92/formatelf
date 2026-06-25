//! In-place serializer: patch the IR back onto the original file bytes used as
//! a canvas. Valid only while every region keeps its original size and offset
//! (identity round-trips and edits that fit in place). Size-changing edits must
//! first go through the layout engine, which reassigns offsets.

use crate::codec;
use crate::error::{Error, Result};
use crate::ir::{sht, ElfImage, Encoding};

fn put(buf: &mut [u8], off: u64, bytes: &[u8], what: &str) -> Result<()> {
    let off = off as usize;
    let end = off
        .checked_add(bytes.len())
        .filter(|&e| e <= buf.len())
        .ok_or_else(|| Error::Serialize(format!("{what}: write out of bounds")))?;
    buf[off..end].copy_from_slice(bytes);
    Ok(())
}

/// Encode a fixed-stride table (phdrs/shdrs) starting at `base`.
fn put_table<T>(
    buf: &mut [u8],
    base: u64,
    enc: Encoding,
    items: &[T],
    encode: impl Fn(Encoding, &T, &mut Vec<u8>),
    what: &str,
) -> Result<()> {
    let mut tmp = Vec::new();
    for (i, it) in items.iter().enumerate() {
        tmp.clear();
        encode(enc, it, &mut tmp);
        put(buf, base + (i * tmp.len()) as u64, &tmp, what)?;
    }
    Ok(())
}

pub fn write(image: &ElfImage, original: &[u8]) -> Result<Vec<u8>> {
    let mut buf = original.to_vec();
    let enc = image.enc;

    let mut tmp = Vec::new();
    codec::write_ehdr(
        enc,
        &image.ehdr,
        image.phdrs.len() as u16,
        image.shdrs.len() as u16,
        &mut tmp,
    );
    put(&mut buf, 0, &tmp, "ehdr")?;

    let phoff = image.ehdr.phoff;
    let shoff = image.ehdr.shoff;
    put_table(
        &mut buf,
        phoff,
        enc,
        &image.phdrs,
        codec::write_phdr,
        "phdr",
    )?;
    put_table(
        &mut buf,
        shoff,
        enc,
        &image.shdrs,
        codec::write_shdr,
        "shdr",
    )?;

    // The parsed `dynamic` array is authoritative; re-encode it over the bytes
    // stored for its section so in-place edits to entries take effect.
    if let Some(idx) = image.shdrs.iter().position(|s| s.sh_type == sht::DYNAMIC) {
        tmp.clear();
        for d in &image.dynamic {
            codec::write_dyn(enc, d, &mut tmp);
        }
        if tmp.len() as u64 > image.shdrs[idx].size {
            return Err(Error::Serialize(
                "dynamic section grew; needs relayout".into(),
            ));
        }
        put(&mut buf, image.shdrs[idx].offset, &tmp, "dynamic")?;
    }

    for (s, data) in image.shdrs.iter().zip(&image.section_data) {
        if s.sh_type == sht::NOBITS || s.sh_type == sht::DYNAMIC {
            continue;
        }
        if data.len() as u64 != s.size {
            return Err(Error::Serialize(format!(
                "section size changed ({} != {}); needs relayout",
                data.len(),
                s.size
            )));
        }
        put(&mut buf, s.offset, data, "section")?;
    }

    Ok(buf)
}
