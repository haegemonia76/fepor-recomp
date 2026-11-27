# 0045 Playtime and Discord rich presence

Status: accepted (implements ticket #45)

**Playtime**
- `<data>/playtime.toml` holds whole seconds per game id (`[seconds] GFEP01 = 7260`). The data folder is the profile (ADR 0010); the game id keeps the EU and Japanese discs apart. `fepor_core::playtime` loads, saves (temporary file + rename) and formats it.
- Counted: wall-clock time while the guest runs and the settings overlay is closed. The overlay is the game's pause (the pad reads neutral, ADR 0020), so time spent in it is not counted. A window without focus keeps counting: the guest still runs. Loading time counts, since it is part of playing.
- A thread in `fepor-game` ticks once a second (`playtime::Counter` hands out whole seconds and carries the rest), publishes the total to the overlay and saves every 10 s and whenever the overlay opens (which is also the way to the Quit page). Closing the window loses at most 10 s.
- A file that does not parse is renamed `playtime.toml.corrupt` and counting restarts at zero.
- Shown on the overlay's Help page and, as a total for the output folder, in the launcher GUI. It is per data folder, not per memory card save: the card holds the game's own saves, which the host cannot attribute reliably.

**Discord rich presence**
- Off by default (`presence.enabled`, Help page checkbox). While off the presence thread only reads the setting: no socket is looked up and nothing is sent. Turning it off while connected clears the activity.
- Own minimal client, no crate: Discord's local IPC (frames of opcode + length + JSON over the `discord-ipc-N` Unix socket, looked up in `XDG_RUNTIME_DIR`, `TMPDIR`, `/tmp` and the Flatpak/Snap subfolders; the `\\?\pipe\discord-ipc-N` named pipe on Windows). Handshake, then `SET_ACTIVITY`. Retries every 15 s while Discord is not running, updates at most every 15 s.
- Sent: the application id, the process id, the start time of this run, and the texts "Fire Emblem: Path of Radiance" and "Playing" or "In the settings menu". Nothing is read from guest memory, so no chapter or scene is shown: the game keeps no single reliable value for it and guessing addresses would break between builds.
- Discord requires the application id of a Discord application. The repository ships none (an application carries a name and pictures that belong to its owner), so `presence.client_id` in `settings.toml` must be filled with one the player created at discord.com/developers; until then the option does nothing.
- Tested against a fake IPC socket (handshake, refusal, absent client, activity JSON). Not tested against a real Discord client.
