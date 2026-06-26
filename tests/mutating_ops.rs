//! Growing mutating ops validated against the reference patchelf, plus
//! execution checks for the cases that stay loadable.

mod fixtures;

use fixtures::{assert_dynstr_synced, copy, guard, out, patch, sample};
use std::path::Path;
use std::process::Command;

#[test]
fn set_interpreter_grows_and_runs() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    assert!(Command::new(&src).status().unwrap().success());

    // Symlink the working loader under a longer path so the grown interpreter
    // still resolves and the binary remains runnable.
    let interp = out(&reference, "--print-interpreter", &src);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("loaderdir-with-a-long-name");
    std::fs::create_dir_all(&dir).unwrap();
    let link = dir.join("ld-linux-renamed-long.so.2");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(interp.trim(), &link).unwrap();

    let bin = copy("exe-dyn-le", "interp");
    let want = link.to_str().unwrap();
    patch(&bin, &["--set-interpreter", want]);

    assert_eq!(out(&reference, "--print-interpreter", &bin).trim(), want);
    assert!(
        Command::new(&bin).status().unwrap().success(),
        "patched binary failed to run"
    );
}

#[test]
fn set_soname_on_shared_object() {
    let Some(reference) = guard() else { return };
    let lib = copy("so-soname-le", "soname");
    patch(
        &lib,
        &["--set-soname", "librenamed-with-a-much-longer-name.so.7"],
    );
    assert_eq!(
        out(&reference, "--print-soname", &lib).trim(),
        "librenamed-with-a-much-longer-name.so.7"
    );
}

#[test]
fn add_then_remove_needed_round_trips() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    let before = out(&reference, "--print-needed", &src);

    let bin = copy("exe-dyn-le", "needed");
    patch(&bin, &["--add-needed", "libextra-placeholder.so.9"]);
    let after_add = out(&reference, "--print-needed", &bin);
    assert!(
        after_add.lines().any(|l| l == "libextra-placeholder.so.9"),
        "added lib missing: {after_add:?}"
    );

    patch(&bin, &["--remove-needed", "libextra-placeholder.so.9"]);
    assert_eq!(out(&reference, "--print-needed", &bin), before);
    // The bogus entry is gone, so the binary loads again.
    assert!(Command::new(&bin).status().unwrap().success());
}

#[test]
fn replace_needed_changes_entry() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    let needed = out(&reference, "--print-needed", &src);
    let first = needed.lines().next().unwrap();

    let bin = copy("exe-dyn-le", "replace");
    patch(
        &bin,
        &["--replace-needed", first, "libreplacement-longer-name.so.3"],
    );
    let after = out(&reference, "--print-needed", &bin);
    assert!(after
        .lines()
        .any(|l| l == "libreplacement-longer-name.so.3"));
    assert!(!after.lines().any(|l| l == first));
}

#[test]
fn remove_rpath_clears_runpath_keeps_needed() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    assert_eq!(
        out(&reference, "--print-rpath", &src).trim(),
        "/opt/custom/lib"
    );
    let needed = out(&reference, "--print-needed", &src);

    let bin = copy("exe-dyn-le", "norpath");
    patch(&bin, &["--remove-rpath"]);
    assert_eq!(out(&reference, "--print-rpath", &bin).trim(), "");
    assert_eq!(out(&reference, "--print-needed", &bin), needed);
}

#[test]
fn set_rpath_grows_and_stays_loadable() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    assert!(Command::new(&src).status().unwrap().success());
    let needed = out(&reference, "--print-needed", &src);
    let interp = out(&reference, "--print-interpreter", &src);

    let bin = copy("exe-dyn-le", "rpath");
    let long = "/a/very/long/custom/runpath/that/will/not/fit/in/place/lib";
    patch(&bin, &["--set-rpath", long]);

    assert_eq!(out(&reference, "--print-rpath", &bin).trim(), long);
    assert_eq!(out(&reference, "--print-needed", &bin), needed);
    assert_eq!(out(&reference, "--print-interpreter", &bin), interp);
    assert_dynstr_synced(&bin);
    assert!(
        Command::new(&bin).status().unwrap().success(),
        "patched binary failed to run"
    );
}

