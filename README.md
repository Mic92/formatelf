# formatelf

A reimplementation of [patchelf](https://github.com/NixOS/patchelf) in Rust:
modify the dynamic linker, RPATH, and other ELF metadata of executables and
shared libraries.

It is CLI-compatible with patchelf and the Nix package installs a `patchelf`
symlink, so it works as a drop-in replacement.

## Install

```sh
nix build github:Mic92/formatelf      # result/bin/formatelf (+ patchelf symlink)
nix run  github:Mic92/formatelf -- --version
```

To build without the `patchelf` symlink:

```sh
nix build --expr '(builtins.getFlake (toString ./.)).packages.x86_64-linux.default.override { patchelfSymlink = false; }'
```

Without Nix:

```sh
cargo build --release            # target/release/formatelf
```

## Usage

```sh
formatelf --set-interpreter /lib/ld-linux-x86-64.so.2 ./prog
formatelf --set-rpath '$ORIGIN/../lib' ./prog
formatelf --print-needed ./prog
formatelf --replace-needed libfoo.so.1 libbar.so.1 ./prog
```

Run `formatelf --help` for the full option list. The flags mirror patchelf,
including `--set-interpreter`, `--set-rpath`/`--add-rpath`/`--shrink-rpath`,
`--add-needed`/`--remove-needed`/`--replace-needed`, `--set-soname`,
`--set-os-abi`, `--clear-symbol-version`, `--set-execstack`/`--clear-execstack`,
`--rename-dynamic-symbols`, and the NixOS `--build-resolution-cache`.

## Beyond patchelf

The read-only operations (`--print-rpath`, `--print-needed`,
`--print-interpreter`, `--print-soname`) work on binaries whose section
headers have been stripped, recovering the data from `PT_DYNAMIC` and
`PT_INTERP`. Reference patchelf refuses these outright.

## Development

The flake's dev shell provides the toolchain:

```sh
nix develop
cargo test
cargo clippy --tests
```

Tests build their own ELF fixtures with `zig cc` on demand (no binaries are
committed). The differential tests compare output byte-for-byte against a
reference patchelf; they skip themselves when `zig` or the reference is absent.
`PATCHELF_REFERENCE` overrides the reference binary's path.

### Fuzzing

`cargo-fuzz` targets cover the read path (`parse`), the write path (`mutate`),
and the argument parser (`cli`). They build on stable via `RUSTC_BOOTSTRAP`:

```sh
RUSTC_BOOTSTRAP=1 cargo fuzz run parse -s none
```

## License

MIT
