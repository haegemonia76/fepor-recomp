# 0007 Presentation extras by design

Status: accepted

Fidelity first for the first milestone, but architecture supports: resizable virtual EFB, high frame rate via render-time interpolation (native sim rate measured, not assumed; per-version exclusion list for UI, transitions, THP), full input remapping, glyph sets (Xbox/Nintendo/PlayStation). Textures decoded to RGBA8 with a content-hash cache behind an interface allowing BC/ASTC later. THP via libjpeg-turbo plus own ADPCM decoder. Stack: SDL3 + SDL_GPU.