#[test]
fn set_rpath_on_non_pie_executable() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-nopie-le");
    assert!(Command::new(&src).status().unwrap().success());

    let bin = copy("exe-nopie-le", "rpath");
    let long = "/a/much/longer/rpath/than/before/for/the/exec/path/lib";
    patch(&bin, &["--set-rpath", long]);

    assert_eq!(out(&reference, "--print-rpath", &bin).trim(), long);
    assert_dynstr_synced(&bin);
    assert!(
        Command::new(&bin).status().unwrap().success(),
        "patched ET_EXEC failed to run"
    );
}

#[test]
fn add_rpath_appends_to_existing() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    let before = out(&reference, "--print-rpath", &src);
    let before = before.trim();

    let bin = copy("exe-dyn-le", "addrpath");
    patch(&bin, &["--add-rpath", "/new/dir/here"]);
    assert_eq!(
        out(&reference, "--print-rpath", &bin).trim(),
        format!("{before}:/new/dir/here")
    );
    assert!(Command::new(&bin).status().unwrap().success());
}

#[test]
fn force_rpath_uses_dt_rpath_tag() {
    let Some(reference) = guard() else { return };
    let bin = copy("exe-dyn-le", "forcerpath");
    patch(
        &bin,
        &["--force-rpath", "--set-rpath", "/forced/path/longlonglong"],
    );

    assert_eq!(
        out(&reference, "--print-rpath", &bin).trim(),
        "/forced/path/longlonglong"
    );
    let img = formatelf::parser::parse(&std::fs::read(&bin).unwrap()).unwrap();
    assert!(
        img.dynamic
            .iter()
            .any(|d| d.tag == formatelf::ir::dt::RPATH),
        "expected DT_RPATH after --force-rpath"
    );
    assert!(
        !img.dynamic
            .iter()
            .any(|d| d.tag == formatelf::ir::dt::RUNPATH),
        "DT_RUNPATH should have been converted"
    );
    assert!(Command::new(&bin).status().unwrap().success());
}

#[test]
fn set_os_abi_changes_ident() {
    let Some(reference) = guard() else { return };
    let bin = copy("exe-dyn-le", "osabi");
    patch(&bin, &["--set-os-abi", "freebsd"]);
    assert_eq!(out(&reference, "--print-os-abi", &bin).trim(), "FreeBSD");
}

#[test]
fn execstack_set_then_clear() {
    let Some(reference) = guard() else { return };
    let bin = copy("exe-dyn-le", "execstack");

    patch(&bin, &["--set-execstack"]);
    assert_eq!(
        out(&reference, "--print-execstack", &bin).trim(),
        "execstack: X"
    );
    assert!(Command::new(&bin).status().unwrap().success());

    patch(&bin, &["--clear-execstack"]);
    assert_eq!(
        out(&reference, "--print-execstack", &bin).trim(),
        "execstack: -"
    );
    assert!(Command::new(&bin).status().unwrap().success());
}

#[test]
fn no_default_lib_sets_flag() {
    if !fixtures::zig_available() {
        eprintln!("skipping: zig not on PATH");
        return;
    }
    let bin = copy("exe-dyn-le", "nodeflib");
    patch(&bin, &["--no-default-lib"]);
    let img = formatelf::parser::parse(&std::fs::read(&bin).unwrap()).unwrap();
    let flags1 = img
        .dynamic
        .iter()
        .find(|d| d.tag == formatelf::ir::dt::FLAGS_1)
        .expect("DT_FLAGS_1 present");
    assert_ne!(flags1.val & formatelf::ir::df1::NODEFLIB, 0);
}

#[test]
fn add_debug_tag_inserts_exactly_one() {
    use formatelf::ir::dt;
    if !fixtures::zig_available() {
        eprintln!("skipping: zig not on PATH");
        return;
    }
    // The fixture already has a DT_DEBUG; strip it, then applying the op twice
    // must leave exactly one.
    let orig = std::fs::read(sample("exe-dyn-le")).unwrap();
    let mut img = formatelf::parser::parse(&orig).unwrap();
    img.dynamic.retain(|d| d.tag != dt::DEBUG);

    let mut report = formatelf::ops::Report { lines: vec![] };
    for _ in 0..2 {
        formatelf::ops::apply(
            &mut img,
            &formatelf::cli::Operation::AddDebugTag,
            &formatelf::ops::Modifiers::default(),
            &mut report,
        )
        .unwrap();
    }
    assert_eq!(img.dynamic.iter().filter(|d| d.tag == dt::DEBUG).count(), 1);
}

