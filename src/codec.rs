//! Single place mapping the native IR to/from `object` raw ELF structs. Keeping
//! read and write symmetric here prevents the Elf32/Elf64 field-reorder and
//! width differences from drifting between parser and serializer.

use object::elf;
use object::{Endianness, U16, U32, U64};

use crate::error::{Error, Result};
use crate::ir::{Class, DynEntry, Ehdr, Encoding, Endian, Phdr, Shdr};

/// Placeholder e_ident; the real 16 bytes are copied over verbatim afterwards.
fn zero_ident() -> elf::Ident {
    elf::Ident {
        magic: [0; 4],
        class: 0,
        data: 0,
        version: 0,
        os_abi: 0,
        abi_version: 0,
        padding: [0; 7],
    }
}

pub fn endianness(e: Endian) -> Endianness {
    match e {
        Endian::Little => Endianness::Little,
        Endian::Big => Endianness::Big,
    }
}

fn pod<T: object::Pod>(data: &[u8]) -> Result<&T> {
    object::from_bytes(data)
        .map(|(v, _)| v)
        .map_err(|_| Error::Parse("truncated struct".into()))
}

/// Narrow a widened IR value to a 32-bit on-disk field, refusing to silently
/// truncate: a computed offset/addr/size that no longer fits an Elf32 file is a
/// hard error, not a corrupt write.
fn narrow(v: u64, field: &str) -> Result<u32> {
    u32::try_from(v)
        .map_err(|_| Error::Serialize(format!("{field} value {v:#x} overflows 32-bit ELF field")))
}

/// Returns the decoded header, its encoding, and the raw `(e_phnum, e_shnum)`
/// table counts (needed by the parser; not stored in the IR since the
/// serializer derives them from vector lengths).
pub fn read_ehdr(data: &[u8]) -> Result<(Ehdr, Encoding, u16, u16)> {
    if data.len() < 16 || &data[..4] != b"\x7fELF" {
        return Err(Error::Parse("bad ELF magic".into()));
    }
    let class = match data[4] {
        1 => Class::Elf32,
        2 => Class::Elf64,
        c => return Err(Error::Unsupported(format!("EI_CLASS {c}"))),
    };
    let endian = match data[5] {
        1 => Endian::Little,
        2 => Endian::Big,
        d => return Err(Error::Unsupported(format!("EI_DATA {d}"))),
    };
    let e = endianness(endian);
    let mut ident = [0u8; 16];
    ident.copy_from_slice(&data[..16]);
    let (ehdr, phnum, shnum) = match class {
        Class::Elf64 => {
            let h: &elf::FileHeader64<Endianness> = pod(data)?;
            let ehdr = Ehdr {
                e_type: h.e_type.get(e),
                machine: h.e_machine.get(e),
                version: h.e_version.get(e),
                entry: h.e_entry.get(e),
                phoff: h.e_phoff.get(e),
                shoff: h.e_shoff.get(e),
                flags: h.e_flags.get(e),
                ehsize: h.e_ehsize.get(e),
                phentsize: h.e_phentsize.get(e),
                shentsize: h.e_shentsize.get(e),
                shstrndx: h.e_shstrndx.get(e) as u32,
                os_abi: data[7],
                abi_version: data[8],
                ident,
            };
            (ehdr, h.e_phnum.get(e), h.e_shnum.get(e))
        }
        Class::Elf32 => {
            let h: &elf::FileHeader32<Endianness> = pod(data)?;
            let ehdr = Ehdr {
                e_type: h.e_type.get(e),
                machine: h.e_machine.get(e),
                version: h.e_version.get(e),
                entry: h.e_entry.get(e) as u64,
                phoff: h.e_phoff.get(e) as u64,
                shoff: h.e_shoff.get(e) as u64,
                flags: h.e_flags.get(e),
                ehsize: h.e_ehsize.get(e),
                phentsize: h.e_phentsize.get(e),
                shentsize: h.e_shentsize.get(e),
                shstrndx: h.e_shstrndx.get(e) as u32,
                os_abi: data[7],
                abi_version: data[8],
                ident,
            };
            (ehdr, h.e_phnum.get(e), h.e_shnum.get(e))
        }
    };

    Ok((ehdr, Encoding { class, endian }, phnum, shnum))
}

