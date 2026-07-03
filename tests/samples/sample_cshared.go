// A Go c-shared object: the Go linker's shared-library layout, distinct again
// from its executable output. //export forces an exported cgo symbol.
package main

import "C"

//export Probe
func Probe(x C.int) C.int { return x*2 + 1 }

func main() {}
