// SPDX-License-Identifier: GPL-2.0-or-later
#include "fepor/runtime.h"

#include <algorithm>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>

#include "fepor/dispatch.h"
#include "fepor/ppc.h"

namespace fepor {

void fatal(const char* fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    fprintf(stderr, "fepor: fatal: ");
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    va_end(ap);
    abort();
}

void unimplemented(Context&, uint32_t addr, uint32_t code) {
    fatal("unimplemented instruction %08x at %08x", code, addr);
}

void unresolved(Context&, uint32_t from, uint32_t target) {
    fatal("control flow from %08x to %08x has no generated function", from, target);
}

HostFn lookup(uint32_t from, uint32_t target) {
    const DispatchEntry* end = fepor_dispatch_table + fepor_dispatch_count;
    const DispatchEntry* it = std::lower_bound(
        fepor_dispatch_table, end, target, [](const DispatchEntry& e, uint32_t a) { return e.addr < a; });
    if (it == end || it->addr != target) fatal("indirect branch at %08x to %08x: not in dispatch table", from, target);
    return it->fn;
}

void trap(Context&, uint32_t addr) { fatal("trap at %08x", addr); }
void syscall(Context&, uint32_t addr) { fatal("sc at %08x", addr); }
void rfi(Context&, uint32_t addr) { fatal("rfi at %08x (exception return not supported yet)", addr); }
uint32_t mftb(uint32_t tbr) { fatal("mftb %u: timebase not implemented yet", tbr); }
uint32_t hw_read(uint32_t ea, int size) { fatal("hardware register read (%d bytes) at %08x not implemented yet", size, ea); }
void hw_write(uint32_t ea, uint32_t value, int size) {
    fatal("hardware register write (%d bytes, value %08x) at %08x not implemented yet", size, value, ea);
}

}  // namespace fepor
