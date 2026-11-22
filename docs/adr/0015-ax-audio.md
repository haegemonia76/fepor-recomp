
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
