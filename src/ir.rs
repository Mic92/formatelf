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
    pub os_abi: u8,
    pub abi_version: u8,
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
