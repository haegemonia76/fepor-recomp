// SPDX-License-Identifier: GPL-2.0-or-later
// Unit tests for runtime helpers that have no oracle in the instruction vectors (those are in
// crates/ppc-lift/tests): estimate accuracy and special values, conversions, NaN rules.
#include <bit>
#include <cmath>
#include <cstdio>
#include <cstdlib>

#include "fepor/dispatch.h"
#include "fepor/ppc.h"

using namespace fepor;

// The runtime library needs a dispatch table; none of these tests call through it.
const DispatchEntry fepor_dispatch_table[] = {{0, nullptr}};
const size_t fepor_dispatch_count = 0;

static int failures = 0;
#define CHECK(cond)                                                       \
    do {                                                                  \
        if (!(cond)) {                                                    \
            fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #cond); \
            failures++;                                                   \
        }                                                                 \
    } while (0)

static uint64_t bits(double d) { return std::bit_cast<uint64_t>(d); }

static void estimates() {
    // Hardware guarantees roughly 12 bits; a typo in a table entry shows up as a spike here.
    double max_res = 0, max_rsqrte = 0;
    for (int e = -120; e < 120; e++)
        for (int k = 0; k < 4096; k++) {
            double x = std::ldexp(1.0 + k / 4096.0, e);
            max_res = std::fmax(max_res, std::fabs(fres(x) * x - 1.0));
            double s = frsqrte(x);
            max_rsqrte = std::fmax(max_rsqrte, std::fabs(s * s * x - 1.0));
        }
    CHECK(max_res < std::ldexp(1.0, -12));
    CHECK(max_rsqrte < std::ldexp(1.0, -11));
    CHECK(fres(-4.0) < 0 && std::fabs(fres(-4.0) * -4.0 - 1.0) < std::ldexp(1.0, -12));

    CHECK(fres(0.0) == INFINITY && fres(-0.0) == -INFINITY);
    CHECK(fres(INFINITY) == 0.0 && !std::signbit(fres(INFINITY)));
    CHECK(std::signbit(fres(-INFINITY)));
    CHECK(std::isnan(fres(NAN)));
    CHECK(fres(1e-300) == (double)std::numeric_limits<float>::max());  // too small: saturates
    CHECK(fres(1e300) == 0.0);                                         // too large: zero
    CHECK(frsqrte(0.0) == INFINITY && frsqrte(-0.0) == -INFINITY);
    CHECK(std::isnan(frsqrte(-1.0)));
    CHECK(frsqrte(INFINITY) == 0.0);
    CHECK(std::isnan(frsqrte(-INFINITY)));
    CHECK(std::fabs(frsqrte(5e-324) * std::sqrt(5e-324) - 1.0) < std::ldexp(1.0, -11));  // denormal input
}

static void conversions() {
    // stfs truncates, it does not round to nearest
    CHECK(double_to_single(1.0 + std::ldexp(1.0, -24)) == 0x3F800000u);  // would round to even, still truncated
    CHECK(double_to_single(1.0 + std::ldexp(1.0, -23) * 0.99) == 0x3F800000u);
    CHECK(double_to_single(-1.5) == 0xBFC00000u);
    CHECK(double_to_single(0.0) == 0 && double_to_single(-0.0) == 0x80000000u);
    CHECK(double_to_single(INFINITY) == 0x7F800000u);
    CHECK(double_to_single(std::ldexp(1.0, -149)) == 1u);  // smallest single denormal
    CHECK(double_to_single(std::ldexp(1.0, -127)) == 0x00400000u);
    CHECK(double_to_single(1e-60) == 0u);       // underflow to zero
    // lfs keeps signalling NaNs signalling
    CHECK(bits(single_to_double(0x7FA00000u)) == 0x7FF4000000000000ull);
    CHECK(bits(single_to_double(0xFF800000u)) == bits(-INFINITY));
    CHECK(single_to_double(0x00000001u) == (double)std::bit_cast<float>(0x00000001u));
    CHECK(single_to_double(0x3F800000u) == 1.0);
}

static void nan_rules() {
    const double inf = INFINITY;
    CHECK(bits(pfsub(inf, inf)) == 0x7FF8000000000000ull);  // default NaN is positive on every host
    CHECK(bits(pfmul(0.0, inf)) == 0x7FF8000000000000ull);
    CHECK(bits(pfdiv(0.0, 0.0)) == 0x7FF8000000000000ull);
    double snan = std::bit_cast<double>(0x7FF0000000000001ull);
    CHECK(bits(pfadd(snan, 1.0)) == 0x7FF8000000000001ull);  // operand NaN, quieted
    CHECK(bits(pfadd(1.0, snan)) == 0x7FF8000000000001ull);
    double qa = std::bit_cast<double>(0x7FF8000000000AAAull), qb = std::bit_cast<double>(0x7FF8000000000BBBull);
    CHECK(bits(pfadd(qa, qb)) == 0x7FF8000000000AAAull);  // A has priority over B
    CHECK(bits(pfmadd(1.0, qb, qa)) == 0x7FF8000000000AAAull);  // a*c + b: priority A, B, C
    CHECK(bits(neg_nonan(qa)) == bits(qa));
    // force25 leaves inf/NaN alone and rounds the 25th bit
    CHECK(bits(force25(qa)) == bits(qa));
    CHECK(force25(1.0 + std::ldexp(1.0, -24)) == 1.0 + std::ldexp(1.0, -24));
}

int main() {
    estimates();
    conversions();
    nan_rules();
    if (failures) {
        fprintf(stderr, "%d check(s) failed\n", failures);
        return 1;
    }
    puts("ok");
    return 0;
}
