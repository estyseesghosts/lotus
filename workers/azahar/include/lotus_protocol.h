// Lotus worker ABI, shared between the Rust application and the native
// Azahar worker.
//
// This header mirrors `crates/protocol/src/ffi.rs` exactly. Every struct is
// plain data with fixed width members: no std::string, no std::vector, no
// host pointers, no Core::System internals. The static asserts below keep
// the two sides from drifting apart silently.
//
// The Rust side asserts the same sizes in `crates/protocol/src/ffi.rs`.

#ifndef LOTUS_PROTOCOL_H
#define LOTUS_PROTOCOL_H

#include <cstdint>
#include <cstddef>

namespace lotus {

inline constexpr uint16_t kProtocolVersionMajor = 1;
inline constexpr uint16_t kProtocolVersionMinor = 0;
inline constexpr uint32_t kAbiVersion = 1;
inline constexpr size_t kSha256Len = 32;
inline constexpr uint32_t kMaxPayloadLen = 64u * 1024u * 1024u;

inline constexpr uint16_t kScreenTop = 0;
inline constexpr uint16_t kScreenBottom = 1;
inline constexpr uint16_t kEyeLeft = 0;
inline constexpr uint16_t kEyeRight = 1;
inline constexpr uint32_t kPixelFormatRgba8 = 0;

/// Shared memory frame slot header.
struct FrameHeader {
    uint64_t generation;  // monotonic slot generation
    uint64_t seq;         // worker local sequence number
    uint64_t epoch;       // state epoch the frame was produced in
    uint16_t slot;        // index in the shared memory ring
    uint16_t screen;      // kScreenTop or kScreenBottom
    uint16_t eye;         // kEyeLeft or kEyeRight
    uint32_t format;      // kPixelFormatRgba8
    uint32_t stride;      // row stride in bytes
    uint32_t width;       // width in pixels
    uint32_t height;      // height in pixels
};

/// Handshake payload sent by the worker when a client attaches.
struct WorkerHello {
    uint32_t struct_size;
    uint32_t abi_version;
    uint16_t protocol_version_major;
    uint16_t protocol_version_minor;
    uint8_t worker_commit[kSha256Len];  // raw SHA-256 of the Azahar revision
    uint64_t max_memory_read_bytes;
    uint64_t max_trace_bytes;
};

/// Bounded event record header.
struct EventHeader {
    uint32_t struct_size;
    uint32_t category;
    uint64_t sequence;
    uint64_t frame;
    uint64_t guest_tick;  // virtual guest tick, not host time
    uint32_t payload_len;
};

static_assert(sizeof(FrameHeader) == 48, "FrameHeader layout must match lotus-protocol");
static_assert(sizeof(WorkerHello) == 64, "WorkerHello layout must match lotus-protocol");
static_assert(sizeof(EventHeader) == 40, "EventHeader layout must match lotus-protocol");
static_assert(offsetof(FrameHeader, format) == 32, "FrameHeader layout must match lotus-protocol");
static_assert(offsetof(WorkerHello, worker_commit) == 12, "WorkerHello layout must match lotus-protocol");
static_assert(offsetof(EventHeader, sequence) == 8, "EventHeader layout must match lotus-protocol");

}  // namespace lotus

#endif  // LOTUS_PROTOCOL_H
