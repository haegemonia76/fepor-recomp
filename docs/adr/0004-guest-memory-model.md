# 0004 Guest memory model

Status: accepted

One contiguous big-endian host buffer (24 MiB MEM1, 16 MiB ARAM), byte-swapped on each access, address masked with `& 0x01FFFFFF` (cached 0x80000000 and uncached 0xC0000000 aliases). Hardware registers (0xCC00xxxx, including the write-gather pipe) are routed through dedicated functions.