/// e_phnum == PN_XNUM signals the real count lives in section 0's sh_info.
pub const PN_XNUM: u16 = 0xffff;
/// First reserved section index; counts at/above it use the section-0 escape.
pub const SHN_LORESERVE: u32 = 0xff00;
/// e_shstrndx == SHN_XINDEX signals the real index lives in section 0's sh_link.
pub const SHN_XINDEX: u16 = 0xffff;

/// Compute the on-disk header count fields, substituting the section-0 escapes
/// when the real counts exceed what the 16-bit fields can hold. The extended
/// values themselves live in section 0's header, preserved across round-trip.
fn ehdr_counts(h: &Ehdr, phnum: usize, shnum: usize) -> (u16, u16, u16) {
    let e_phnum = if phnum >= PN_XNUM as usize {
        PN_XNUM
    } else {
        phnum as u16
    };
    let e_shnum = if shnum >= SHN_LORESERVE as usize {
        0
    } else {
        shnum as u16
    };
    let e_shstrndx = if h.shstrndx >= SHN_LORESERVE {
        SHN_XINDEX
    } else {
        h.shstrndx as u16
    };
    (e_phnum, e_shnum, e_shstrndx)
}

pub fn write_ehdr(
    enc: Encoding,
    h: &Ehdr,
    phnum: usize,
    shnum: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    let e = endianness(enc.endian);
    let (phnum, shnum, shstrndx) = ehdr_counts(h, phnum, shnum);
    match enc.class {
        Class::Elf64 => {
            let hdr = elf::FileHeader64::<Endianness> {
                e_ident: zero_ident(),
                e_type: U16::new(e, h.e_type),
                e_machine: U16::new(e, h.machine),
                e_version: U32::new(e, h.version),
                e_entry: U64::new(e, h.entry),
                e_phoff: U64::new(e, h.phoff),
                e_shoff: U64::new(e, h.shoff),
                e_flags: U32::new(e, h.flags),
                e_ehsize: U16::new(e, h.ehsize),
                e_phentsize: U16::new(e, h.phentsize),
                e_phnum: U16::new(e, phnum),
                e_shentsize: U16::new(e, h.shentsize),
                e_shnum: U16::new(e, shnum),
                e_shstrndx: U16::new(e, shstrndx),
            };
            let mut bytes = object::bytes_of(&hdr).to_vec();
            bytes[..16].copy_from_slice(&h.ident);
            out.extend_from_slice(&bytes);
        }
        Class::Elf32 => {
            let hdr = elf::FileHeader32::<Endianness> {
                e_ident: zero_ident(),
                e_type: U16::new(e, h.e_type),
                e_machine: U16::new(e, h.machine),
                e_version: U32::new(e, h.version),
                e_entry: U32::new(e, narrow(h.entry, "e_entry")?),
                e_phoff: U32::new(e, narrow(h.phoff, "e_phoff")?),
                e_shoff: U32::new(e, narrow(h.shoff, "e_shoff")?),
                e_flags: U32::new(e, h.flags),
                e_ehsize: U16::new(e, h.ehsize),
                e_phentsize: U16::new(e, h.phentsize),
                e_phnum: U16::new(e, phnum),
                e_shentsize: U16::new(e, h.shentsize),
                e_shnum: U16::new(e, shnum),
                e_shstrndx: U16::new(e, shstrndx),
            };
            let mut bytes = object::bytes_of(&hdr).to_vec();
            bytes[..16].copy_from_slice(&h.ident);
            out.extend_from_slice(&bytes);
        }
    }
    Ok(())
}

pub fn read_phdr(enc: Encoding, data: &[u8]) -> Result<Phdr> {
    let e = endianness(enc.endian);
    match enc.class {
        Class::Elf64 => {
            let p: &elf::ProgramHeader64<Endianness> = pod(data)?;
            Ok(Phdr {
                p_type: p.p_type.get(e),
                flags: p.p_flags.get(e),
                offset: p.p_offset.get(e),
                vaddr: p.p_vaddr.get(e),
                paddr: p.p_paddr.get(e),
                filesz: p.p_filesz.get(e),
                memsz: p.p_memsz.get(e),
                align: p.p_align.get(e),
            })
        }
        Class::Elf32 => {
            let p: &elf::ProgramHeader32<Endianness> = pod(data)?;
            Ok(Phdr {
                p_type: p.p_type.get(e),
                flags: p.p_flags.get(e),
                offset: p.p_offset.get(e) as u64,
                vaddr: p.p_vaddr.get(e) as u64,
                paddr: p.p_paddr.get(e) as u64,
                filesz: p.p_filesz.get(e) as u64,
                memsz: p.p_memsz.get(e) as u64,
                align: p.p_align.get(e) as u64,
            })
        }
    }
}

