// Lotus native worker entry point.
//
// Two build shapes exist:
//
//   * Without the Azahar core linked (the default on a fresh checkout) the
//     worker starts, reports its handshake and exits. It is a stub, not an
//     emulator. Nothing about it should be mistaken for emulation.
//
//   * With the Azahar core linked (LOTUS_WITH_AZAHAR) the worker builds a
//     minimal frontend on top of the pinned core. That path is a proposal
//     until the first integration PR lands.
//
// The worker must stay free of GPUI, MCP and agent concepts.

#include <cstdio>
#include <cstring>

#include "lotus_protocol.h"

#ifdef LOTUS_WITH_AZAHAR
// The native core is included here once the integration branch exists:
//   #include "core/system.h"
//   #include "video_core.h"
#endif

namespace {

void PrintHandshake() {
    lotus::WorkerHello hello{};
    hello.struct_size = static_cast<uint32_t>(sizeof(lotus::WorkerHello));
    hello.abi_version = lotus::kAbiVersion;
    hello.protocol_version_major = lotus::kProtocolVersionMajor;
    hello.protocol_version_minor = lotus::kProtocolVersionMinor;
    std::memset(hello.worker_commit, 0, sizeof(hello.worker_commit));
    hello.max_memory_read_bytes = 1024u * 1024u;
    hello.max_trace_bytes = 512u * 1024u * 1024u;

    std::printf("lotus-worker protocol %u.%u abi %u\n",
                hello.protocol_version_major, hello.protocol_version_minor, hello.abi_version);
}

}  // namespace

int main(int argc, char** argv) {
    (void)argc;
    (void)argv;

    PrintHandshake();

#ifdef LOTUS_WITH_AZAHAR
    std::printf("native core linked: the worker frontend is still a stub\n");
#else
    std::printf("no native core linked: this build is a protocol stub only\n");
#endif

    return 0;
}