#[test]
fn shrink_rpath_drops_useless_dirs() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    let first_needed = out(&reference, "--print-needed", &src)
        .lines()
        .next()
        .unwrap()
        .to_owned();

    // A directory that holds one of the needed libs (any matching-machine ELF
    // works) is kept; an empty directory is dropped.
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let good = tmp.join("rpath-good");
    let bad = tmp.join("rpath-bad");
    std::fs::create_dir_all(&good).unwrap();
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::copy(&src, good.join(&first_needed)).unwrap();

    let bin = copy("exe-dyn-le", "shrink");
    let rpath = format!("{}:{}", good.display(), bad.display());
    patch(&bin, &["--set-rpath", &rpath]);
    patch(&bin, &["--shrink-rpath"]);

    assert_eq!(
        out(&reference, "--print-rpath", &bin).trim(),
        good.to_str().unwrap()
    );
    // Not executed: the placeholder libc in the kept dir would shadow the real
    // one at load time. Shrink correctness is checked via the readback above.
}

#[test]
fn clear_symbol_version_matches_reference() {
    let Some(reference) = guard() else { return };
    let bin_ours = copy("exe-dyn-le", "clearver-ours");
    let bin_ref = copy("exe-dyn-le", "clearver-ref");

    patch(&bin_ours, &["--clear-symbol-version", "abort"]);
    let r = Command::new(&reference)
        .args(["--clear-symbol-version", "abort"])
        .arg(&bin_ref)
        .status()
        .unwrap();
    assert!(r.success());

    // The .gnu.version arrays must match byte-for-byte.
    let a = formatelf::parser::parse(&std::fs::read(&bin_ours).unwrap()).unwrap();
    let b = formatelf::parser::parse(&std::fs::read(&bin_ref).unwrap()).unwrap();
    let ai = a.find_section(".gnu.version").unwrap();
    let bi = b.find_section(".gnu.version").unwrap();
    assert_eq!(a.section_data[ai], b.section_data[bi]);
    assert!(Command::new(&bin_ours).status().unwrap().success());
}

#[test]
fn print_needed_works_without_section_headers() {
    if !fixtures::zig_available() {
        return;
    }
    // Strip the section header table (zero e_shoff/e_shnum/e_shstrndx) from an
    // ELF64 LE shared object; the dynamic info must still be read from segments.
    let mut data = std::fs::read(sample("so-aarch64-le")).unwrap();
    data[0x28..0x30].fill(0); // e_shoff
    data[0x3c..0x3e].fill(0); // e_shnum
    data[0x3e..0x40].fill(0); // e_shstrndx
    let bin = Path::new(env!("CARGO_TARGET_TMPDIR")).join("no-shdr.so");
    std::fs::write(&bin, &data).unwrap();

    let img = formatelf::parser::parse(&std::fs::read(&bin).unwrap()).unwrap();
    assert!(img.shdrs.is_empty(), "section headers should be gone");

    let out = Command::new(env!("CARGO_BIN_EXE_formatelf"))
        .args(["--print-needed"])
        .arg(&bin)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let needed = String::from_utf8(out.stdout).unwrap();
    assert!(
        needed.lines().any(|n| n.starts_with("libc")),
        "got {needed:?}"
    );
}

#[test]
fn print_interpreter_works_without_section_headers() {
    if !fixtures::zig_available() {
        return;
    }
    // Strip the section header table from a dynamic executable; the
    // interpreter must still be read from PT_INTERP. Reference patchelf
    // refuses such a file outright.
    let mut data = std::fs::read(sample("exe-dyn-le")).unwrap();
    data[0x28..0x30].fill(0); // e_shoff
    data[0x3c..0x3e].fill(0); // e_shnum
    data[0x3e..0x40].fill(0); // e_shstrndx
    let bin = Path::new(env!("CARGO_TARGET_TMPDIR")).join("no-shdr-exe");
    std::fs::write(&bin, &data).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_formatelf"))
        .args(["--print-interpreter"])
        .arg(&bin)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8(out.stdout).unwrap().contains("ld-linux"),
        "expected an ld.so path"
    );
}

