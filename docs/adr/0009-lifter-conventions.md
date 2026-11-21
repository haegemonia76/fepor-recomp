# 0009 Lifter conventions and known limits

Status: accepted

- Each guest function becomes `void f_XXXXXXXX(Context&)`. Registers live in C++ locals (`r[]`, `f[]`, `cr[]`, `lr`, `ctr`, `xer`, `fpscr`, only those a function uses). `in()`/`out()` lambdas copy them from/to the `Context` at entry, around every call and before every return or tail call. Rarely used state (other SPRs, GQRs, MSR, SR) is read and written on the `Context` directly.
- `bl` sets `lr` to the return address and calls the target function; `blr` is a C++ `return`. Code that returns by editing `lr` (context switches, longjmp) is the runtime's job (HLE).
- Tail calls and unresolved `bctr` use `[[clang::musttail]]`; `bctr` with a resolved jump table is a `switch` on `ctr` with `goto` into the function; targets outside the function are tail calls. Anything without a generated function ends in `unresolved(...)` (explicit fatal with both addresses).
- FP follows Dolphin's interpreter: single-precision (`*s`, `frsp`, `fres`) results fill ps0 and ps1, double results set ps0 only, `fmuls`/`fmadds`/`ps_mul*` round operand C to 25 bits, `fctiw` returns `0xFFF80000_xxxxxxxx`, `fres`/`frsqrte` use Dolphin's tables and algorithm (hence ADR 0002's GPL decision).
- Not modelled yet: FPSCR exception/FPRF/FI/FR flags (only RN and the bits written by `mtfsf`/`mtfsb`/`mtfsfi`), OE forms of carry instructions and `mulhw*` (emitted as runtime traps), string instructions, external-control instructions.
- `rfi`, `sc`, trap words, hardware-register access and `mftb` call runtime functions that are explicit fatals until the matching runtime milestone.
- Generated code is `-w` and expects clang (`ext_vector_type`, `musttail`).