pub fn write_phdr(enc: Encoding, p: &Phdr, out: &mut Vec<u8>) -> Result<()> {
    let e = endianness(enc.endian);
    match enc.class {
        Class::Elf64 => {
            let h = elf::ProgramHeader64::<Endianness> {
                p_type: U32::new(e, p.p_type),
                p_flags: U32::new(e, p.flags),
                p_offset: U64::new(e, p.offset),
                p_vaddr: U64::new(e, p.vaddr),
                p_paddr: U64::new(e, p.paddr),
                p_filesz: U64::new(e, p.filesz),
                p_memsz: U64::new(e, p.memsz),
                p_align: U64::new(e, p.align),
            };
            out.extend_from_slice(object::bytes_of(&h));
        }
        Class::Elf32 => {
            let h = elf::ProgramHeader32::<Endianness> {
                p_type: U32::new(e, p.p_type),
                p_offset: U32::new(e, narrow(p.offset, "p_offset")?),
                p_vaddr: U32::new(e, narrow(p.vaddr, "p_vaddr")?),
                p_paddr: U32::new(e, narrow(p.paddr, "p_paddr")?),
                p_filesz: U32::new(e, narrow(p.filesz, "p_filesz")?),
                p_memsz: U32::new(e, narrow(p.memsz, "p_memsz")?),
                p_flags: U32::new(e, p.flags),
                p_align: U32::new(e, narrow(p.align, "p_align")?),
            };
            out.extend_from_slice(object::bytes_of(&h));
        }
    }
    Ok(())
}

pub fn read_shdr(enc: Encoding, data: &[u8]) -> Result<Shdr> {
    let e = endianness(enc.endian);
    match enc.class {
        Class::Elf64 => {
            let s: &elf::SectionHeader64<Endianness> = pod(data)?;
            Ok(Shdr {
                name: s.sh_name.get(e),
                sh_type: s.sh_type.get(e),
                flags: s.sh_flags.get(e),
                addr: s.sh_addr.get(e),
                offset: s.sh_offset.get(e),
                size: s.sh_size.get(e),
                link: s.sh_link.get(e),
                info: s.sh_info.get(e),
                addralign: s.sh_addralign.get(e),
                entsize: s.sh_entsize.get(e),
            })
        }
        Class::Elf32 => {
            let s: &elf::SectionHeader32<Endianness> = pod(data)?;
            Ok(Shdr {
                name: s.sh_name.get(e),
                sh_type: s.sh_type.get(e),
                flags: s.sh_flags.get(e) as u64,
                addr: s.sh_addr.get(e) as u64,
                offset: s.sh_offset.get(e) as u64,
                size: s.sh_size.get(e) as u64,
                link: s.sh_link.get(e),
                info: s.sh_info.get(e),
                addralign: s.sh_addralign.get(e) as u64,
                entsize: s.sh_entsize.get(e) as u64,
            })
        }
    }
}

pub fn write_shdr(enc: Encoding, s: &Shdr, out: &mut Vec<u8>) -> Result<()> {
    let e = endianness(enc.endian);
    match enc.class {
        Class::Elf64 => {
            let h = elf::SectionHeader64::<Endianness> {
                sh_name: U32::new(e, s.name),
                sh_type: U32::new(e, s.sh_type),
                sh_flags: U64::new(e, s.flags),
                sh_addr: U64::new(e, s.addr),
                sh_offset: U64::new(e, s.offset),
                sh_size: U64::new(e, s.size),
                sh_link: U32::new(e, s.link),
                sh_info: U32::new(e, s.info),
                sh_addralign: U64::new(e, s.addralign),
                sh_entsize: U64::new(e, s.entsize),
            };
            out.extend_from_slice(object::bytes_of(&h));
        }
        Class::Elf32 => {
            let h = elf::SectionHeader32::<Endianness> {
                sh_name: U32::new(e, s.name),
                sh_type: U32::new(e, s.sh_type),
                sh_flags: U32::new(e, narrow(s.flags, "sh_flags")?),
                sh_addr: U32::new(e, narrow(s.addr, "sh_addr")?),
                sh_offset: U32::new(e, narrow(s.offset, "sh_offset")?),
                sh_size: U32::new(e, narrow(s.size, "sh_size")?),
                sh_link: U32::new(e, s.link),
                sh_info: U32::new(e, s.info),
                sh_addralign: U32::new(e, narrow(s.addralign, "sh_addralign")?),
                sh_entsize: U32::new(e, narrow(s.entsize, "sh_entsize")?),
            };
            out.extend_from_slice(object::bytes_of(&h));
        }
    }
    Ok(())
}

