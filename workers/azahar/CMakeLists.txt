# Lotus native Azahar worker.
#
# The worker is a separate OS process that runs the pinned Azahar core behind
# a minimal frontend. It knows nothing about GPUI, MCP or agents.
#
# One configured build directory per Azahar branch. Never build the
# integration and the experimental branch with the same CMake cache.
#
#   cmake -S workers/azahar -B build/azahar-integration \
#         -DLOTUS_AZAHAR_ROOT=$PWD/vendor/azahar \
#         -DCMAKE_BUILD_TYPE=Release
#   cmake --build build/azahar-integration
#
# Without LOTUS_AZAHAR_ROOT the worker builds as a protocol stub that prints
# its handshake and exits. That stub is useful to check the ABI and the
# header, and it is not an emulator.

cmake_minimum_required(VERSION 3.25)
project(lotus-azahar-worker LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

add_executable(lotus-worker
    src/worker_main.cpp
)

target_include_directories(lotus-worker PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/include
)

if(DEFINED LOTUS_AZAHAR_ROOT AND EXISTS "${LOTUS_AZAHAR_ROOT}/CMakeLists.txt")
    message(STATUS "Building the worker against Azahar at ${LOTUS_AZAHAR_ROOT}")
    add_subdirectory(${LOTUS_AZAHAR_ROOT} ${CMAKE_BINARY_DIR}/azahar EXCLUDE_FROM_ALL)
    target_compile_definitions(lotus-worker PRIVATE LOTUS_WITH_AZAHAR)
    # The Azahar core target is named by its own build; the integration
    # branch wires the real target here.
    if(TARGET citra_core)
        target_link_libraries(lotus-worker PRIVATE citra_core)
    else()
        message(WARNING "Azahar was added, but the core target name has not been pinned yet. "
                        "The worker will be built without the native core.")
    endif()
else()
    message(STATUS "LOTUS_AZAHAR_ROOT is not set: building the protocol stub only")
endif()
