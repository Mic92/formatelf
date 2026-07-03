/* __thread storage forces a PT_TLS segment plus .tdata/.tbss, which a trivial
 * hello lacks. volatile keeps the optimiser from eliding the access. */
static __thread volatile int slot[64];

int main(int argc, char **argv) {
    (void)argv;
    slot[argc] = argc;
    return 0;
}
