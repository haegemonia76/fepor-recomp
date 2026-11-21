# 0003 No Nintendo-derived content in the repo

Status: accepted

The user provides the disc image. The tool extracts into a git-ignored output directory and generates code locally on every build. The repo holds only tooling and runtime. Disc readers are abstract: raw ISO/GCM and extracted folder first, CISO/RVZ later. A hash check of GFEP01 guards the version; other regions come later via version tables.