#[test]
fn repeated_patching_reuses_the_appended_region() {
    let Some(_reference) = guard() else { return };
    let bin = copy("exe-dyn-le", "coalesce");

    let load_count = |p: &Path| {
        let img = formatelf::parser::parse(&std::fs::read(p).unwrap()).unwrap();
        img.phdrs
            .iter()
            .filter(|h| h.p_type == formatelf::ir::pt::LOAD)
            .count()
    };

    patch(
        &bin,
        &["--set-rpath", "/opt/first/aaaaaaaaaaaaaaaaaaaaaaaa/bbbb"],
    );
    let after_first = load_count(&bin);
    for n in 0..4 {
        patch(
            &bin,
            &[
                "--set-rpath",
                &format!("/opt/run{n}/cccccccccccccccccccc/dddd"),
            ],
        );
        assert_eq!(
            load_count(&bin),
            after_first,
            "added a PT_LOAD on re-patch {n}"
        );
    }
    assert!(Command::new(&bin).status().unwrap().success());
}

#[test]
fn rename_dynamic_symbols_matches_reference() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");

    // First dynamic symbol with a plain name, found via our own parser.
    let img = formatelf::parser::parse(&std::fs::read(&src).unwrap()).unwrap();
    let dynsym = img.find_section(".dynsym").unwrap();
    let dynstr = img.find_section(".dynstr").unwrap();
    let name = (0..img.section_data[dynsym].len() / 24)
        .filter_map(|i| {
            let b = &img.section_data[dynsym][i * 24..];
            let st_name = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            formatelf::ir::cstr(&img.section_data[dynstr], st_name)
        })
        .find(|n| n.bytes().next().is_some_and(|c| c.is_ascii_alphabetic()))
        .unwrap()
        .to_owned();

    let map = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rename.map");
    std::fs::write(&map, format!("{name} {name}_renamed\n")).unwrap();

    let ours = copy("exe-dyn-le", "rename-ours");
    let theirs = copy("exe-dyn-le", "rename-ref");
    patch(&ours, &["--rename-dynamic-symbols", map.to_str().unwrap()]);
    assert!(Command::new(&reference)
        .args(["--rename-dynamic-symbols", map.to_str().unwrap()])
        .arg(&theirs)
        .status()
        .unwrap()
        .success());

    let a = formatelf::parser::parse(&std::fs::read(&ours).unwrap()).unwrap();
    let b = formatelf::parser::parse(&std::fs::read(&theirs).unwrap()).unwrap();
    for sec in [".dynsym", ".gnu.hash", ".hash", ".gnu.version", ".dynstr"] {
        if let (Some(i), Some(j)) = (a.find_section(sec), b.find_section(sec)) {
            assert_eq!(
                a.section_data[i], b.section_data[j],
                "{sec} differs from reference"
            );
        }
    }
}

#[test]
fn build_resolution_cache_matches_reference() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    let needed = out(&reference, "--print-needed", &src);

    // A run-path directory holding (placeholder) files for each needed lib.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ldcache-dir");
    std::fs::create_dir_all(&dir).unwrap();
    for lib in needed.lines() {
        std::fs::copy(&src, dir.join(lib)).unwrap();
    }

    let mk = |tag: &str| {
        let bin = copy("exe-dyn-le", tag);
        patch(&bin, &["--set-rpath", dir.to_str().unwrap()]);
        bin
    };
    let ours = mk("ldcache-ours");
    let theirs = mk("ldcache-ref");
    patch(&ours, &["--build-resolution-cache"]);
    assert!(Command::new(&reference)
        .arg("--build-resolution-cache")
        .arg(&theirs)
        .status()
        .unwrap()
        .success());

    let a = formatelf::parser::parse(&std::fs::read(&ours).unwrap()).unwrap();
    let b = formatelf::parser::parse(&std::fs::read(&theirs).unwrap()).unwrap();
    let ai = a.find_section(".note.nixos.ldcache").expect("note written");
    let bi = b.find_section(".note.nixos.ldcache").unwrap();
    assert_eq!(
        a.section_data[ai], b.section_data[bi],
        "ld-cache note differs"
    );

    // The note must be covered by a PT_NOTE segment.
    assert!(a
        .phdrs
        .iter()
        .any(|p| p.p_type == formatelf::ir::pt::NOTE && p.vaddr == a.shdrs[ai].addr));
}