pub fn read_dyn(enc: Encoding, data: &[u8]) -> Result<DynEntry> {
    let e = endianness(enc.endian);
    match enc.class {
        Class::Elf64 => {
            let d: &elf::Dyn64<Endianness> = pod(data)?;
            Ok(DynEntry {
                tag: d.d_tag.get(e) as i64,
                val: d.d_val.get(e),
            })
        }
        Class::Elf32 => {
            let d: &elf::Dyn32<Endianness> = pod(data)?;
            Ok(DynEntry {
                tag: d.d_tag.get(e) as i32 as i64,
                val: d.d_val.get(e) as u64,
            })
        }
    }
}

pub fn write_dyn(enc: Encoding, d: &DynEntry, out: &mut Vec<u8>) -> Result<()> {
    let e = endianness(enc.endian);
    match enc.class {
        Class::Elf64 => {
            let h = elf::Dyn64::<Endianness> {
                d_tag: U64::new(e, d.tag as u64),
                d_val: U64::new(e, d.val),
            };
            out.extend_from_slice(object::bytes_of(&h));
        }
        Class::Elf32 => {
            // d_tag is Elf32_Sword: the standard tags use its 32-bit pattern, so
            // truncation is intentional. Only d_val carries addresses/sizes.
            let h = elf::Dyn32::<Endianness> {
                d_tag: U32::new(e, d.tag as u32),
                d_val: U32::new(e, narrow(d.val, "d_val")?),
            };
            out.extend_from_slice(object::bytes_of(&h));
        }
    }
    Ok(())
}

pub fn phdr_size(class: Class) -> usize {
    match class {
        Class::Elf64 => core::mem::size_of::<elf::ProgramHeader64<Endianness>>(),
        Class::Elf32 => core::mem::size_of::<elf::ProgramHeader32<Endianness>>(),
    }
}

pub fn shdr_size(class: Class) -> usize {
    match class {
        Class::Elf64 => core::mem::size_of::<elf::SectionHeader64<Endianness>>(),
        Class::Elf32 => core::mem::size_of::<elf::SectionHeader32<Endianness>>(),
    }
}

pub fn dyn_size(class: Class) -> usize {
    match class {
        Class::Elf64 => core::mem::size_of::<elf::Dyn64<Endianness>>(),
        Class::Elf32 => core::mem::size_of::<elf::Dyn32<Endianness>>(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Shdr;

    #[test]
    fn extended_counts_use_section0_escapes() {
        let enc = Encoding {
            class: Class::Elf64,
            endian: Endian::Little,
        };
        let mut ident = [0u8; 16];
        ident[..4].copy_from_slice(b"\x7fELF");
        ident[4] = 2; // ELFCLASS64
        ident[5] = 1; // little-endian
        let h = Ehdr {
            e_type: 2,
            machine: 62,
            version: 1,
            entry: 0,
            phoff: 0,
            shoff: 64,
            flags: 0,
            ehsize: 64,
            phentsize: 56,
            shentsize: 64,
            shstrndx: 0x1_0000,
            os_abi: 0,
            abi_version: 0,
            ident,
        };
        let mut out = Vec::new();
        write_ehdr(enc, &h, 0x1_0000, 0x1_0000, &mut out).unwrap();
        // read_ehdr returns the raw on-disk fields, which must carry the escapes.
        let (got, _, phnum, shnum) = read_ehdr(&out).unwrap();
        assert_eq!(phnum, PN_XNUM);
        assert_eq!(shnum, 0);
        assert_eq!(got.shstrndx, SHN_XINDEX as u32);
    }

    #[test]
    fn elf32_rejects_oversized_offset() {
        let enc = Encoding {
            class: Class::Elf32,
            endian: Endian::Little,
        };
        let s = Shdr {
            name: 0,
            sh_type: 1,
            flags: 0,
            addr: 0,
            offset: 0x1_0000_0000, // > u32::MAX
            size: 0,
            link: 0,
            info: 0,
            addralign: 0,
            entsize: 0,
        };
        let mut out = Vec::new();
        assert!(write_shdr(enc, &s, &mut out).is_err());
    }
}
