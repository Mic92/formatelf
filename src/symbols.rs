//! Rename dynamic symbols, rebuilding the structures that index them by name
//! or position: the GNU and `SysV` hash tables, the `.gnu.version` array and the
//! symbol indices held in relocation entries. Mirrors patchelf's
//! `renameDynamicSymbols`/`rebuildGnuHashTable`/`rebuildHashTable`.
//!
//! All edits are in place except `.dynstr`, which grows by the appended names;
//! the layout engine assigns its new offset afterwards.

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::ir::{Class, ElfImage, Endian, sht, usize_at};

fn enc(big: bool) -> Endian {
    if big { Endian::Big } else { Endian::Little }
}

fn rd_u32(big: bool, b: &[u8], o: usize) -> u32 {
    enc(big).read_u32(b, o)
}

fn wr_u32(big: bool, b: &mut [u8], o: usize, v: u32) {
    enc(big).write_u32(b, o, v);
}

fn rd_uptr(big: bool, elf64: bool, b: &[u8], o: usize) -> u64 {
    if elf64 {
        enc(big).read_u64(b, o)
    } else {
        u64::from(enc(big).read_u32(b, o))
    }
}

fn wr_uptr(big: bool, elf64: bool, b: &mut [u8], o: usize, v: u64) {
    if elf64 {
        enc(big).write_u64(b, o, v);
    } else {
        // ELF32 stores a 32-bit word here; `v` is an address or bloom word that
        // fits by construction, so the narrowing is intentional.
        #[allow(clippy::cast_possible_truncation)]
        enc(big).write_u32(b, o, v as u32);
    }
}

fn gnu_hash(name: &[u8]) -> u32 {
    let mut h = 5381u32;
    for &c in name {
        h = h.wrapping_mul(33).wrapping_add(u32::from(c));
    }
    h
}

fn sysv_hash(name: &[u8]) -> u32 {
    let mut h = 0u32;
    for &c in name {
        h = (h << 4).wrapping_add(u32::from(c));
        let g = h & 0xf000_0000;
        if g != 0 {
            h ^= g >> 24;
        }
        h &= !g;
    }
    h
}

/// Name bytes of dynamic symbol `i`, borrowed from `.dynstr`. The hashes work
/// on bytes, so no UTF-8 check or copy.
fn sym_name<'a>(
    image: &'a ElfImage<'_>,
    dynsym: usize,
    dynstr: usize,
    symsize: usize,
    i: usize,
) -> &'a [u8] {
    let big = image.enc.endian == Endian::Big;
    let st_name = rd_u32(big, &image.section_data[dynsym], i * symsize) as usize;
    let tab = &image.section_data[dynstr];
    let rest = tab.get(st_name..).unwrap_or(&[]);
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    &rest[..end]
}

pub(crate) fn rename_dynamic_symbols(
    image: &mut ElfImage<'_>,
    remap: &BTreeMap<String, String>,
) -> Result<()> {
    let big = image.enc.endian == Endian::Big;
    let elf64 = image.enc.class == Class::Elf64;
    let symsize = if elf64 { 24 } else { 16 };

    let dynsym = image
        .find_section(".dynsym")
        .ok_or_else(|| Error::Missing("cannot find section .dynsym".into()))?;
    let dynstr = image
        .find_section(".dynstr")
        .ok_or_else(|| Error::Missing("cannot find section .dynstr".into()))?;
    let nsyms = image.section_data[dynsym].len() / symsize;

    // Collect renames first to avoid aliasing the two section buffers, then
    // append the new names to .dynstr and repoint st_name.
    let mut renames: Vec<(usize, String)> = Vec::new();
    for i in 0..nsyms {
        let name = sym_name(image, dynsym, dynstr, symsize, i);
        if let Ok(s) = std::str::from_utf8(name)
            && let Some(new) = remap.get(s)
        {
            renames.push((i, new.clone()));
        }
    }
    if renames.is_empty() {
        return Ok(());
    }
    for (i, new) in &renames {
        let off = u32::try_from(image.section_data[dynstr].len())
            .map_err(|_| Error::Parse(".dynstr exceeds 32-bit st_name range".into()))?;
        image.section_data[dynstr]
            .to_mut()
            .extend_from_slice(new.as_bytes());
        image.section_data[dynstr].to_mut().push(0);
        wr_u32(big, image.section_data[dynsym].to_mut(), i * symsize, off);
    }

    // Read every name once and derive both hashes; the two tables would
    // otherwise walk the whole symbol table separately.
    let (gnu_hashes, sysv_hashes) = symbol_hashes(image, dynsym, dynstr, symsize, nsyms);
    let reorder = rebuild_gnu_hash(image, &gnu_hashes, nsyms)?;
    rebuild_sysv_hash(image, &sysv_hashes, nsyms, reorder.as_ref())?;
    Ok(())
}

