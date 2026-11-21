# 0019 Frame interpolation and render cost

Status: accepted

## Context

M6 needs high frame rates (ADR 0007) and a playable first stage. The first map scene ran at 15 fps: the
renderer made three GPU buffers, two bind groups and one render pass per draw, so the buffer count per
submit (not the guest CPU) dominated the frame. `perf` on a stripped binary blamed a guest function; a
build with symbols showed `wgpu` buffer tracking and the driver's buffer-list handling.

## Decisions

- **Streaming buffers.** Draw data is appended to three per-submit staging vectors (vertex, index,
  uniform with 256-byte dynamic offsets) and uploaded once in `flush`. Texture bind groups are cached per
  submit by (view pointers, sampler state). Consecutive draws share one render pass; only a clear, an EFB
  copy or `flush` ends it. Map scenes went from 15 to 60 fps.
- **Interpolation by replay (`FEPOR_FPS_MULT=n`, window only).** With `n > 1` the renderer records each
  frame as clears and draws (resolved pipeline, bind group, vertices, uniforms). When frame N completes it
  is not shown at once: `n - 1` images are shown first, each replaying frame N into a second target with
  every object moved `j/n` of the way from where frame N-1 had it, then the real frame N (kept in a
  texture copy). The cost is one frame of latency. Images are paced by `Sink::tick`, called from the
  guest's poll points, so no extra thread is needed; the swapchain uses Mailbox/Immediate so presenting
  never blocks the guest.
- **Matching.** Draw i of N is the same object as draw i of N-1 when shader, vertex/index counts and
  textures match. A draw whose vertices move more than 0.4 NDC units in one frame (cut, teleport) or has
  no counterpart is drawn unmoved. Only clip-space positions are interpolated: colour and texture
  animations stay at the native rate.
- **Measured native rate.** In the opening movie, title, difficulty menu and prologue map scene every
  presented frame differs from the previous one at 60 images/s (`FEPOR_TRACE=gxhash`): the game
  simulates and renders at 60 Hz there. THP movies are 30 fps video.
- Per-thread scheduler condvars (one host thread wake per guest switch instead of all).
- Stale parked threads are dropped when OSCreateThread rewrites the context they were parked on.

- **Internal resolution (`FEPOR_RES=n`, 1..4).** Colour/depth targets and EFB-to-texture copies are
  `n` times the GameCube's 640x528; viewport and scissor scale with them. Geometry and 3D edges get
  sharper; textures and the 2D UI (bitmaps) stay at their native detail. Verified at 3x with a headless
  dump (1824x1344 for the 608x448 frame).
- **Aspect (`FEPOR_ASPECT=fit|stretch|W:H`).** `fit` (default) letterboxes to the frame's 4:3, `stretch`
  fills the window, `16:9` shows the 4:3 picture stretched into a 16:9 box. There is no true widescreen:
  the game's projection is 4:3 and extending it would change what is visible (and break the UI).

## Consequences

- Interpolation is an option for now (env var); the in-game overlay (#20) will own it.
- Objects that change draw order or count between frames are not interpolated for that frame.
- Verified: 120 images/s at `FEPOR_FPS_MULT=2` (240 images per 120 frames, 2.0 s) in the map scene;
  interpolated frames dumped headless (`FEPOR_DUMP_FRAMES`) look right. Not verified on other displays.
