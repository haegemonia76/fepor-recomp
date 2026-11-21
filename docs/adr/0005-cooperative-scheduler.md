# 0005 Deterministic cooperative scheduler

Status: accepted

Each guest `OSThread` is a fiber on a single host thread; switches occur at OS calls; interrupts (VI retrace, DVD, AI, EXI) are delivered at safe points. Deterministic, which is required for Dolphin trace comparison. Fiber implementation must work on Linux, Windows, macOS (Boost.Context or in-house; not `ucontext`).
