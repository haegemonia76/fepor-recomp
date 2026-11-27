# 0041 MSAA and anisotropic filtering

Status: accepted

## Context

Edges drawn at the GameCube's resolution (or at the internal resolution of ADR 0019) alias, and textures on
slanted surfaces blur. Both are optional quality settings. The EFB model of ADR 0013 (EFB copies and
the presented frames are taken from one colour texture) must not change, and with the options off the output
must stay exactly what it was.

## Decisions

- **Settings.** `[video] msaa` (1 = off, 2, 4, 8) and `[video] anisotropy` (1 = off, 2, 4, 8, 16), on the
  overlay's Video page; `FEPOR_MSAA` / `FEPOR_ANISO` override them for one run. Defaults: off.
- **Single-sample EFB stays the truth.** With MSAA the draws go to multisampled colour and depth targets and
  every render pass (the draw pass and the clear pass) resolves into the ordinary single-sample `efb`
  texture. EFB copies to textures, the copy to the XFB, readbacks, frame dumps, the interpolation copy and
  the presenter therefore read resolved pixels and are unchanged. The multisampled colour is stored between
  passes (`LoadOp::Load` keeps the samples), so a pass continues the same picture. The interpolation target
  (`alt`, ADR 0019) has its own multisampled pair, so replayed images are antialiased the same way.
- **Internal resolution.** Targets are `scale` times the EFB and multisampled on top of that; both
  are rebuilt together. Memory grows with `scale^2 * samples` (x4 and 8 samples: about 175 MB per colour and
  per depth target, twice with the interpolation target).
- **Fallback.** At start the adapter is asked which of 2/4/8 it can multisample *and resolve* for `Rgba8Unorm`
  and `Depth32Float`; a request uses the largest supported count not above it (1 when none) and says so on
  stderr. The overlay always offers 2/4/8 and says that the next lower is used.
- **Live.** A change of MSAA rebuilds the targets (like a change of internal resolution), drops the pipeline
  cache (pipelines are made for one sample count) and clears the target; the current frame shows
  one cleared frame. Anisotropy only clears the sampler and bind group caches.
- **Anisotropy.** `anisotropy_clamp` on the guest texture samplers. wgpu allows it only when mag, min and
  mipmap filters are all linear, so samplers whose guest min or mag filter is nearest keep it off (that is
  also what the game asked for), and the mip filter is set to linear when it is on (textures have a single
  level: ADR 0013 says there are no mipmaps). With one level the hardware takes several taps along the
  longer axis of the footprint at level 0, which is what sharpens slanted surfaces.
- **Off is exact.** With both off the targets, pipelines and samplers are created with the same values as
  before (sample count 1, no resolve target, default sampler); dumps are byte-identical to the base.

## Consequences

- MSAA costs fill rate and memory; it smooths geometry edges, not alpha-tested texture edges or shader
  aliasing (the game's own textures and 2D art are untouched).
- Verified on one AMD RX 7800 XT with Vulkan via headless dumps only.
