# 0011 Single executable for players

Status: accepted (implemented by ADR 0024)

Players get one program per platform, with no development tools to install. Everything runs inside it: disc reading, analysis, code generation, the game runtime, the GUI and the CLI.

Consequences:

- **Production backend**: the lifter emits native code in-process (Cranelift) from the same `Analysis`, at setup time, cached under `{data}`. The generated C++ stays as the *development and debugging backend* (readable output, runs the instruction vectors with clang). The lifter gets a backend interface with two implementations; both must pass the same vectors (ADR 0008).
- **Runtime in Rust** (`crates/runtime`): memory, hardware registers, scheduler, HLE, video and audio, exposed to generated code through the C ABI declared in `runtime/include/fepor/ppc.h` (`fepor_*`). The only C++ in the runtime is `runtime/src/fp.cpp` (fres/frsqrte, Dolphin-derived), compiled into the executable at *our* build time.
- **Development flow**: `tools/dev-run.sh <disc>` generates C++ with call tracing, builds it with clang, links it with the Rust runtime (`crates/game`) and runs it. Developers need clang/CMake/Ninja; players do not.
- `prepare` in the RecompLauncher profile becomes "analyse, generate native code, write the cache" inside the executable; exit code 20 (toolchain missing) disappears with the C++ backend in releases.
- **Risks**: code quality of Cranelift versus clang `-O2` (measure), setup time for ~640k instructions (target: under a minute), tail calls and vector operations (Cranelift `return_call`, SIMD or out-of-line helpers for paired singles), Windows/macOS arm64 support.
