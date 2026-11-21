# 0014 DVD, THP and CPU-wide state

Status: accepted

- **DVD**: no function-level HLE. The DI registers execute the SDK's commands (inquiry, read) against the abstract `Disc`, so the lifted DVD driver and everything above it (file system, async reads, streaming) run unchanged. `fepor-recomp ls` prints disc offsets so `FEPOR_TRACE=di` reads can be matched to files.
- **THP**: the Dolphin SDK's THP player is guest code (three guest threads: read, video decode, audio decode). Nothing is re-implemented; libjpeg-turbo is not needed. It plays `opening.thp` to the title screen once the CPU state it relies on exists:
  - **Locked-cache DMA**: `mtspr DMA_L` with the trigger bit runs `fepor_lc_dma` (memory <-> the 16 KiB locked cache at 0xE0000000, 32-byte blocks, 0 = 128).
  - **CPU-wide SPRs**: HID0/1/2, WPAR, DMA_U/L and L2CR belong to the CPU, not to an OSContext. Each guest thread has its own `Context`, so they are copied into the running thread on every switch/spawn (`hle::CPU_SPRS`). Before this, the decode thread saw HID2[LCE] clear and THPVideoDecode returned an error.
  - **Decrementer**: `mtdec`/`mfdec` are runtime services (exception 8 delivered at safe points through the same path as external interrupts), for OS alarms.
- **Audio stays silent** (M5.1): the AX ucode is answered with the minimum protocol (init, `0xBABE` frame -> `DSP_YIELD`), so the SDK's audio frame loop keeps running at 5 ms.
- **Diagnostics added while getting here**: `FEPOR_WATCH=<addrs>` (arguments on function entry), `FEPOR_RECENT`, first-execution order and `FEPOR_MARK_DI` (functions first run after a DVD offset) in the watchdog report, `FEPOR_GX_LOG=<frame>` (command stream of one frame), `FEPOR_TRACE=gxstats`.
