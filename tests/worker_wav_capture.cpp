// Isolated Windows acceptance for the native worker WAV writer.
#include "audio_core/worker_wav_capture.h"
#include <array>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <future>
#include <iostream>
#include <semaphore>
#include <span>
#include <stdexcept>
#include <string_view>
#include <vector>

namespace {
using AudioCore::WorkerWavCapture;
using namespace std::chrono_literals;
constexpr unsigned sample_rate = 32728;
using Frame = std::array<std::int16_t, 2>;
constexpr std::size_t block_frames = (16 << 10) / sizeof(Frame);
std::binary_semaphore write_started{0};
std::binary_semaphore unblock_write{0};
std::counting_semaphore<> writes{0};
std::atomic<bool> stalled{false};

void Check(bool condition, const char* message) {
    if (!condition)
        throw std::runtime_error(message);
}

void StallFirstWrite() {
    if (!stalled.exchange(true)) {
        write_started.release();
        // Limit a failing test's lifetime if the expected status transition never
        // occurs.
        if (!unblock_write.try_acquire_for(8s))
            throw std::runtime_error("test WAV writer never released");
    }
}

void CheckHeader(const std::filesystem::path& file, std::size_t pcm_bytes) {
    Check(std::filesystem::file_size(file) == pcm_bytes + 44, "incorrect WAV file size");
    std::ifstream input{file, std::ios::binary};
    std::array<char, 44> header{};
    input.read(header.data(), header.size());
    const auto field = [&](std::size_t offset, std::size_t size) {
        std::uint32_t value = 0;
        for (std::size_t index = 0; index < size; ++index)
            value |= std::uint32_t(static_cast<unsigned char>(header[offset + index]))
                     << (8 * index);
        return value;
    };
    Check(input.good(), "could not read WAV header");
    Check(std::string_view{header.data(), 4} == "RIFF" &&
              std::string_view{header.data() + 8, 8} == "WAVEfmt " &&
              std::string_view{header.data() + 36, 4} == "data",
          "incorrect WAV chunks");
    Check(field(4, 4) == pcm_bytes + 36 && field(40, 4) == pcm_bytes, "incorrect chunk sizes");
    Check(field(16, 4) == 16 && field(20, 2) == 1 && field(22, 2) == 2 &&
              field(24, 4) == sample_rate && field(28, 4) == sample_rate * 4 && field(32, 2) == 4 &&
              field(34, 2) == 16,
          "incorrect stereo PCM format");
}

void CheckFrames(const std::filesystem::path& file, std::span<const Frame> first,
                 std::span<const Frame> second) {
    std::ifstream input{file, std::ios::binary};
    input.seekg(44);
    for (auto segment : {first, second}) {
        for (const auto& expected : segment) {
            Frame actual{};
            input.read(reinterpret_cast<char*>(actual.data()), sizeof(Frame));
            Check(input.good() && actual == expected, "PCM sample order or channel changed");
        }
    }
    Check(input.peek() == EOF, "unexpected PCM after expected prefix");
}

void SlowWriter(const std::filesystem::path& file) {
    WorkerWavCapture capture{StallFirstWrite};
    Check(capture.Start(file, sample_rate).phase == WorkerWavCapture::Phase::Recording,
          "capture did not start");
    Check(!std::filesystem::exists(file), "unfinished WAV was published");
    const std::vector<Frame> first(block_frames, {1234, -4567});
    const std::size_t capacity_frames = WorkerWavCapture::queue_capacity / sizeof(Frame);
    std::vector<Frame> next(capacity_frames + 8);
    for (std::size_t index = 0; index < next.size(); ++index)
        next[index] = {static_cast<std::int16_t>(index), static_cast<std::int16_t>(~index)};
    capture.Push(first.data(), first.size());
    Check(write_started.try_acquire_for(2s), "writer did not reach the stall");
    auto producer = std::async(std::launch::async, [&] {
        const auto start = std::chrono::steady_clock::now();
        capture.Push(next.data(), next.size());
        capture.Push(next.data(), 4);
        return std::chrono::steady_clock::now() - start;
    });
    if (producer.wait_for(1s) != std::future_status::ready) {
        unblock_write.release();
        throw std::runtime_error("producer waited for the filesystem");
    }
    const auto producer_time = producer.get();
    const auto stop_time = std::chrono::steady_clock::now();
    const auto pending = capture.Stop();
    const auto stop_elapsed = std::chrono::steady_clock::now() - stop_time;
    unblock_write.release();
    Check(pending.phase == WorkerWavCapture::Phase::Finalizing && !pending.Published(),
          "stalled capture should reach the control deadline as finalizing");
    Check(stop_elapsed < 3s, "stalled WAV stop exceeded its control deadline");
    Check(pending.queued_bytes == WorkerWavCapture::queue_capacity, "incorrect queue occupancy");
    const auto done = capture.Stop();
    Check(done.Published() && done.queue_overflow && !done.limit_reached,
          "overflow did not publish a truncated WAV");
    Check(done.dropped_frames == 12 && done.dropped_blocks == 2, "wrong overflow accounting");
    Check(done.pcm_bytes == (first.size() + capacity_frames) * sizeof(Frame),
          "wrong output prefix");
    Check(done.queue_peak_bytes == WorkerWavCapture::queue_capacity, "wrong queue high-water mark");
    CheckHeader(file, done.pcm_bytes);
    CheckFrames(file, first, std::span{next}.first(capacity_frames));

    Check(capture.Start(file, sample_rate).dropped_frames == 0, "capture retained old counters");
    capture.Push(next.data(), 4);
    Check(capture.Stop().Published(), "could not replace output on repeated capture");
    CheckHeader(file, 4 * sizeof(Frame));
    CheckFrames(file, std::span{next}.first(4), {});
    std::cout << "stalled_write_push_ns="
              << std::chrono::duration_cast<std::chrono::nanoseconds>(producer_time).count()
              << " stop_ms="
              << std::chrono::duration_cast<std::chrono::milliseconds>(stop_elapsed).count()
              << '\n';
}

void SizeLimit(const std::filesystem::path& file) {
    WorkerWavCapture capture{[] { writes.release(); }};
    std::vector<Frame> block(block_frames, {71, -15});
    Check(capture.Start(file, sample_rate).phase == WorkerWavCapture::Phase::Recording,
          "limit test capture did not start");
    constexpr auto target_frames = WorkerWavCapture::pcm_limit / sizeof(Frame);
    for (std::size_t written = 0; written < target_frames; written += block.size()) {
        capture.Push(block.data(), block.size());
        Check(writes.try_acquire_for(2s), "writer stopped draining the limit test");
    }
    const auto result = capture.Stop();
    Check(result.Published() && result.limit_reached && !result.queue_overflow,
          "WAV file limit failed");
    Check(result.dropped_frames == block_frames - target_frames % block_frames &&
              result.dropped_blocks == 1,
          "wrong file-limit counters");
    CheckHeader(file, WorkerWavCapture::pcm_limit);
}

void OutputFailure(const std::filesystem::path& file) {
    WorkerWavCapture capture;
    auto blocked = file;
    blocked += ".part";
    Check(std::filesystem::create_directory(blocked), "could not set up blocked output");
    const auto result = capture.Start(file, sample_rate);
    Check(result.phase == WorkerWavCapture::Phase::Failed && result.error && !result.Published(),
          "unwritable WAV unexpectedly started");
    Check(std::filesystem::is_directory(blocked), "writer removed a preexisting blocker");
    Check(!std::filesystem::exists(file), "failed WAV was published");
    Check(std::filesystem::remove(blocked), "could not remove the test blocker");
    Check(std::filesystem::create_directory(file), "could not block WAV publication");
    Check(capture.Start(file, sample_rate).phase == WorkerWavCapture::Phase::Recording,
          "publication test did not start");
    const auto unpublished = capture.Stop();
    Check(unpublished.phase == WorkerWavCapture::Phase::Failed && unpublished.error &&
              !unpublished.Published(),
          "WAV publication failure was lost");
    Check(!std::filesystem::exists(blocked), "failed publication left a pending WAV");
    Check(std::filesystem::remove(file), "could not remove the publication blocker");
    Check(capture.Start(file, sample_rate).phase == WorkerWavCapture::Phase::Recording,
          "failed writer prevented a subsequent capture");
    Check(capture.Stop().Published(), "empty capture did not finalize");
    CheckHeader(file, 0);
}
} // namespace

int main(int argc, char** argv) {
    try {
        Check(argc == 2, "supply a new output directory");
        const std::filesystem::path root{argv[1]};
        Check(std::filesystem::create_directory(root), "output directory must be new");
        SlowWriter(root / "stalled.wav");
        SizeLimit(root / "limit.wav");
        OutputFailure(root / "failure.wav");
        std::cout << "WAV prefix, queue overflow, size limit, and recovery passed\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
