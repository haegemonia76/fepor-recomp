// SPDX-License-Identifier: GPL-2.0-or-later
// Guest address to host function table, emitted by `fepor-recomp generate`.
#pragma once

#include <cstddef>
#include <cstdint>

#include "fepor/ppc.h"

namespace fepor {

struct DispatchEntry {
    uint32_t addr;
    HostFn fn;
};

}  // namespace fepor

/// Sorted by address. Defined in the generated dispatch.cpp.
extern const fepor::DispatchEntry fepor_dispatch_table[];
extern const size_t fepor_dispatch_count;