/// GNU and `SysV` hashes of every dynamic symbol, indexed by symbol position, in
/// the pre-reorder order. Computed in a single pass over `.dynstr`.
fn symbol_hashes(
    image: &ElfImage<'_>,
    dynsym: usize,
    dynstr: usize,
    symsize: usize,
    nsyms: usize,
) -> (Vec<u32>, Vec<u32>) {
    let mut gnu = vec![0u32; nsyms];
    let mut sysv = vec![0u32; nsyms];
    for i in 0..nsyms {
        let name = sym_name(image, dynsym, dynstr, symsize, i);
        gnu[i] = gnu_hash(name);
        sysv[i] = sysv_hash(name);
    }
    (gnu, sysv)
}

/// How `rebuild_gnu_hash` permuted the hashed symbols, so the `SysV` table can find
/// each symbol's pre-reorder hash without re-reading names: `new2old[k]` is the
/// old position (relative to `symndx`) of the symbol now at new position `k`.
struct Reorder {
    symndx: usize,
    new2old: Vec<usize>,
}

/// A hashed symbol's GNU hash, target bucket, and pre-sort position.
struct Entry {
    hash: u32,
    bucket: usize,
    old_pos: usize,
}

/// Reorder the hashed symbols by bucket (a GNU hash invariant), remap every
/// structure that referenced them by index, then refill the bloom filter,
/// buckets and chain.
fn rebuild_gnu_hash(
    image: &mut ElfImage<'_>,
    gnu_hashes: &[u32],
    nsyms: usize,
) -> Result<Option<Reorder>> {
    let symsize = if image.enc.class == Class::Elf64 {
        24
    } else {
        16
    };
    let dynsym = image.find_section(".dynsym").unwrap();
    let Some(gh) = image.find_section(".gnu.hash") else {
        return Ok(None);
    };
    let big = image.enc.endian == Endian::Big;
    let elf64 = image.enc.class == Class::Elf64;
    let word = if elf64 { 8 } else { 4 };
    let wordbits: u32 = if elf64 { 64 } else { 32 };

    let data = &image.section_data[gh];
    if data.len() < 16 {
        return Err(Error::Parse(".gnu.hash too small".into()));
    }
    let num_buckets = rd_u32(big, data, 0) as usize;
    let symndx = rd_u32(big, data, 4) as usize;
    let maskwords = rd_u32(big, data, 8) as usize;
    let shift2 = rd_u32(big, data, 12);
    let bloom_off = 16;
    let buckets_off = bloom_off + maskwords * word;
    let table_off = buckets_off + num_buckets * 4;
    if num_buckets == 0 || table_off > data.len() {
        return Ok(None); // empty table: symndx is not meaningful
    }
    let count = nsyms - symndx;

    let mut entries: Vec<Entry> = (0..count)
        .map(|old_pos| {
            let hash = gnu_hashes[symndx + old_pos];
            Entry {
                hash,
                bucket: hash as usize % num_buckets,
                old_pos,
            }
        })
        .collect();
    // A GNU hash table requires the hashed symbols grouped by bucket.
    entries.sort_by_key(|e| e.bucket);

    let mut old2new = vec![0usize; count];
    let mut new2old = vec![0usize; count];
    for (new_i, e) in entries.iter().enumerate() {
        old2new[e.old_pos] = new_i;
        new2old[new_i] = e.old_pos;
    }

    reorder_records(
        image.section_data[dynsym].to_mut(),
        symndx * symsize,
        symsize,
        &old2new,
    );
    if let Some(ver) = image.find_section(".gnu.version") {
        reorder_records(image.section_data[ver].to_mut(), symndx * 2, 2, &old2new);
    }
    remap_reloc_symbols(image, symndx, &old2new);

    // Refill bloom filter, buckets and chain in the new order.
    let data = image.section_data[gh].to_mut();
    for b in &mut data[bloom_off..buckets_off] {
        *b = 0;
    }
    for e in &entries {
        let idx = (e.hash / wordbits) as usize % maskwords;
        let o = bloom_off + idx * word;
        let mut v = rd_uptr(big, elf64, data, o);
        v |= 1u64 << (e.hash % wordbits);
        v |= 1u64 << ((e.hash >> shift2) % wordbits);
        wr_uptr(big, elf64, data, o, v);
    }
    for b in &mut data[buckets_off..table_off] {
        *b = 0;
    }
    // First symbol of each bucket; later symbols chain from it.
    for (new_i, e) in entries.iter().enumerate() {
        let o = buckets_off + e.bucket * 4;
        if rd_u32(big, data, o) == 0 {
            let v = u32::try_from(new_i + symndx)
                .map_err(|_| Error::Parse("symbol index exceeds 32 bits".into()))?;
            wr_u32(big, data, o, v);
        }
    }
    // Chain word per symbol: low bit set marks the last entry in a bucket.
    for (new_i, e) in entries.iter().enumerate() {
        let last = new_i + 1 == count || entries[new_i + 1].bucket != e.bucket;
        let v = if last { e.hash | 1 } else { e.hash & !1 };
        wr_u32(big, data, table_off + new_i * 4, v);
    }
    Ok(Some(Reorder { symndx, new2old }))
}

