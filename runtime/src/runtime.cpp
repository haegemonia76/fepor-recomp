// SPDX-License-Identifier: GPL-2.0-or-later
// C-ABI runtime services for test programs (instruction vectors, helper tests). The real game uses
// crates/runtime, which implements the same symbols.
#include "fepor/runtime.h"

#include <algorithm>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>

#include "fepor/dispatch.h"
#include "fepor/ppc.h"

extern "C" {

void fepor_fatal(const char* fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    fprintf(stderr, "fepor: fatal: ");
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    va_end(ap);
    abort();
}

void fepor_unimplemented(fepor::Context*, uint32_t addr, uint32_t code) {
    fepor_fatal("unimplemented instruction %08x at %08x", code, addr);
}

void fepor_unresolved(fepor::Context*, uint32_t from, uint32_t target) {
    fepor_fatal("control flow from %08x to %08x has no generated function", from, target);
}

fepor::HostFn fepor_lookup(uint32_t from, uint32_t target) {
    const fepor::DispatchEntry* end = fepor_dispatch_table + fepor_dispatch_count;
    const fepor::DispatchEntry* it = std::lower_bound(
        fepor_dispatch_table, end, target, [](const fepor::DispatchEntry& e, uint32_t a) { return e.addr < a; });
    if (it == end || it->addr != target) fepor_fatal("indirect branch at %08x to %08x: not in dispatch table", from, target);
    return it->fn;
}

void fepor_trap(fepor::Context*, uint32_t addr) { fepor_fatal("trap at %08x", addr); }
void fepor_syscall(fepor::Context*, uint32_t addr) { fepor_fatal("sc at %08x", addr); }
void fepor_rfi(fepor::Context*, uint32_t addr) { fepor_fatal("rfi at %08x (exception return not supported yet)", addr); }
uint32_t fepor_mftb(uint32_t tbr) { fepor_fatal("mftb %u: timebase not implemented yet", tbr); }
uint32_t fepor_hw_read(uint32_t ea, int size) { fepor_fatal("hardware register read (%d bytes) at %08x not implemented yet", size, ea); }
void fepor_hw_write(uint32_t ea, uint32_t value, int size) {
    fepor_fatal("hardware register write (%d bytes, value %08x) at %08x not implemented yet", size, value, ea);
}
void fepor_enter(uint32_t) {}

}  // extern "C"
