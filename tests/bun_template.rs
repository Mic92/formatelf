//! Binaries with a `.bun` section are reused by `bun build --compile` as the
//! template for the output executable. bun appends the JS payload, grows the
//! writable `PT_LOAD` containing `.bun` to cover it, and moves everything past
//! that segment's file image as an opaque tail, fixing up only `e_shoff` and
//! `sh_offset`. Patching such a template must therefore keep all loaded bytes
//! (program header table included) below that segment's file end and must not
//! map anything above it. The oracle below re-implements bun's
//! `write_bun_section` and runs the "compiled" result.

mod fixtures;

use fixtures::{assert_dynstr_synced, assert_runs, copy, ours, out, patch, sample, zig_available};
use formatelf::ir::{pf, pt, shf, sht};
use formatelf::parser;
use std::path::Path;

/// The layout invariants bun's compile backend relies on (see module doc).
fn assert_bun_template_invariants(file: &Path) {
    let data = std::fs::read(file).unwrap();
    let img = parser::parse(&data).unwrap();

    let bun = img.find_section(".bun").expect(".bun section present");
    let bun_vaddr = img.shdrs[bun].addr;
    let rw = img
        .phdrs
        .iter()
        .find(|p| {
            p.p_type == pt::LOAD
                && p.flags & pf::W != 0
                && p.vaddr <= bun_vaddr
                && bun_vaddr < p.vaddr + p.memsz
        })
        .expect("writable PT_LOAD containing .bun");

    let max_vaddr_end = img
        .phdrs
        .iter()
        .filter(|p| p.p_type == pt::LOAD)
        .map(|p| p.vaddr + p.memsz)
        .max()
        .unwrap();
    assert_eq!(
        max_vaddr_end,
        rw.vaddr + rw.memsz,
        "a PT_LOAD maps addresses above the segment containing .bun"
    );

    let rw_file_end = rw.offset + rw.filesz;
    let pht_end = img.ehdr.phoff + img.phdrs.len() as u64 * u64::from(img.ehdr.phentsize);
    assert!(
        pht_end <= rw_file_end,
        "program header table at {:#x}..{pht_end:#x} sits in the movable tail (rw file end {rw_file_end:#x})",
        img.ehdr.phoff
    );
    for (i, p) in img.phdrs.iter().enumerate() {
        assert!(
            p.p_type != pt::LOAD || p.offset + p.filesz <= rw_file_end,
            "PT_LOAD {i} at {:#x}..{:#x} sits in the movable tail (rw file end {rw_file_end:#x})",
            p.offset,
            p.offset + p.filesz
        );
    }
    for (i, s) in img.shdrs.iter().enumerate() {
        assert!(
            s.flags & shf::ALLOC == 0
                || s.sh_type == sht::NOBITS
                || s.offset + s.size <= rw_file_end,
            "ALLOC section {} ({}) sits in the movable tail",
            i,
            img.section_name(i).unwrap_or("?")
        );
    }
    assert!(
        img.ehdr.shoff >= rw_file_end,
        "bun expects the section header table in the movable tail"
    );
}