fn rebuild_sysv_hash(
    image: &mut ElfImage<'_>,
    sysv_hashes: &[u32],
    nsyms: usize,
    reorder: Option<&Reorder>,
) -> Result<()> {
    let Some(hash) = image.find_section(".hash") else {
        return Ok(());
    };
    let big = image.enc.endian == Endian::Big;
    let data = &image.section_data[hash];
    if data.len() < 8 {
        return Ok(());
    }
    let num_buckets_u32 = rd_u32(big, data, 0);
    let num_buckets = num_buckets_u32 as usize;
    let nchain = rd_u32(big, data, 4) as usize;
    if num_buckets == 0 || 8 + (num_buckets + nchain) * 4 > data.len() {
        return Ok(());
    }
    let buckets_off = 8;
    let chain_off = buckets_off + num_buckets * 4;
    let first = nsyms - nchain;

    // The GNU reorder permuted symbols in [symndx, nsyms); map each new slot
    // back to its old one to reuse the pre-reorder hash.
    let old_index = |new: usize| match reorder {
        Some(r) if new >= r.symndx => r.symndx + r.new2old[new - r.symndx],
        _ => new,
    };
    let names: Vec<u32> = (first..nsyms)
        .map(|i| sysv_hashes[old_index(i)] % num_buckets_u32)
        .collect();
    let data = image.section_data[hash].to_mut();
    for b in &mut data[buckets_off..buckets_off + (num_buckets + nchain) * 4] {
        *b = 0;
    }
    for (i, &bucket) in names.iter().enumerate() {
        let bo = buckets_off + bucket as usize * 4;
        let prev = rd_u32(big, data, bo);
        wr_u32(big, data, chain_off + i * 4, prev);
        let v =
            u32::try_from(i).map_err(|_| Error::Parse("symbol index exceeds 32 bits".into()))?;
        wr_u32(big, data, bo, v);
    }
    Ok(())
}

/// Move fixed-stride records in `data[base..]` so record at old position p ends
/// up at `old2new[p]`.
fn reorder_records(data: &mut [u8], base: usize, stride: usize, old2new: &[usize]) {
    let region = data[base..base + old2new.len() * stride].to_vec();
    for (old, &new) in old2new.iter().enumerate() {
        let src = &region[old * stride..(old + 1) * stride];
        data[base + new * stride..base + (new + 1) * stride].copy_from_slice(src);
    }
}

/// Rewrite relocation symbol indices after the symbol table was reordered.
fn remap_reloc_symbols(image: &mut ElfImage<'_>, symndx: usize, old2new: &[usize]) {
    let big = image.enc.endian == Endian::Big;
    let elf64 = image.enc.class == Class::Elf64;
    let ptr = if elf64 { 8 } else { 4 };
    let info_off = ptr; // r_info follows r_offset
    let remap = |old: usize| -> usize {
        if old >= symndx {
            old2new[old - symndx] + symndx
        } else {
            old
        }
    };
    for i in 0..image.shdrs.len() {
        let stride = match (image.shdrs[i].sh_type, elf64) {
            (sht::RELA, true) => 24,
            (sht::RELA, false) => 12,
            (sht::REL, true) => 16,
            (sht::REL, false) => 8,
            _ => continue,
        };
        let data = image.section_data[i].to_mut();
        for r in 0..data.len() / stride {
            let o = r * stride + info_off;
            let info = rd_uptr(big, elf64, data, o);
            let (sym_shift, sym_mask) = if elf64 {
                (32, 0xffff_ffffu64)
            } else {
                (8, 0xff)
            };
            let old = usize_at(info >> sym_shift);
            let new = remap(old);
            if new != old {
                wr_uptr(
                    big,
                    elf64,
                    data,
                    o,
                    (info & sym_mask) | ((new as u64) << sym_shift),
                );
            }
        }
    }
}
