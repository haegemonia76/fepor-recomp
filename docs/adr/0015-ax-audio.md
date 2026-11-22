
## Update: polyphase resampling

Voices with source type 0 were resampled linearly. Against Dolphin's own AX output (a one-shot sound effect
alone on screen, recorded in both), linear interpolation left an error of about 20 % of the signal, mostly
above 4 kHz (image frequencies: the hiss heard on dialogue sound effects). Fitting four taps per phase to
Dolphin's output showed the hardware filters: sets of 128 four-tap phases, one per cutoff (0.5, 0.8, 1.0 of the
output rate), window centred between the middle two taps. The table itself is a DSP ROM dump (`dsp_coef.bin`)
and is not shipped; each set is a Hamming-windowed sinc whose parameters were fitted to it (max deviation
under 1 %) and is regenerated at startup. The set is the smallest cutoff covering the voice's ratio; ratios above
1.0 stay linear. Result on two isolated effects: error 234/274 -> 12/15 (correlation 0.99995).

Still open: a frame-edge discontinuity in busy scenes (about 7 % of the 5 ms frames, none in Dolphin's recording
of the same game); it is not caused by the aux return, compressor, ramps, stopped voices or stream voices.
`FEPOR_AX_DUMP_VOICE=<hex PB address>` prints a voice's resampler input and output for offline fits.

## Update: the crackle was replayed frames (found by comparing with Dolphin)

Frame-edge discontinuities (7-10 % of the 5 ms frames in dialogue scenes, 0.1 % in Dolphin's recording) turned
out to be **frames played twice**: 15 % of the frames in the music segment were exact copies of the previous
one. The sink was fed from the AI DMA clock, which re-reads the DMA start address every period; when the guest
answered late the clock replayed the same buffer. A buffer is now pushed only if the guest programmed one since
the last period (`Dsp::ai_start_written`); a late guest leaves a gap instead of a replay.

Feeding the sink from the ucode's own output instead was tried and is wrong: movie audio (THP) is mixed into the
AI DMA buffer by the CPU after the ucode wrote it, so the AI buffer is the audio. `FEPOR_TRACE=audio` prints the
AX frames per second, the AI buffers pushed per second (200 needed) and the peak of each second.

The guest answered late because the renderer rebuilt the WGSL source and its hash for every draw (about 15 % of
the time in the busy scenes): AX production fell to 163 frames/s against 200 needed. Shader sources are now cached
by `tev::shader_key`; production is 200 frames/s throughout. After both changes: edge glitches 0.03-0.08 %,
no duplicated frame, audio length equal to wall time.

`FEPOR_AX_DUMP_CPU=<file>` records the main bus the ucode uploads and the buffers the CPU hands back.
