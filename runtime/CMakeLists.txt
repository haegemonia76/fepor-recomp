cmake_minimum_required(VERSION 3.25)
project(fepor_runtime LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Runtime library linked with generated code (os, dvd, gx, ax, input, ui land here in later milestones).
# Generated code uses clang extensions (ext_vector_type, musttail): clang is required.
add_library(fepor_runtime STATIC src/runtime.cpp src/fp.cpp)
target_include_directories(fepor_runtime PUBLIC include)
