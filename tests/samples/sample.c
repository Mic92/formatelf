/* Minimal program used only to generate ELF fixtures for codec round-trip
 * tests. Content is irrelevant; we exercise header/section/dynamic decoding. */
int answer(int x) { return x * 2 + 1; }

int main(void) { return answer(20) & 0; }
