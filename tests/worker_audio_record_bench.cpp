#include <algorithm>
#include <chrono>
#include <cstdint>
#include <filesystem>
#include <iostream>
#include <numeric>
#include <string_view>
#include <thread>
#include <vector>

namespace Bench {
using Clock = std::chrono::steady_clock;
using Ns = std::chrono::nanoseconds;
using namespace std::chrono_literals;
constexpr int calls = 1000;
bool slow = false;
std::vector<Ns> records, waits, holds, writes;

struct Hold {
    Clock::time_point acquired;
    ~Hold() { holds.push_back(Clock::now() - acquired); }
};

template <class Write>
void MeasureWrite(Write write) {
    const auto start = Clock::now();
    if (slow)
        std::this_thread::sleep_for(12ms);
    write();
    writes.push_back(Clock::now() - start);
}

void Print(const char* name, std::vector<Ns>& times) {
    std::sort(times.begin(), times.end());
    std::cout << '"' << name << "\":{\"count\":" << times.size();
    if (!times.empty()) {
        const auto at = [&](std::size_t percent) {
            return times[(times.size() * percent + 99) / 100 - 1].count() / 1000.0;
        };
        const auto total = std::accumulate(times.begin(), times.end(), Ns{});
        std::cout << ",\"avg_us\":" << total.count() / (1000.0 * times.size())
                  << ",\"p95_us\":" << at(95) << ",\"p99_us\":" << at(99)
                  << ",\"max_us\":" << at(100);
    }
    std::cout << '}';
}
} // namespace Bench

#include "audio_core/worker_sink.h"

int main(int argc, char** argv) {
    if (argc != 3)
        return 2;
    const std::string_view mode = argv[1];
    if (mode != "off" && mode != "on" && mode != "slow")
        return 2;
    const bool capture = mode != "off";
    Bench::slow = mode == "slow";
    for (auto* times : {&Bench::records, &Bench::waits, &Bench::holds, &Bench::writes})
        times->reserve(Bench::calls);

    using AudioCore::WorkerSink;
    if (capture)
        WorkerSink::CaptureStart(std::filesystem::path{argv[2]});
    std::uint64_t drained = 0;
    std::jthread status{[&](std::stop_token done) {
        while (!done.stop_requested()) {
            drained = WorkerSink::Drain().samples;
            std::this_thread::sleep_for(std::chrono::milliseconds{16});
        }
    }};
    AudioCore::StereoFrame16 frame{};
    for (std::size_t i = 0; i < frame.size(); ++i)
        frame[i] = {static_cast<s16>(i), static_cast<s16>(-int(i))};

    const auto run_start = Bench::Clock::now();
    for (int i = 0; i < Bench::calls; ++i) {
        // Pace relative to the start of each call so both variants see the same
        // inter-call gap on fast storage and e.g. never try to catch a 12 ms stall.
        const auto start = Bench::Clock::now();
        WorkerSink::Record(frame.data(), frame.size());
        Bench::records.push_back(Bench::Clock::now() - start);
        std::this_thread::sleep_until(start + std::chrono::milliseconds{1});
    }
    const auto run = Bench::Clock::now() - run_start;
    status.request_stop();
    status.join();
#ifdef BENCH_QUEUED_CAPTURE
    AudioCore::WorkerWavCapture::Status sealed;
    if (capture) {
        sealed = WorkerSink::CaptureStop();
        if (!sealed.Published())
            return 1;
    }
#else
    if (capture)
        WorkerSink::CaptureStop();
#endif

    const auto stats = WorkerSink::Drain();
    if (stats.samples != Bench::calls * frame.size() || drained > stats.samples ||
        Bench::records.size() != Bench::waits.size() || Bench::records.size() != Bench::holds.size())
        return 1;
    std::cout << '{';
    Bench::Print("record", Bench::records);
    std::cout << ',';
    Bench::Print("lock_wait", Bench::waits);
    std::cout << ',';
    Bench::Print("lock_hold", Bench::holds);
    std::cout << ',';
    Bench::Print("pcm_write", Bench::writes);
#ifdef BENCH_QUEUED_CAPTURE
    std::cout << ",\"dropped_frames\":" << sealed.dropped_frames
              << ",\"queue_peak_bytes\":" << sealed.queue_peak_bytes;
#endif
    std::cout << ",\"producer_wall_ms\":" << std::chrono::duration_cast<Bench::Ns>(run).count() / 1e6
              << ",\"samples\":" << stats.samples << ",\"hash\":\"" << stats.hash << "\"}\n";
}
