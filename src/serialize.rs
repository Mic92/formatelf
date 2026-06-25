//! Write the IR onto a byte buffer (a copy of the original file used as a
//! canvas, optionally pre-grown by the layout engine). Every section's data
//! must already match its header `sh_size` and `sh_offset`; the layout engine
//! is responsible for assigning offsets when sizes change. The `.dynamic`
//! array must be synced into its section data beforehand (see `layout`).

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
    encode: impl Fn(Encoding, &T, &mut Vec<u8>) -> Result<()>,
    what: &str,
) -> Result<()> {
    let mut tmp = Vec::new();
    for (i, it) in items.iter().enumerate() {
        tmp.clear();
        encode(enc, it, &mut tmp)?;
        put(buf, base + (i * tmp.len()) as u64, &tmp, what)?;
    }
    Ok(())
}

pub fn write(image: &ElfImage, mut buf: Vec<u8>) -> Result<Vec<u8>> {
    let enc = image.enc;

    let mut tmp = Vec::new();
    codec::write_ehdr(
        enc,
        &image.ehdr,
        image.phdrs.len(),
        image.shdrs.len(),
        &mut tmp,
    )?;
    put(&mut buf, 0, &tmp, "ehdr")?;

    put_table(
        &mut buf,
        image.ehdr.phoff,
        enc,
        &image.phdrs,
        codec::write_phdr,
        "phdr",
    )?;
    put_table(
        &mut buf,
        image.ehdr.shoff,
        enc,
        &image.shdrs,
        codec::write_shdr,
        "shdr",
    )?;

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
        put(&mut buf, s.offset, data, "section")?;
    }

    Ok(buf)
}