#[test]
fn no_clobber_appends_a_fresh_region() {
    let Some(_reference) = guard() else { return };
    let bin = copy("exe-dyn-le", "noclobber");
    let loads = |p: &Path| {
        formatelf::parser::parse(&std::fs::read(p).unwrap())
            .unwrap()
            .phdrs
            .iter()
            .filter(|h| h.p_type == formatelf::ir::pt::LOAD)
            .count()
    };

    patch(
        &bin,
        &["--set-rpath", "/opt/first/aaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
    );
    let before = loads(&bin);
    // With clobbering disabled the prior region is not reused, so a new
    // PT_LOAD is added rather than reclaimed.
    patch(
        &bin,
        &[
            "--no-clobber-old-sections",
            "--set-rpath",
            "/opt/second/bbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ],
    );
    assert!(loads(&bin) > before, "expected a fresh PT_LOAD");
    assert!(Command::new(&bin).status().unwrap().success());
}

#[test]
fn adds_gnu_stack_segment_when_absent() {
    use formatelf::ir::{pf, pt};
    let orig = std::fs::read(sample("exe-dyn-le")).unwrap();
    let mut img = formatelf::parser::parse(&orig).unwrap();
    // Drop every slot the op could otherwise reuse, forcing a fresh phdr.
    img.phdrs
        .retain(|p| p.p_type != pt::GNU_STACK && p.p_type != pt::NULL);
    let before = img.phdrs.len();

    let mut report = formatelf::ops::Report { lines: vec![] };
    formatelf::ops::apply(
        &mut img,
        &formatelf::cli::Operation::SetExecstack,
        &formatelf::ops::Modifiers::default(),
        &mut report,
    )
    .unwrap();
    assert_eq!(img.phdrs.len(), before + 1, "a new phdr should be appended");

    let bytes = formatelf::layout::finalize(&mut img, &orig, None, false, false).unwrap();
    let out = formatelf::parser::parse(&bytes).unwrap();
    let gs = out
        .phdrs
        .iter()
        .find(|p| p.p_type == pt::GNU_STACK)
        .expect("PT_GNU_STACK present");
    // A fresh executable stack is exactly R|W|X.
    assert_eq!(gs.flags, pf::R | pf::W | pf::X, "stack flags");
    // The segment carries no file content and needs no address.
    assert_eq!((gs.offset, gs.vaddr, gs.filesz, gs.memsz), (0, 0, 0, 0));
}

fn ldcache_note(p: &Path) -> Vec<u8> {
    let img = formatelf::parser::parse(&std::fs::read(p).unwrap()).unwrap();
    let i = img
        .find_section(".note.nixos.ldcache")
        .expect("ld-cache note written");
    img.section_data[i].clone()
}

/// $ORIGIN (and other token/hwcaps dirs) cannot be resolved at patch time, so
/// the directory is recorded as a `?` hint rather than a `=` absolute path.
#[test]
fn build_resolution_cache_records_unresolvable_hint() {
    let Some(reference) = guard() else { return };
    let mk = |tag: &str| {
        let bin = copy("exe-dyn-le", tag);
        patch(&bin, &["--set-rpath", "$ORIGIN/../lib"]);
        bin
    };
    let ours = mk("ldcache-hint-ours");
    let theirs = mk("ldcache-hint-ref");
    patch(&ours, &["--build-resolution-cache"]);
    assert!(Command::new(&reference)
        .arg("--build-resolution-cache")
        .arg(&theirs)
        .status()
        .unwrap()
        .success());
    assert_eq!(ldcache_note(&ours), ldcache_note(&theirs));
}

/// A lib found under several run-path directories yields one note entry whose
/// resolved paths are joined with ':'.
#[test]
fn build_resolution_cache_joins_multiple_dirs() {
    let Some(reference) = guard() else { return };
    let src = sample("exe-dyn-le");
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ldcache-multi");
    let (d1, d2) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(&d1).unwrap();
    std::fs::create_dir_all(&d2).unwrap();
    for lib in out(&reference, "--print-needed", &src).lines() {
        std::fs::copy(&src, d1.join(lib)).unwrap();
        std::fs::copy(&src, d2.join(lib)).unwrap();
    }
    let rpath = format!("{}:{}", d1.display(), d2.display());
    let mk = |tag: &str| {
        let bin = copy("exe-dyn-le", tag);
        patch(&bin, &["--set-rpath", &rpath]);
        bin
    };
    let ours = mk("ldcache-multi-ours");
    let theirs = mk("ldcache-multi-ref");
    patch(&ours, &["--build-resolution-cache"]);
    assert!(Command::new(&reference)
        .arg("--build-resolution-cache")
        .arg(&theirs)
        .status()
        .unwrap()
        .success());
    assert_eq!(ldcache_note(&ours), ldcache_note(&theirs));
}

#[test]
fn build_resolution_cache_refreshes_in_place() {
    use formatelf::ir::pt;
    let Some(_reference) = guard() else { return };
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ldcache-refresh-dir");
    std::fs::create_dir_all(&dir).unwrap();
    let src = sample("exe-dyn-le");
    for lib in out(&_reference, "--print-needed", &src).lines() {
        std::fs::copy(&src, dir.join(lib)).unwrap();
    }

    let bin = copy("exe-dyn-le", "ldcache-refresh");
    patch(&bin, &["--set-rpath", dir.to_str().unwrap()]);
    patch(&bin, &["--build-resolution-cache"]);
    // An intervening relayout moves the note; a second build must refresh it,
    // not append a duplicate, and keep its PT_NOTE anchored.
    patch(&bin, &["--add-needed", "libfoo.so.1"]);
    patch(&bin, &["--build-resolution-cache"]);

    let img = formatelf::parser::parse(&std::fs::read(&bin).unwrap()).unwrap();
    let notes: Vec<usize> = (0..img.shdrs.len())
        .filter(|&i| img.section_name(i) == Some(".note.nixos.ldcache"))
        .collect();
    assert_eq!(notes.len(), 1, "exactly one ld-cache note section");
    let addr = img.shdrs[notes[0]].addr;
    assert!(
        img.phdrs
            .iter()
            .any(|p| p.p_type == pt::NOTE && p.vaddr == addr),
        "PT_NOTE must stay anchored on the note after a move"
    );
}

/// After a relayout that moves .dynamic, the MIPS run-time-loader-debug offset
/// (DT_MIPS_RLD_MAP_REL) must still resolve to .rld_map, and PT_MIPS_ABIFLAGS
/// must still cover .MIPS.abiflags.
#[test]
fn mips_relayout_fixes_arch_specific_fields() {
    use formatelf::ir::{dt, pt, sht};
    if !fixtures::zig_available() {
        return;
    }
    let bin = copy("exe-mips-be", "mips-relayout");
    // Adds a DT_NEEDED entry and a .dynstr string, growing both and forcing a
    // relayout that relocates .dynamic.
    patch(&bin, &["--add-needed", "libextra.so.1"]);

    let img = formatelf::parser::parse(&std::fs::read(&bin).unwrap()).unwrap();
    let addr = |name: &str| img.shdrs[img.find_section(name).unwrap()].addr;
    let dyn_addr = img.shdrs[img
        .shdrs
        .iter()
        .position(|s| s.sh_type == sht::DYNAMIC)
        .unwrap()]
    .addr;
    let dyn_stride = if img.enc.class == formatelf::ir::Class::Elf32 {
        8
    } else {
        16
    };
    let rld = addr(".rld_map");
    let (i, e) = img
        .dynamic
        .iter()
        .enumerate()
        .find(|(_, d)| d.tag == dt::MIPS_RLD_MAP_REL)
        .expect("fixture must carry DT_MIPS_RLD_MAP_REL");
    let mut expect = rld
        .wrapping_sub(i as u64 * dyn_stride)
        .wrapping_sub(dyn_addr);
    if img.enc.class == formatelf::ir::Class::Elf32 {
        expect &= 0xffff_ffff;
    }
    assert_eq!(
        e.val, expect,
        "DT_MIPS_RLD_MAP_REL not recomputed after move"
    );

    let af = img.find_section(".MIPS.abiflags").unwrap();
    let seg = img
        .phdrs
        .iter()
        .find(|p| p.p_type == pt::MIPS_ABIFLAGS)
        .expect("PT_MIPS_ABIFLAGS segment");
    assert_eq!(
        seg.vaddr, img.shdrs[af].addr,
        "PT_MIPS_ABIFLAGS not anchored"
    );
    assert_eq!(seg.offset, img.shdrs[af].offset);
    assert_eq!(seg.filesz, img.shdrs[af].size);
}
