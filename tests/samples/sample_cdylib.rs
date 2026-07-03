// A Rust cdylib: a shared object (no PT_INTERP) with an exported dynsym and a
// DT_SONAME, exercising the .so paths exes never reach.
#[no_mangle]
pub extern "C" fn probe(x: i32) -> i32 {
    std::hint::black_box(x) * 2 + 1
}
