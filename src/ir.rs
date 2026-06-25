//! Arch-agnostic ELF model: fields widened to 64-bit, native endianness.
//! Class/byte order recorded only so the serializer can re-encode.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Elf32,
    Elf64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

#[derive(Debug, Clone, Copy)]
pub struct Encoding {
    pub class: Class,
    pub endian: Endian,
}

#[derive(Debug, Clone)]
pub struct Ehdr {
    pub e_type: u16,
    pub machine: u16,
    pub version: u32,
    pub entry: u64,
    pub phoff: u64,
    pub shoff: u64,
    pub flags: u32,
    pub ehsize: u16,
    pub phentsize: u16,
    pub shentsize: u16,
    pub shstrndx: u16,
    pub os_abi: u8,
    pub abi_version: u8,
    /// First 16 bytes verbatim; preserves padding/OS-specific bytes on re-encode.
    pub ident: [u8; 16],
}

#[derive(Debug, Clone)]
pub struct Phdr {
    pub p_type: u32,
    pub flags: u32,
    pub offset: u64,
    pub vaddr: u64,
    pub paddr: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
}

#[derive(Debug, Clone)]
pub struct Shdr {
    pub name: u32,
    pub sh_type: u32,
    pub flags: u64,
    pub addr: u64,
    pub offset: u64,
    pub size: u64,
    pub link: u32,
    pub info: u32,
    pub addralign: u64,
    pub entsize: u64,
}

#[derive(Debug, Clone)]
pub struct DynEntry {
    pub tag: i64,
    pub val: u64,
}

pub mod sht {
    pub const STRTAB: u32 = 3;
    pub const DYNAMIC: u32 = 6;
    pub const NOBITS: u32 = 8;
}

pub mod et {
    pub const EXEC: u16 = 2;
    pub const DYN: u16 = 3;
}

pub mod sht_extra {
    pub const NOTE: u32 = 7;
}

pub mod pt {
    pub const LOAD: u32 = 1;
    pub const DYNAMIC: u32 = 2;
    pub const INTERP: u32 = 3;
    pub const PHDR: u32 = 6;
    pub const GNU_STACK: u32 = 0x6474_e551;
}

pub mod pf {
    pub const X: u32 = 1;
    pub const W: u32 = 2;
    pub const R: u32 = 4;
}

pub mod dt {
    pub const NULL: i64 = 0;
    pub const NEEDED: i64 = 1;
    pub const HASH: i64 = 4;
    pub const STRTAB: i64 = 5;
    pub const SYMTAB: i64 = 6;
    pub const RELA: i64 = 7;
    pub const STRSZ: i64 = 10;
    pub const SONAME: i64 = 14;
    pub const RPATH: i64 = 15;
    pub const REL: i64 = 17;
    pub const JMPREL: i64 = 23;
    pub const RUNPATH: i64 = 29;
    pub const GNU_HASH: i64 = 0x6fff_fef5;
    pub const VERNEED: i64 = 0x6fff_fffe;
    pub const VERSYM: i64 = 0x6fff_fff0;
}

/// Section contents owned separately so ops can grow them without fighting
/// on-disk offsets; the layout engine assigns final offsets later.
#[derive(Debug, Clone)]
pub struct ElfImage {
    pub enc: Encoding,
    pub ehdr: Ehdr,
    pub phdrs: Vec<Phdr>,
    pub shdrs: Vec<Shdr>,
    /// Parallel to `shdrs`.
    pub section_data: Vec<Vec<u8>>,
    pub dynamic: Vec<DynEntry>,
}

/// Read a NUL-terminated string starting at `off` in a string table.
pub fn cstr(tab: &[u8], off: u32) -> Option<&str> {
    let tab = tab.get(off as usize..)?;
    let end = tab.iter().position(|&b| b == 0).unwrap_or(tab.len());
    std::str::from_utf8(&tab[..end]).ok()
}

impl ElfImage {
    pub fn section_name(&self, idx: usize) -> Option<&str> {
        let strtab = self.section_data.get(self.ehdr.shstrndx as usize)?;
        cstr(strtab, self.shdrs.get(idx)?.name)
    }

    pub fn find_section(&self, name: &str) -> Option<usize> {
        (0..self.shdrs.len()).find(|&i| self.section_name(i) == Some(name))
    }

    pub fn has_dynamic(&self) -> bool {
        self.shdrs.iter().any(|s| s.sh_type == sht::DYNAMIC)
    }

    /// Bytes of the string table linked from the `.dynamic` section.
    pub fn dynstr(&self) -> Option<&[u8]> {
        let dyn_idx = self.shdrs.iter().position(|s| s.sh_type == sht::DYNAMIC)?;
        let link = self.shdrs[dyn_idx].link as usize;
        let s = self.shdrs.get(link)?;
        if s.sh_type != sht::STRTAB {
            return None;
        }
        self.section_data.get(link).map(Vec::as_slice)
    }
}
