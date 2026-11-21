# 0006 HLE/LLE split

Status: accepted

HLE for OS, DVD, PAD, CARD (raw Dolphin-compatible memory card image), AX (mixer to SDL3, Dolphin as behavioural reference) and VI. LLE for the GX FIFO: a CP/XF/BP parser feeds the SDL_GPU backend. SDK functions are identified by signature matching (dtk/doldecomp) and manual config.

FP: clang vector extensions; `fres`, `frsqrte` and conversions bit-exact; fast paths optional and validated by instruction tests.
