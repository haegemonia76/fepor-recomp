# 0042 FSR upscaling

Status: accepted

## Context

A lower internal resolution (ADR 0019) costs less GPU time but a plain bilinear stretch to the window
looks soft. AMD FSR 1 is a spatial upscaler (EASU, edge-adaptive upscale, then RCAS, contrast-adaptive
sharpening) that needs only the image. It is MIT-licensed (FidelityFX FSR 1.0, `ffx_fsr1.h`).

## Decisions

- **Where.** In the scaling pass of ADR 0039 (`present.rs`, `Post::scale`): EASU is a render pass from the
  copied region of the EFB (or interpolation target; so it composes with ADR 0019) to an 8-bit texture of
  the picture's size on the window; the final pass then reads that texture, applies RCAS, then the render
  filters, writing the window. **FSR runs before the filters** (they see the upscaled picture, scanlines
  stay on the game's lines) and **the overlay is painted afterwards** and is never upscaled (ADR 0020).
- **Port.** `crates/video/src/fsr.rs`, WGSL from AMD's reference with its MIT notice in the file header:
  the plain float path, 12 `textureLoad` taps clamped to the copied region (instead of gathers: the region
  is a sub-rectangle of the EFB and must not bleed in its neighbours), no RCAS denoise. Values are
  gamma-space, as FSR 1 expects. The sharpness setting (0..1) maps to FSR's stops of attenuation
  (`2 * (1 - sharpness)`; 0.9 = the reference's 0.2 stops, the default).
- **Presets.** `[video.fsr] quality = off|quality|balanced|performance|ultra` is the ratio of output to
  internal height: 1.5, 1.7, 2.0, 3.0 (AMD's). While on, the internal resolution is *derived*:
  `round(output height / ratio / 528)` clamped to 1..4, where the output height is the picture's height
  on the window (re-evaluated when the window changes; the targets are reallocated when the factor
  changes) or, for headless dumps, 3x the GameCube frame. The internal resolution slider is disabled
  while FSR is on. Because the internal factor is a whole number, a 1080p window gets x1 for every preset
  (and 4K x3/x2/x2/x1), so the presets differ only where the window is large enough.
- **Off is exact.** With `quality = off` the shader takes the old sampling path (a uniform flag) and
  dumps keep their original path.
- **Cost on the stats HUD.** With `TIMESTAMP_QUERY` available the device is created with it and the
  scaling pass(es) are timed; the HUD shows "Scaling pass x ms on the GPU (FSR, filters)". Measured on the
  RX 7800 XT through headless dumps (1824x1344 output): filters alone 0.05 ms, FSR ultra performance
  (x1 to x3) 0.23 ms, FSR quality (x2 to x3) 0.23 ms, FSR ultra plus scanlines 0.24 ms.
  Single samples at four dumps each; real windows at 4K were not measured.

## FSR 2/3 (temporal): feasibility, not implemented

FSR 2/3 need, per frame: a depth buffer, a per-pixel motion-vector buffer, the sub-pixel jitter applied to
the projection (and its value), exposure and a camera reset signal, all at the internal resolution.

- **Depth**: exists (`Depth32Float` EFB depth), but is only a render attachment; it would need
  `TEXTURE_BINDING`, and the game's own depth tricks (EFB copies, depth-copy effects) must not be seen as
  scene depth.
- **Jitter**: the vertex shader receives clip-space positions already transformed on the CPU by the
  emulated XF matrices (`RecDraw.verts`), so adding a per-frame sub-pixel offset to x/y (times w) is
  cheap and exact for 3D draws; 2D UI draws (orthographic) must be excluded or they would shimmer.
- **Motion vectors**: the interpolation replay (ADR 0019) already pairs each draw with its counterpart in
  the previous frame and has both clip-space positions per vertex, which is exactly the screen motion
  (camera and object). It would need a second colour attachment in every pipeline (previous position as
  a vertex attribute) and a matching that survives what the heuristic drops today: draws without a
  counterpart (text that grows, effects, cuts) would have zero or wrong vectors, and render-to-texture
  effects and EFB copies break the history.
- **Cost/benefit**: the FidelityFX FSR 2/3 SDK is MIT-licensed but is a chain of several compute passes
  with no WGSL port, and the 2D menus and map UI gain nothing from temporal upscaling. So it is
  *possible in principle* for the 3D passes only, at the price of a motion-vector attachment in every
  pipeline, a jitter path, history management across cuts and a large shader port: a project of its own,
  not attempted here and not needed for FSR 1.

## Consequences

- FSR 1 amplifies aliasing it is given and, being spatial, cannot recover detail textures never had: the
  2D UI is already native-resolution bitmaps, and RCAS can add faint speckle at text edges (seen in
  the dumps). Gains show on 3D edges and text at x2/x3 internal factors.
- Verified through headless dumps only (AMD RX 7800 XT, Vulkan); no window, no other GPUs or platforms.
  Where the device has no timestamp queries the HUD line is absent.
