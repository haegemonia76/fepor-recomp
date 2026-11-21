// SPDX-License-Identifier: GPL-2.0-or-later
// Gekko guest model and instruction helpers used by generated code.
#pragma once

#include <bit>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>

namespace fepor {

using v2d = double __attribute__((ext_vector_type(2)));
using v2f = float __attribute__((ext_vector_type(2)));

struct CRF {
    uint8_t lt, gt, eq, so;
};

struct Xer {
    uint8_t so, ov, ca, bc;
};

constexpr uint32_t kRamSize = 0x01800000;  // 24 MiB MEM1

struct Context {
    uint32_t r[32];
    v2d f[32];  // .x = ps0, .y = ps1
    CRF cr[8];
    uint32_t lr, ctr;
    Xer xer;
    uint32_t fpscr;
    uint32_t msr;
    uint32_t spr[1024];
    uint32_t sr[16];
    uint32_t reserve_addr;
    uint8_t* ram;  // kRamSize bytes plus slack for accesses straddling the end
};

static_assert(sizeof(Context) == 4864 && offsetof(Context, ram) == 4856, "Context layout is mirrored by crates/runtime/src/context.rs");

using HostFn = void (*)(Context&);

/// Thrown by generated code to abandon the host frames of a guest call chain (a thread resumed after an
/// interrupt-time context switch continues from the interrupted instruction, not from the handler).
struct Unwind {};
[[noreturn]] inline void unwind() { throw Unwind{}; }

}  // namespace fepor

// ---- runtime services: C ABI, implemented by crates/runtime (or runtime/src for tests) -------

extern "C" {
[[noreturn]] void fepor_fatal(const char* fmt, ...) __attribute__((format(printf, 1, 2)));
[[noreturn]] void fepor_unimplemented(fepor::Context* c, uint32_t addr, uint32_t code);
/// Control flow to a guest address that has no generated function.
[[noreturn]] void fepor_unresolved(fepor::Context* c, uint32_t from, uint32_t target);
/// Guest address to host function; fatal if the address is not in the dispatch table.
fepor::HostFn fepor_lookup(uint32_t from, uint32_t target);
[[noreturn]] void fepor_trap(fepor::Context* c, uint32_t addr);
void fepor_syscall(fepor::Context* c, uint32_t addr);
void fepor_rfi(fepor::Context* c, uint32_t addr);
uint32_t fepor_mftb(uint32_t tbr);
uint32_t fepor_mfdec(void);
void fepor_mtdec(uint32_t value);
void fepor_lc_dma(fepor::Context* c);
uint32_t fepor_hw_read(uint32_t ea, int size);
void fepor_hw_write(uint32_t ea, uint32_t value, int size);
/// Function entry hook, emitted when generating with call tracing.
void fepor_enter(uint32_t addr);
/// Backward branches count this down; at zero the runtime gets a chance to deliver interrupts.
extern uint32_t fepor_ticks;
void fepor_poll(fepor::Context* c);
}

