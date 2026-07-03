// cgo forces external linking, so the Go toolchain emits a dynamically linked
// executable (interpreter + DT_NEEDED) with its own segment layout, .gopclntab
// and .note.go.buildid. Content is irrelevant; we only decode the ELF shape.
package main

/*
int probe(int x) { return x * 2 + 1; }
*/
import "C"

func main() { _ = C.probe(20) }
