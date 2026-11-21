# 0001 Static recompilation of the DOL

Status: accepted

Gekko code from `main.dol` is lifted statically to C++ and linked with a hand-written runtime (N64Recomp/XenonRecomp model). Not a matching decompilation; progressive decompilation may come later. The DOL has no `.rel` modules, so addresses are fixed and no runtime relocation is needed. No interpreter fallback.

Indirect calls go through a runtime dispatch table (guest address to host function); a miss is an explicit logged error. Static analysis is corrected by TOML overrides (functions, jump tables, exclusions).

Generated code is grouped into compilation units of configurable size (default a few hundred functions) so ninja can parallelise. Registers are C++ locals; clang optimises. No custom optimisation pass before measuring.
