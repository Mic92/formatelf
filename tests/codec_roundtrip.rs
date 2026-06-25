//! Re-encoding every fixed-size ELF struct must reproduce the original bytes.
//! This guards the highest-risk part of the rewrite: the class/endianness codec
//! and the Elf32/Elf64 field-reorder handling.

mod fixtures;

use patchelf_rs::codec;
use patchelf_rs::parser;

fn check(path: &std::path::Path) {
    let data = std::fs::read(path).unwrap();
    let img = parser::parse(&data).unwrap_or_else(|e| panic!("parse {path:?}: {e}"));

    // Ehdr: re-encode with the original table counts and compare.
    let mut buf = Vec::new();
    codec::write_ehdr(
        img.enc,
        &img.ehdr,
        img.phdrs.len() as u16,
        img.shdrs.len() as u16,
        &mut buf,
    )
    .unwrap();
    assert_eq!(buf, &data[..buf.len()], "ehdr mismatch for {path:?}");

    let phsize = codec::phdr_size(img.enc.class);
    for (i, p) in img.phdrs.iter().enumerate() {
        let off = img.ehdr.phoff as usize + i * phsize;
        let mut buf = Vec::new();
        codec::write_phdr(img.enc, p, &mut buf).unwrap();
        assert_eq!(
            buf,
            &data[off..off + phsize],
            "phdr {i} mismatch for {path:?}"
        );
    }

    let shsize = codec::shdr_size(img.enc.class);
    for (i, s) in img.shdrs.iter().enumerate() {
        let off = img.ehdr.shoff as usize + i * shsize;
        let mut buf = Vec::new();
        codec::write_shdr(img.enc, s, &mut buf).unwrap();
        assert_eq!(
            buf,
            &data[off..off + shsize],
            "shdr {i} mismatch for {path:?}"
        );
    }

    // Dynamic entries round-trip back into the .dynamic section bytes.
    if !img.dynamic.is_empty() {
        let mut buf = Vec::new();
        for d in &img.dynamic {
            codec::write_dyn(img.enc, d, &mut buf).unwrap();
        }
        let dyn_sh = img
            .shdrs
            .iter()
            .find(|s| s.sh_type == 6) // SHT_DYNAMIC
            .expect("dynamic section present");
        let off = dyn_sh.offset as usize;
        assert_eq!(
            buf,
            &data[off..off + buf.len()],
            "dynamic mismatch for {path:?}"
        );
    }
}

#[test]
fn roundtrip_all_fixtures() {
    if !fixtures::zig_available() {
        eprintln!("skipping: zig not on PATH");
        return;
    }
    for path in fixtures::samples() {
        check(&path);
    }
}
