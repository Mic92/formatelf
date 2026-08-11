/* Mimics bun's single-file-executable template: a writable `.bun` section in
 * the data segment whose first word the compiler backend later overwrites with
 * the virtual address of an appended payload (see bun's write_bun_section).
 * Used by tests/bun_template.rs to check that patching keeps the layout that
 * backend relies on. */
__attribute__((section(".bun"), used)) volatile unsigned long long BUN_COMPILED[2] = { 0, 0 };

int main(void) { return (int)(BUN_COMPILED[1] & 0); }
