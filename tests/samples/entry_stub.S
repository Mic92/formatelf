# x86_64 test stub for --set-entry-stub (see tests/entry_stub.rs). Regenerate STUB_CODE with:
#   clang -c entry_stub.S -o t.o && llvm-objcopy -O binary -j .text t.o t.bin  (bytes from offset 16)
.text
hdr: .ascii "TESTSTUB"
saved: .quad 0
.globl _stub
_stub:
  mov %rsp, %rsi            # rsi -> argc
  mov (%rsi), %rcx
  lea 16(%rsi,%rcx,8), %rsi # skip argc, argv[], NULL -> envp
1: mov (%rsi), %rcx
  add $8, %rsi
  test %rcx, %rcx
  jnz 1b                    # rsi -> auxv
  xor %r8d, %r8d            # r8 = AT_PHDR value
  xor %r9d, %r9d            # r9 = &AT_ENTRY value
  mov %rsi, %rdi
2: mov (%rdi), %rcx
  test %rcx, %rcx
  jz 4f
  cmp $3, %rcx
  jne 3f
  mov 8(%rdi), %r8
3: cmp $9, %rcx
  jne 5f
  lea 8(%rdi), %r9
5: add $16, %rdi
  jmp 2b
4: # bias = AT_PHDR - PT_PHDR.p_vaddr; find PT_PHDR by scanning phdrs at r8 for p_type==6
  mov %r8, %rdi
6: cmpl $6, (%rdi)
  je 7f
  add $56, %rdi
  jmp 6b
7: mov %r8, %rax
  sub 16(%rdi), %rax        # rax = bias
  add saved(%rip), %rax     # rax = real entry
  test %r9, %r9
  jz 8f
  mov %rax, (%r9)           # AT_ENTRY = real entry
8: xor %edx, %edx
  jmp *%rax
