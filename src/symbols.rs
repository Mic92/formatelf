//! Rename dynamic symbols, rebuilding the structures that index them by name
//! or position: the GNU and SysV hash tables, the `.gnu.version` array and the
//! symbol indices held in relocation entries. Mirrors patchelf's
//! `renameDynamicSymbols`/`rebuildGnuHashTable`/`rebuildHashTable`.
//!
//! All edits are in place except `.dynstr`, which grows by the appended names;
//! the layout engine assigns its new offset afterwards.

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::ir::{cstr, sht, Class, ElfImage, Endian};

fn enc(big: bool) -> Endian {
    if big {
        Endian::Big
    } else {
        Endian::Little
    }
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
        enc(big).read_u32(b, o) as u64
    }
}

fn wr_uptr(big: bool, elf64: bool, b: &mut [u8], o: usize, v: u64) {
    if elf64 {
        enc(big).write_u64(b, o, v);
    } else {
        enc(big).write_u32(b, o, v as u32);
    }
}

fn gnu_hash(name: &[u8]) -> u32 {
    let mut h = 5381u32;
    for &c in name {
        h = h.wrapping_mul(33).wrapping_add(c as u32);
    }
    h
}

fn sysv_hash(name: &[u8]) -> u32 {
    let mut h = 0u32;
    for &c in name {
        h = (h << 4).wrapping_add(c as u32);
        let g = h & 0xf000_0000;
        if g != 0 {
            h ^= g >> 24;
        }
        h &= !g;
    }
    h
}

/// Name of dynamic symbol `i`, read from the (possibly grown) `.dynstr`.
fn sym_name(image: &ElfImage, dynsym: usize, dynstr: usize, symsize: usize, i: usize) -> Vec<u8> {
    let big = image.enc.endian == Endian::Big;
    let st_name = rd_u32(big, &image.section_data[dynsym], i * symsize);
    let tab = &image.section_data[dynstr];
    cstr(tab, st_name).unwrap_or("").as_bytes().to_vec()
}

pub fn rename_dynamic_symbols(
    image: &mut ElfImage,
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
        if let Ok(s) = std::str::from_utf8(&name) {
            if let Some(new) = remap.get(s) {
                renames.push((i, new.clone()));
            }
        }
    }
    if renames.is_empty() {
        return Ok(());
    }
    for (i, new) in &renames {
        let off = image.section_data[dynstr].len() as u32;
        image.section_data[dynstr].extend_from_slice(new.as_bytes());
        image.section_data[dynstr].push(0);
        wr_u32(big, &mut image.section_data[dynsym], i * symsize, off);
    }

    rebuild_gnu_hash(image, dynsym, dynstr, symsize, nsyms)?;
    rebuild_sysv_hash(image, dynsym, dynstr, symsize, nsyms);
    Ok(())
}

/// Reorder the hashed symbols by bucket (a GNU hash invariant), remap every
/// structure that referenced them by index, then refill the bloom filter,
/// buckets and chain.
fn rebuild_gnu_hash(
    image: &mut ElfImage,
    dynsym: usize,
    dynstr: usize,
    symsize: usize,
    nsyms: usize,
) -> Result<()> {
    let Some(gh) = image.find_section(".gnu.hash") else {
        return Ok(());
    };
    let big = image.enc.endian == Endian::Big;
    let elf64 = image.enc.class == Class::Elf64;
    let word = if elf64 { 8 } else { 4 };
    let wordbits = (word * 8) as u32;

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
        return Ok(()); // empty table: symndx is not meaningful
    }
    let count = nsyms - symndx;

    // hash, bucket and original position for each hashed symbol.
    let mut entries: Vec<(u32, usize, usize)> = (0..count)
        .map(|pos| {
            let name = sym_name(image, dynsym, dynstr, symsize, symndx + pos);
            let h = gnu_hash(&name);
            (h, h as usize % num_buckets, pos)
        })
        .collect();
    entries.sort_by_key(|e| e.1);

    let mut old2new = vec![0usize; count];
    for (new_i, e) in entries.iter().enumerate() {
        old2new[e.2] = new_i;
    }

    reorder_records(
        &mut image.section_data[dynsym],
        symndx * symsize,
        symsize,
        &old2new,
    );
    if let Some(ver) = image.find_section(".gnu.version") {
        reorder_records(&mut image.section_data[ver], symndx * 2, 2, &old2new);
    }
    remap_reloc_symbols(image, symndx, &old2new);

    // Refill bloom filter, buckets and chain in the new order.
    let data = &mut image.section_data[gh];
    for b in &mut data[bloom_off..buckets_off] {
        *b = 0;
    }
    for &(h, _, _) in &entries {
        let idx = (h / wordbits) as usize % maskwords;
        let o = bloom_off + idx * word;
        let mut v = rd_uptr(big, elf64, data, o);
        v |= 1u64 << (h % wordbits);
        v |= 1u64 << ((h >> shift2) % wordbits);
        wr_uptr(big, elf64, data, o, v);
    }
    for b in &mut data[buckets_off..table_off] {
        *b = 0;
    }
    for (new_i, &(_, bucket, _)) in entries.iter().enumerate() {
        let o = buckets_off + bucket * 4;
        if rd_u32(big, data, o) == 0 {
            wr_u32(big, data, o, (new_i + symndx) as u32);
        }
    }
    for (new_i, &(h, bucket, _)) in entries.iter().enumerate() {
        let last = new_i + 1 == count || entries[new_i + 1].1 != bucket;
        let v = if last { h | 1 } else { h & !1 };
        wr_u32(big, data, table_off + new_i * 4, v);
    }
    Ok(())
}

fn rebuild_sysv_hash(
    image: &mut ElfImage,
    dynsym: usize,
    dynstr: usize,
    symsize: usize,
    nsyms: usize,
) {
    let Some(hash) = image.find_section(".hash") else {
        return;
    };
    let big = image.enc.endian == Endian::Big;
    let data = &image.section_data[hash];
    if data.len() < 8 {
        return;
    }
    let num_buckets = rd_u32(big, data, 0) as usize;
    let nchain = rd_u32(big, data, 4) as usize;
    if num_buckets == 0 || 8 + (num_buckets + nchain) * 4 > data.len() {
        return;
    }
    let buckets_off = 8;
    let chain_off = buckets_off + num_buckets * 4;
    let first = nsyms - nchain;

    let names: Vec<u32> = (first..nsyms)
        .map(|i| sysv_hash(&sym_name(image, dynsym, dynstr, symsize, i)) % num_buckets as u32)
        .collect();
    let data = &mut image.section_data[hash];
    for b in &mut data[buckets_off..buckets_off + (num_buckets + nchain) * 4] {
        *b = 0;
    }
    for (i, &bucket) in names.iter().enumerate() {
        let bo = buckets_off + bucket as usize * 4;
        let prev = rd_u32(big, data, bo);
        wr_u32(big, data, chain_off + i * 4, prev);
        wr_u32(big, data, bo, i as u32);
    }
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
fn remap_reloc_symbols(image: &mut ElfImage, symndx: usize, old2new: &[usize]) {
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
        let data = &mut image.section_data[i];
        for r in 0..data.len() / stride {
            let o = r * stride + info_off;
            let info = rd_uptr(big, elf64, data, o);
            let (sym_shift, sym_mask) = if elf64 {
                (32, 0xffff_ffffu64)
            } else {
                (8, 0xff)
            };
            let old = (info >> sym_shift) as usize;
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