/// Oracle: bun's `write_bun_section` (`src/exe_format/elf.rs`), ELF64LE only.
fn bun_write_section(data: &mut Vec<u8>, payload: &[u8]) {
    const PHSZ: u64 = 56;
    const SHSZ: u64 = 64;
    let r16 = |d: &[u8], o: usize| u16::from_le_bytes(d[o..o + 2].try_into().unwrap());
    let r32 = |d: &[u8], o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    let r64 = |d: &[u8], o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap());
    let w64 = |d: &mut [u8], o: usize, v: u64| d[o..o + 8].copy_from_slice(&v.to_le_bytes());
    let usz = |v: u64| usize::try_from(v).unwrap();
    let align_up = |v: u64, a: u64| v.div_ceil(a) * a;

    let (phoff, shoff) = (r64(data, 0x20), r64(data, 0x28));
    let (phnum, shnum, shstrndx) = (r16(data, 0x38), r16(data, 0x3c), r16(data, 0x3e));
    let page = 0x1000u64;

    let shdr = |i: u64| usz(shoff + i * SHSZ);
    let strtab_off = r64(data, shdr(u64::from(shstrndx)) + 0x18);
    let bun = (0..u64::from(shnum))
        .find(|&i| {
            let name = usz(strtab_off + u64::from(r32(data, shdr(i))));
            data[name..].starts_with(b".bun\0")
        })
        .expect(".bun section in template");
    let (bun_off, bun_vaddr) = (r64(data, shdr(bun) + 0x18), r64(data, shdr(bun) + 0x10));

    let ph = |i: u64| usz(phoff + i * PHSZ);
    let (mut rw_idx, mut max_end) = (None, 0u64);
    for i in 0..u64::from(phnum) {
        let o = ph(i);
        if r32(data, o) != pt::LOAD {
            continue;
        }
        let (vaddr, memsz) = (r64(data, o + 0x10), r64(data, o + 0x28));
        max_end = max_end.max(vaddr + memsz);
        if r32(data, o + 4) & pf::W != 0 && vaddr <= bun_vaddr && bun_vaddr < vaddr + memsz {
            rw_idx = Some(i);
        }
    }
    let rw = ph(rw_idx.expect("writable PT_LOAD containing .bun"));
    let (rw_off, rw_vaddr, rw_filesz, rw_memsz) = (
        r64(data, rw + 8),
        r64(data, rw + 0x10),
        r64(data, rw + 0x20),
        r64(data, rw + 0x28),
    );

    let content_size = 8 + payload.len() as u64;
    let aligned_size = align_up(content_size, page);
    let new_vaddr = align_up(max_end, page);
    let new_off = rw_off + (new_vaddr - rw_vaddr);
    assert!(new_vaddr >= rw_vaddr + rw_memsz, "vaddr collides");

    let old_end = rw_off + rw_filesz;
    assert!(shoff >= old_end, "bun requires the SHT in the movable tail");
    let tail = data[usz(old_end)..].to_vec();
    let dst = new_off + aligned_size;
    data.resize(usz(dst + tail.len() as u64), 0);
    data[usz(old_end)..usz(new_off)].fill(0);
    w64(data, usz(new_off), payload.len() as u64);
    data[usz(new_off + 8)..usz(new_off + content_size)].copy_from_slice(payload);
    data[usz(new_off + content_size)..usz(dst)].fill(0);
    data[usz(dst)..].copy_from_slice(&tail);
    w64(data, usz(bun_off), new_vaddr);

    let delta = dst - old_end;
    w64(data, 0x28, shoff + delta);
    for i in 0..u64::from(shnum) {
        let o = usz(shoff + delta + i * SHSZ);
        if i == bun {
            w64(data, o + 0x10, new_vaddr);
            w64(data, o + 0x18, new_off);
            w64(data, o + 0x20, content_size);
        } else if r32(data, o + 4) != sht::NOBITS && r64(data, o + 0x18) >= old_end {
            let cur = r64(data, o + 0x18);
            w64(data, o + 0x18, cur + delta);
        }
    }
    let seg_size = (new_vaddr - rw_vaddr) + aligned_size;
    w64(data, rw + 0x20, seg_size);
    w64(data, rw + 0x28, seg_size);
}

#[test]
fn patched_bun_template_still_compiles_and_runs() {
    if !zig_available() {
        return;
    }
    let src = sample("exe-bun-le");
    assert_runs(&src);

    // Same interpreter under a longer path — the shape auto-formatelf produces.
    let interp = out(ours(), "--print-interpreter", &src);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("bun-loaderdir-with-a-long-name");
    std::fs::create_dir_all(&dir).unwrap();
    let link = dir.join("ld-linux-renamed-long.so.2");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(interp.trim(), &link).unwrap();

    let bin = copy("exe-bun-le", "patched");
    patch(
        &bin,
        &[
            "--set-interpreter",
            link.to_str().unwrap(),
            "--set-rpath",
            "/nix/store/00000000000000000000000000000000-glibc-2.40/lib",
        ],
    );
    assert_dynstr_synced(&bin);
    assert_bun_template_invariants(&bin);
    assert_runs(&bin);

    let mut compiled = std::fs::read(&bin).unwrap();
    bun_write_section(&mut compiled, b"console.log('hello from the payload')\n");
    let out_path = bin.with_extension("compiled");
    std::fs::write(&out_path, &compiled).unwrap();
    let mut perms = std::fs::metadata(&out_path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&out_path, perms).unwrap();
    assert_runs(&out_path);
}
