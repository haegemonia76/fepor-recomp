// SPDX-License-Identifier: GPL-2.0-or-later
// Runs generated code so that fepor::Unwind exceptions (thread resumed after an interrupt-time context
// switch) stop here instead of crossing into Rust frames.
#include "fepor/ppc.h"

extern "C" int fepor_call_unwindable(void (*fn)(fepor::Context&), fepor::Context* c) {
    try {
        fn(*c);
        return 0;
    } catch (fepor::Unwind&) {
        return 1;
    }
}
