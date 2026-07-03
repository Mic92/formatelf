// Rust links glibc dynamically and pulls a heavier .dynamic, more PT_LOAD
// segments and larger .eh_frame than a C hello. Content is irrelevant.
fn main() {
    std::process::exit(std::hint::black_box(20) & 0);
}
