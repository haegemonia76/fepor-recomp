# 0025 Render filters

Status: accepted

## Context

Players want a CRT look (scanlines, mask) and colour correction (gamma, saturation, contrast, brightness).
These must change only what is shown: the EFB, EFB copies and the game's own pictures stay as the game
drew them, and the overlay (ADR 0020) must stay readable.

## Decisions

- **One pass at presentation.** The window presenter's scaling pass (`present.rs`, `Post`) applies the
  filters in its fragment shader while it stretches the XFB/interpolation target into the window. Nothing
  inside the EFB is touched, so the filters compose with interpolation (ADR 0019: every shown image,
  real or interpolated, goes through the same pass), internal resolution and aspect handling. The overlay
  is painted after that pass, so it is never filtered.
- **Order.** brightness, contrast, saturation, gamma (all on the game's gamma-space values, as the surface
  format is not sRGB), then scanlines, then the mask. Scanlines are placed on the game's own lines
  (the copy's height), not the window's pixels, so they stay aligned at any window size; the mask follows
  window pixels (a phosphor triad is a display property).
- **Settings.** `[video.filters]` in `settings.toml` (`scanlines`, `gamma`, `saturation`, `contrast`,
  `brightness`, `mask`, `mask_strength`), edited on the overlay's Video page and applied live through the
  settings generation. The defaults are the identity.
- **Off is exact.** With every value at its default the shader returns the sampled pixel unchanged (a
  uniform flag, no arithmetic), and frame dumps keep their original path: dumps are bit-identical to
  before the filters existed.
- **Frame dumps with filters on** (`FEPOR_DUMP_FRAMES`, no window) render the same pass into a texture of
  at least 3x the GameCube's frame, so that scanlines (several rows per game line) and the mask are
  visible, and draw the overlay on top, unfiltered. Dumps are therefore larger than usual when filtering.

## Consequences

- Scanline darkness is applied on the displayed image: at x1 internal resolution on a small window the
  pattern aliases; it is meant for windows of at least twice the game's height.
- Not verified on other GPUs or on a real window; verified through headless dumps.