namespace fepor {

[[noreturn]] inline void unimplemented(Context& c, uint32_t addr, uint32_t code) { fepor_unimplemented(&c, addr, code); }
[[noreturn]] inline void unresolved(Context& c, uint32_t from, uint32_t target) { fepor_unresolved(&c, from, target); }
inline HostFn lookup(uint32_t from, uint32_t target) { return fepor_lookup(from, target); }
[[noreturn]] inline void trap(Context& c, uint32_t addr) { fepor_trap(&c, addr); }
inline void syscall(Context& c, uint32_t addr) { fepor_syscall(&c, addr); }
inline void rfi(Context& c, uint32_t addr) { fepor_rfi(&c, addr); }
inline uint32_t mftb(uint32_t tbr) { return fepor_mftb(tbr); }
inline uint32_t hw_read(uint32_t ea, int size) { return fepor_hw_read(ea, size); }
inline void hw_write(uint32_t ea, uint32_t value, int size) { fepor_hw_write(ea, value, size); }

// ---- memory ---------------------------------------------------------------------------------

inline bool in_ram(uint32_t ea) { return (ea & 0x3FFFFFFF) < kRamSize; }
inline uint8_t* host_ptr(uint8_t* m, uint32_t ea) { return m + (ea & 0x3FFFFFFF); }

inline uint8_t ld8(uint8_t* m, uint32_t ea) {
    if (__builtin_expect(in_ram(ea), 1)) return *host_ptr(m, ea);
    return (uint8_t)hw_read(ea, 1);
}
inline uint16_t ld16(uint8_t* m, uint32_t ea) {
    if (__builtin_expect(in_ram(ea), 1)) {
        uint16_t v;
        memcpy(&v, host_ptr(m, ea), 2);
        return __builtin_bswap16(v);
    }
    return (uint16_t)hw_read(ea, 2);
}
inline uint32_t ld32(uint8_t* m, uint32_t ea) {
    if (__builtin_expect(in_ram(ea), 1)) {
        uint32_t v;
        memcpy(&v, host_ptr(m, ea), 4);
        return __builtin_bswap32(v);
    }
    return hw_read(ea, 4);
}
inline uint64_t ld64(uint8_t* m, uint32_t ea) {
    if (__builtin_expect(in_ram(ea), 1)) {
        uint64_t v;
        memcpy(&v, host_ptr(m, ea), 8);
        return __builtin_bswap64(v);
    }
    return ((uint64_t)hw_read(ea, 4) << 32) | hw_read(ea + 4, 4);
}
inline void st8(uint8_t* m, uint32_t ea, uint8_t v) {
    if (__builtin_expect(in_ram(ea), 1)) *host_ptr(m, ea) = v;
    else hw_write(ea, v, 1);
}
inline void st16(uint8_t* m, uint32_t ea, uint16_t v) {
    if (__builtin_expect(in_ram(ea), 1)) {
        v = __builtin_bswap16(v);
        memcpy(host_ptr(m, ea), &v, 2);
    } else hw_write(ea, v, 2);
}
inline void st32(uint8_t* m, uint32_t ea, uint32_t v) {
    if (__builtin_expect(in_ram(ea), 1)) {
        v = __builtin_bswap32(v);
        memcpy(host_ptr(m, ea), &v, 4);
    } else hw_write(ea, v, 4);
}
inline void st64(uint8_t* m, uint32_t ea, uint64_t v) {
    if (__builtin_expect(in_ram(ea), 1)) {
        v = __builtin_bswap64(v);
        memcpy(host_ptr(m, ea), &v, 8);
    } else {
        hw_write(ea, (uint32_t)(v >> 32), 4);
        hw_write(ea + 4, (uint32_t)v, 4);
    }
}
inline uint16_t ld16br(uint8_t* m, uint32_t ea) { return __builtin_bswap16(ld16(m, ea)); }
inline uint32_t ld32br(uint8_t* m, uint32_t ea) { return __builtin_bswap32(ld32(m, ea)); }
inline void st16br(uint8_t* m, uint32_t ea, uint16_t v) { st16(m, ea, __builtin_bswap16(v)); }
inline void st32br(uint8_t* m, uint32_t ea, uint32_t v) { st32(m, ea, __builtin_bswap32(v)); }

inline void dcbz(uint8_t* m, uint32_t ea) {
    ea &= ~31u;
    for (uint32_t i = 0; i < 32; i += 4) st32(m, ea + i, 0);
}

// ---- integer --------------------------------------------------------------------------------

inline uint32_t rotl32(uint32_t v, uint32_t n) { return std::rotl(v, (int)(n & 31)); }
inline uint32_t extsb(uint32_t v) { return (uint32_t)(int32_t)(int8_t)v; }
inline uint32_t extsh(uint32_t v) { return (uint32_t)(int32_t)(int16_t)v; }
inline uint32_t cntlzw(uint32_t v) { return v ? (uint32_t)__builtin_clz(v) : 32u; }
inline uint32_t slw(uint32_t a, uint32_t s) { return (s & 0x20) ? 0 : a << (s & 31); }
inline uint32_t srw(uint32_t a, uint32_t s) { return (s & 0x20) ? 0 : a >> (s & 31); }
inline uint32_t sraw(Xer& x, uint32_t a, uint32_t s) {
    s &= 0x3F;
    int32_t v = (int32_t)a;
    if (s & 0x20) {
        x.ca = v < 0;
        return v < 0 ? 0xFFFFFFFFu : 0;
    }
    uint32_t r = (uint32_t)(v >> s);
    x.ca = v < 0 && s != 0 && (a & ((1u << s) - 1)) != 0;
    return r;
}
inline uint32_t srawi(Xer& x, uint32_t a, uint32_t s) { return sraw(x, a, s); }

inline uint32_t addc(Xer& x, uint32_t a, uint32_t b) {
    uint64_t t = (uint64_t)a + b;
    x.ca = t >> 32;
    return (uint32_t)t;
}
inline uint32_t adde(Xer& x, uint32_t a, uint32_t b) {
    uint64_t t = (uint64_t)a + b + x.ca;
    x.ca = t >> 32;
    return (uint32_t)t;
}
/// rB - rA with carry (subfc).
inline uint32_t subfc(Xer& x, uint32_t a, uint32_t b) {
    uint64_t t = (uint64_t)b + (uint32_t)~a + 1;
    x.ca = t >> 32;
    return (uint32_t)t;
}
inline uint32_t subfe(Xer& x, uint32_t a, uint32_t b) { return adde(x, ~a, b); }
inline uint32_t addze(Xer& x, uint32_t a) { return adde(x, a, 0); }
inline uint32_t addme(Xer& x, uint32_t a) { return adde(x, a, 0xFFFFFFFFu); }
inline uint32_t subfze(Xer& x, uint32_t a) { return adde(x, ~a, 0); }
inline uint32_t subfme(Xer& x, uint32_t a) { return adde(x, ~a, 0xFFFFFFFFu); }
inline uint32_t mullw(uint32_t a, uint32_t b) { return (uint32_t)((int64_t)(int32_t)a * (int32_t)b); }
inline uint32_t mulhw(uint32_t a, uint32_t b) { return (uint32_t)(((int64_t)(int32_t)a * (int32_t)b) >> 32); }
inline uint32_t mulhwu(uint32_t a, uint32_t b) { return (uint32_t)(((uint64_t)a * b) >> 32); }
inline uint32_t divw(uint32_t a, uint32_t b) {
    int32_t x = (int32_t)a, y = (int32_t)b;
    if (y == 0 || (x == INT32_MIN && y == -1)) return x < 0 ? 0xFFFFFFFFu : 0u;
    return (uint32_t)(x / y);
}
inline uint32_t divwu(uint32_t a, uint32_t b) { return b == 0 ? 0 : a / b; }

// Overflow-enabled (OE) forms: result plus OV/SO update.
inline void set_ov(Xer& x, bool ov) {
    x.ov = ov;
    x.so |= ov;
}
inline uint32_t add_o(Xer& x, uint32_t a, uint32_t b) {
    uint32_t r = a + b;
    set_ov(x, (~(a ^ b) & (a ^ r)) >> 31);
    return r;
}
inline uint32_t subf_o(Xer& x, uint32_t a, uint32_t b) {
    uint32_t r = b - a;
    set_ov(x, ((a ^ b) & (b ^ r)) >> 31);
    return r;
}
inline uint32_t neg_o(Xer& x, uint32_t a) {
    set_ov(x, a == 0x80000000u);
    return 0u - a;
}
inline uint32_t mullw_o(Xer& x, uint32_t a, uint32_t b) {
    int64_t p = (int64_t)(int32_t)a * (int32_t)b;
    set_ov(x, p != (int32_t)p);
    return (uint32_t)p;
}
inline uint32_t divw_o(Xer& x, uint32_t a, uint32_t b) {
    set_ov(x, (int32_t)b == 0 || (a == 0x80000000u && b == 0xFFFFFFFFu));
    return divw(a, b);
}
inline uint32_t divwu_o(Xer& x, uint32_t a, uint32_t b) {
    set_ov(x, b == 0);
    return divwu(a, b);
}

inline uint32_t xer_pack(const Xer& x) { return (uint32_t)x.so << 31 | (uint32_t)x.ov << 30 | (uint32_t)x.ca << 29 | (x.bc & 0x7F); }
inline Xer xer_unpack(uint32_t v) { return Xer{(uint8_t)(v >> 31 & 1), (uint8_t)(v >> 30 & 1), (uint8_t)(v >> 29 & 1), (uint8_t)(v & 0x7F)}; }

// ---- condition register ---------------------------------------------------------------------

inline CRF cmp_s(int32_t a, int32_t b, uint8_t so) { return CRF{a < b, a > b, a == b, so}; }
inline CRF cmp_u(uint32_t a, uint32_t b, uint8_t so) { return CRF{a < b, a > b, a == b, so}; }
/// CR0 after a record-form (Rc=1) instruction.
inline CRF rc0(uint32_t r, uint8_t so) { return cmp_s((int32_t)r, 0, so); }
inline uint32_t mfcr(const CRF (&cr)[8]) {
    uint32_t v = 0;
    for (int i = 0; i < 8; i++) v = v << 4 | cr[i].lt << 3 | cr[i].gt << 2 | cr[i].eq << 1 | cr[i].so;
    return v;
}
inline void mtcrf(CRF (&cr)[8], uint32_t mask, uint32_t v) {
    for (int i = 0; i < 8; i++)
        if (mask & (0x80u >> i)) {
            uint32_t n = v >> (28 - 4 * i);
            cr[i] = CRF{(uint8_t)(n >> 3 & 1), (uint8_t)(n >> 2 & 1), (uint8_t)(n >> 1 & 1), (uint8_t)(n & 1)};
        }
}
inline uint8_t crbit_get(const CRF (&cr)[8], int bit) { return (&cr[bit >> 2].lt)[bit & 3]; }
inline void crbit_set(CRF (&cr)[8], int bit, uint8_t v) { (&cr[bit >> 2].lt)[bit & 3] = v & 1; }

// ---- floating point -------------------------------------------------------------------------

inline double round_single(double x) { return (double)(float)x; }
inline v2d round_single(v2d x) { return __builtin_convertvector(__builtin_convertvector(x, v2f), v2d); }
inline v2d fill(double x) { return v2d{x, x}; }
inline double force25(double d) {
    uint64_t b = std::bit_cast<uint64_t>(d);
    if ((b >> 52 & 0x7FF) == 0x7FF) return d;  // inf/NaN pass through unchanged
    uint64_t integral = b & 0xFFFFFFFFF8000000ull;
    return std::bit_cast<double>(integral + (b & 0x8000000ull));
}
inline v2d force25(v2d d) { return v2d{force25(d.x), force25(d.y)}; }

// Arithmetic with PowerPC NaN rules: a NaN operand is returned quieted (priority A, B, C), an invalid
// operation produces the default NaN 0x7FF8000000000000. Host defaults differ (x86 yields 0xFFF8...).
inline double quiet_nan(double x) { return std::bit_cast<double>(std::bit_cast<uint64_t>(x) | 0x0008000000000000ull); }
inline double default_nan() { return std::bit_cast<double>(0x7FF8000000000000ull); }
inline double nan_fix(double r, double a, double b) {
    if (__builtin_expect(!std::isnan(r), 1)) return r;
    return std::isnan(a) ? quiet_nan(a) : std::isnan(b) ? quiet_nan(b) : default_nan();
}
inline double nan_fix(double r, double a, double b, double c) {
    if (__builtin_expect(!std::isnan(r), 1)) return r;
    return std::isnan(a) ? quiet_nan(a) : std::isnan(b) ? quiet_nan(b) : std::isnan(c) ? quiet_nan(c) : default_nan();
}
inline double pfadd(double a, double b) { return nan_fix(a + b, a, b); }
inline double pfsub(double a, double b) { return nan_fix(a - b, a, b); }
inline double pfmul(double a, double c) { return nan_fix(a * c, a, c); }
inline double pfdiv(double a, double b) { return nan_fix(a / b, a, b); }
/// Fused a*c + b (single rounding, as on Gekko). NaN priority follows the instruction: A, B, C.
inline double pfmadd(double a, double c, double b) { return nan_fix(std::fma(a, c, b), a, b, c); }
inline double pfmsub(double a, double c, double b) { return nan_fix(std::fma(a, c, -b), a, b, c); }

#define FEPOR_PS2(name, expr)                                  \
    inline v2d name(v2d a, v2d b) {                            \
        auto lane = [](double x, double y) { return expr; };   \
        return v2d{lane(a.x, b.x), lane(a.y, b.y)};            \
    }
FEPOR_PS2(ps_add, round_single(pfadd(x, y)))
FEPOR_PS2(ps_sub, round_single(pfsub(x, y)))
FEPOR_PS2(ps_div, round_single(pfdiv(x, y)))
#undef FEPOR_PS2
inline v2d ps_mul(v2d a, v2d c) { return v2d{round_single(pfmul(a.x, force25(c.x))), round_single(pfmul(a.y, force25(c.y)))}; }
inline v2d ps_madd(v2d a, v2d c, v2d b) {
    return v2d{round_single(pfmadd(a.x, force25(c.x), b.x)), round_single(pfmadd(a.y, force25(c.y), b.y))};
}
inline v2d ps_msub(v2d a, v2d c, v2d b) {
    return v2d{round_single(pfmsub(a.x, force25(c.x), b.x)), round_single(pfmsub(a.y, force25(c.y), b.y))};
}

double fres(double v);
double frsqrte(double v);
inline v2d ps_res(v2d v) { return v2d{fres(v.x), fres(v.y)}; }
inline v2d ps_rsqrte(v2d v) { return v2d{frsqrte(v.x), frsqrte(v.y)}; }

/// fctiw/fctiwz: `mode` is the FPSCR rounding mode (0 nearest, 1 zero, 2 +inf, 3 -inf).
inline double fctiw(double b, uint32_t mode) {
    double rounded;
    switch (mode & 3) {
        case 0: rounded = std::nearbyint(b); break;  // ties to even in the default environment
        case 1: rounded = std::trunc(b); break;
        case 2: rounded = std::ceil(b); break;
        default: rounded = std::floor(b); break;
    }
    uint32_t value;
    if (std::isnan(b)) value = 0x80000000u;
    else if (rounded >= 2147483648.0) value = 0x7FFFFFFFu;
    else if (rounded < -2147483648.0) value = 0x80000000u;
    else value = (uint32_t)(int32_t)rounded;
    uint64_t result = 0xFFF8000000000000ull | value;
    if (value == 0 && std::signbit(b)) result |= 0x100000000ull;
    return std::bit_cast<double>(result);
}

inline double neg_nonan(double x) { return std::isnan(x) ? x : -x; }
inline v2d neg_nonan(v2d x) { return v2d{neg_nonan(x.x), neg_nonan(x.y)}; }

/// Trap-word condition (TO bits: 16 lt, 8 gt, 4 eq, 2 ltu, 1 gtu).
inline bool tw_cond(uint32_t to, uint32_t a, uint32_t b) {
    int32_t sa = (int32_t)a, sb = (int32_t)b;
    return (sa < sb && (to & 16)) || (sa > sb && (to & 8)) || (sa == sb && (to & 4)) || (a < b && (to & 2)) || (a > b && (to & 1));
}

inline CRF fcmp(double a, double b) {
    if (std::isnan(a) || std::isnan(b)) return CRF{0, 0, 0, 1};
    return CRF{a < b, a > b, a == b, 0};
}
inline double fsel(double a, double b, double c) { return a >= -0.0 ? c : b; }

/// lfs conversion: exact, and NaN payloads (including signalling NaNs) are kept as is.
inline double single_to_double(uint32_t x) {
    if ((x >> 23 & 0xFF) == 0xFF)
        return std::bit_cast<double>((uint64_t)(x & 0x80000000u) << 32 | 0x7FF0000000000000ull | (uint64_t)(x & 0x7FFFFF) << 29);
    return (double)std::bit_cast<float>(x);
}
/// stfs conversion: the mantissa is truncated (not rounded); values below the single range become
/// denormals or zero. Out-of-range exponents wrap like the hardware does.
inline uint32_t double_to_single(double d) {
    uint64_t x = std::bit_cast<uint64_t>(d);
    uint32_t exp = (uint32_t)(x >> 52 & 0x7FF);
    if (exp > 896 || (x & 0x7FFFFFFFFFFFFFFFull) == 0) return (uint32_t)((x >> 32 & 0xC0000000u) | (x >> 29 & 0x3FFFFFFFu));
    if (exp >= 874) {
        uint64_t t = (x & 0x000FFFFFFFFFFFFFull) | 0x0010000000000000ull;
        t >>= 926 - exp;  // denormal single: mantissa = 1.f * 2^(exp - 874), truncated
        return (uint32_t)((x >> 32 & 0x80000000u) | t);
    }
    return (uint32_t)(x >> 32 & 0x80000000u);
}
inline double ldfs(uint8_t* m, uint32_t ea) { return single_to_double(ld32(m, ea)); }
inline double ldfd(uint8_t* m, uint32_t ea) { return std::bit_cast<double>(ld64(m, ea)); }
inline void stfs(uint8_t* m, uint32_t ea, double v) { st32(m, ea, double_to_single(v)); }
inline void stfd(uint8_t* m, uint32_t ea, double v) { st64(m, ea, std::bit_cast<uint64_t>(v)); }
inline void stfiwx(uint8_t* m, uint32_t ea, double v) { st32(m, ea, (uint32_t)std::bit_cast<uint64_t>(v)); }

/// Paired-single quantized load/store (GQR holds type and scale for both directions).
inline double dequant(uint32_t type, int scale, uint8_t* m, uint32_t ea, uint32_t* size) {
    double v;
    switch (type) {
        case 4: v = ld8(m, ea); *size = 1; break;
        case 5: v = ld16(m, ea); *size = 2; break;
        case 6: v = (int8_t)ld8(m, ea); *size = 1; break;
        case 7: v = (int16_t)ld16(m, ea); *size = 2; break;
        default: *size = 4; return ldfs(m, ea);
    }
    return std::ldexp(v, -scale);
}
inline v2d psq_ld(uint8_t* m, uint32_t ea, uint32_t gqr, uint32_t w) {
    uint32_t type = gqr >> 16 & 7;
    int scale = (int)(gqr >> 24 & 0x3F);
    scale = scale >= 32 ? scale - 64 : scale;
    if (type < 4 || type == 1 || type == 2 || type == 3) {
        type = 0;
        scale = 0;
    }
    uint32_t sz = 0;
    double a = dequant(type, scale, m, ea, &sz);
    if (w) return v2d{a, 1.0};
    return v2d{a, dequant(type, scale, m, ea + sz, &sz)};
}
inline uint32_t quant(double v, uint32_t type, int scale, uint32_t* size) {
    if (type < 4 || type > 7) {
        *size = 4;
        return double_to_single(v);
    }
    v = std::ldexp(v, scale);
    switch (type) {
        case 4: *size = 1; return (uint32_t)(v <= 0 || std::isnan(v) ? 0 : v >= 255 ? 255 : (int)v);
        case 5: *size = 2; return (uint32_t)(v <= 0 || std::isnan(v) ? 0 : v >= 65535 ? 65535 : (int)v);
        case 6: *size = 1; return (uint32_t)(int32_t)(std::isnan(v) ? 0 : v <= -128 ? -128 : v >= 127 ? 127 : (int)v) & 0xFF;
        default: *size = 2; return (uint32_t)(int32_t)(std::isnan(v) ? 0 : v <= -32768 ? -32768 : v >= 32767 ? 32767 : (int)v) & 0xFFFF;
    }
}
inline void psq_st(uint8_t* m, uint32_t ea, uint32_t gqr, uint32_t w, v2d v) {
    uint32_t type = gqr & 7;
    int scale = (int)(gqr >> 8 & 0x3F);
    scale = scale >= 32 ? scale - 64 : scale;
    uint32_t sz = 0;
    auto put = [&](uint32_t at, double d) {
        uint32_t q = quant(d, type, scale, &sz);
        if (sz == 1) st8(m, at, (uint8_t)q);
        else if (sz == 2) st16(m, at, (uint16_t)q);
        else st32(m, at, q);
    };
    put(ea, v.x);
    if (!w) put(ea + sz, v.y);
}

}  // namespace fepor
