# 0013 GX command processor and wgpu video backend

Status: accepted

- **Split**: `crates/gx` is the front end (no GPU): it parses the write-gather pipe stream (CP/XF/BP registers, display lists, vertex data) and emits `Event`s (`Draw`, `Copy`, `Finish`, `Token`) to a `Sink`. `crates/video` implements `Sink` with wgpu. `crates/runtime` owns the `Gx` and feeds it from the WGP register at 0xCC008000 in 32-byte bursts, on the guest thread, synchronously.
- **CPU-side vertex transform**: positions go through the XF matrices on the CPU and arrive as clip-space vertices with the GX depth range (`(ndc_z*zrange+zfar)/2^24-1`, clamped to 0..1). Lighting channels (diffuse, no spot/specular) and regular texgen are computed there as well. Emboss, bump, projective texgen variants, fog and indirect texturing are not modelled yet.
- **TEV as generated WGSL** (`gx::tev`): one fragment shader per distinct TEV/alpha-compare configuration (source hashed, pipelines cached on shader, blend, z, mask, cull). Colour registers and konsts are uniforms, so changing them does not recompile.
- **EFB**: 640x528 RGBA8 plus Depth32Float. Each draw is its own render pass with `LoadOp::Load`. EFB copies to textures stay on the GPU, keyed by destination address; copies to the XFB are the presented frames. Copy-with-clear implements the clear.
- **Textures**: decoded on the CPU from guest RAM (`gx::texture`: I4/I8/IA4/IA8/RGB565/RGB5A3/RGBA8/C4/C8/C14X2/CMPR, TLUT from `LOADTLUT`), cached on address, format, size, TLUT and a sampled content fingerprint. No mipmaps, LOD or anisotropy yet.
- **Pixel-engine interrupts**: `PE_INTSR` (0x100A) has enables in bits 0/1 and flags in bits 2/3 (cleared by writing 1). The SDK's token handler writes the whole register back, which would also clear a pending finish flag, so a finish is raised only once the token flag was acknowledged.
- **Dev outputs**: `FEPOR_DUMP_FRAMES=dir` (+ `FEPOR_DUMP_EVERY=n`) writes every n-th presented frame as PNG, `FEPOR_DUMP_TEXTURES=dir` every decoded texture, `FEPOR_TRACE=gx` per-frame draw counts, `FEPOR_TRACE=gxdraw` per-draw state, `FEPOR_CULL=1` enables face culling (winding not validated), `FEPOR_NO_VIDEO=1` uses the counting sink.
- **Not yet**: an on-screen window (winit or SDL3 surface on the main thread, guest on another), a GPU-independent software path, and a comparison against Dolphin frame dumps.

Related device work done for this milestone: SI with one scripted controller (`FEPOR_PAD=A@400+8,...`), the DSP boot answering `DSP_INIT`/AX sync mails, AI DMA interrupts at the programmed buffer rate, and ARAM DMA triggered by the low half of the count register.
