/* A const table of addresses forces R_*_RELATIVE relocations, which the linker
 * packs into DT_RELR when built with -z pack-relative-relocs. Exercises the
 * RELR-aware paths (a modern glibc/Rust default). */
static int a, b, c;
int *const table[] = {&a, &b, &c};

int main(void) { return (table[0] == &a) & 0; }
