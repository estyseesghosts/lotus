// Thin test driver for the unmodified pinned Azahar core. Not a production worker.
#include <charconv>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <memory>
#include <string_view>
#include "common/file_util.h"
#include "common/logging/backend.h"
#include "common/settings.h"
#include "core/core.h"
#include "core/core_timing.h"
#include "core/frontend/applets/default_applets.h"
#include "core/frontend/emu_window.h"
#include "core/frontend/image_interface.h"
#include "core/hle/service/service.h"
#include "video_core/gpu.h"
#include "video_core/renderer_base.h"
#include "video_core/renderer_software/renderer_software.h"

class OracleWindow final : public Frontend::EmuWindow {
public:
    OracleWindow() { UpdateCurrentFramebufferLayout(400, 480); }
    void PollEvents() override { ++frames; }
    unsigned frames = 0;
};

bool ParseUnsigned(std::string_view text, unsigned& result) {
    const auto end = text.data() + text.size();
    const auto parsed = std::from_chars(text.data(), end, result);
    return parsed.ec == std::errc{} && parsed.ptr == end;
}

int main(int argc, char** argv) {
    const bool benchmark = argc == 9 && std::string_view{argv[3]} == "--benchmark";
    unsigned warmup = 0, frames = 60, frame_limit = 100, new_3ds = 0, deterministic_async = 0;
    if (argc != 3 && (!benchmark || !ParseUnsigned(argv[4], warmup) ||
                     !ParseUnsigned(argv[5], frames) || !ParseUnsigned(argv[6], frame_limit) ||
                     !ParseUnsigned(argv[7], new_3ds) || !ParseUnsigned(argv[8], deterministic_async) ||
                     warmup > 6000 || frames == 0 || frames > 6000 || frame_limit > 1000 ||
                     new_3ds > 1 || deterministic_async > 1)) {
        std::cerr << "usage: lotus-stock-oracle <homebrew.3dsx> <private-root> "
                     "[--benchmark <warmup> <frames> <frame-limit> <new-3ds 0|1> <deterministic-async 0|1>]\n";
        return 2;
    }
    std::filesystem::create_directories(argv[2]);
    FileUtil::SetUserPath(std::string{argv[2]} + "/");
    Common::Log::Initialize();
    Common::Log::Start();
    Settings::values.graphics_api = Settings::GraphicsAPI::Software;
    Settings::values.is_new_3ds = new_3ds != 0;
    Settings::values.output_type = AudioCore::SinkType::Null;
    Settings::values.use_gdbstub = false;
    Settings::values.init_ticks_type = Settings::InitTicks::Fixed;
    Settings::values.init_ticks_override = 0;
    Settings::values.init_clock = Settings::InitClock::FixedTime;
    Settings::values.init_time = 946684800;
    if (benchmark) {
        Settings::values.input_type = AudioCore::InputType::Static;
        Settings::values.frame_limit = frame_limit;
        Settings::values.deterministic_async_operations = deterministic_async != 0;
    }
    for (const auto& module : Service::service_module_map) {
        Settings::values.lle_modules.emplace(module.name, false);
    }
    auto& system = Core::System::GetInstance();
    Frontend::RegisterDefaultApplets(system);
    system.RegisterImageInterface(std::make_shared<Frontend::ImageInterface>());
    OracleWindow window;
    using Clock = std::chrono::steady_clock;
    using Seconds = std::chrono::duration<double>;
    const auto loading = Clock::now();
    auto result = system.Load(window, argv[1]);
    const Seconds load_seconds = Clock::now() - loading;
    std::cout << "load_status=" << static_cast<unsigned>(result) << std::endl;
    bool completed = false;
    if (result == Core::System::ResultStatus::Success) {
        const auto run_to = [&](unsigned target) {
            const auto deadline = Clock::now() + std::chrono::seconds(benchmark ? 180 : 20);
            while (window.frames < target && Clock::now() < deadline) {
                result = system.RunLoop();
                if (result != Core::System::ResultStatus::Success) return false;
            }
            return window.frames == target;
        };
        if (run_to(warmup)) {
            if (benchmark) {
                (void)system.GetAndResetPerfStats();
            }
            const auto started = Clock::now();
            completed = run_to(warmup + frames);
            const Seconds measured = Clock::now() - started;
            if (benchmark && completed) {
                const auto perf = system.GetAndResetPerfStats();
                std::ofstream metrics{std::filesystem::path{argv[2]} / "benchmark.txt"};
                metrics << std::setprecision(12) << "load_seconds=" << load_seconds.count() << '\n'
                          << "host_seconds=" << measured.count() << '\n'
                          << "frames=" << window.frames - warmup << '\n'
                          << "guest_ticks=" << system.CoreTiming().GetGlobalTicks() << '\n'
                          << "headless_system_fps=" << perf.system_fps << '\n'
                          << "gpu_seconds_per_frame=" << perf.time_gpu << '\n'
                          << "svc_seconds_per_frame=" << perf.time_hle_svc << '\n'
                          << "ipc_seconds_per_frame=" << perf.time_hle_ipc << '\n'
                          << "swap_seconds_per_frame=" << perf.time_swap << '\n'
                          << "graphics_api=" << static_cast<unsigned>(Settings::values.graphics_api.GetValue()) << '\n'
                          << "is_new_3ds=" << Settings::values.is_new_3ds.GetValue() << '\n'
                          << "deterministic_async_operations=" << Settings::values.deterministic_async_operations.GetValue() << '\n'
                          << "init_clock=" << static_cast<unsigned>(Settings::values.init_clock.GetValue()) << '\n'
                          << "init_time=" << Settings::values.init_time.GetValue() << '\n'
                          << "init_ticks_type=" << static_cast<unsigned>(Settings::values.init_ticks_type.GetValue()) << '\n'
                          << "init_ticks_override=" << Settings::values.init_ticks_override.GetValue() << '\n'
                          << "frame_limit=" << Settings::values.frame_limit.GetValue() << '\n'
                          << "resolution_factor=" << Settings::values.resolution_factor.GetValue() << '\n'
                          << "use_cpu_jit=" << Settings::values.use_cpu_jit.GetValue() << '\n'
                          << "cpu_clock_percentage=" << Settings::values.cpu_clock_percentage.GetValue() << '\n'
                          << "input_type=" << static_cast<unsigned>(Settings::values.input_type.GetValue()) << '\n'
                          << "output_type=" << static_cast<unsigned>(Settings::values.output_type.GetValue()) << '\n';
                metrics.close();
                completed = metrics.good();
            }
        }
        std::cout << "presented_frames=" << window.frames << " result=" << static_cast<unsigned>(result) << std::endl;
        const auto& renderer = static_cast<const SwRenderer::RendererSoftware&>(system.GPU().Renderer());
        for (const auto& [name, id] : {std::pair{"top.rgba", VideoCore::ScreenId::TopLeft},
                                     std::pair{"bottom.rgba", VideoCore::ScreenId::Bottom}}) {
            const auto& screen = renderer.Screen(id);
            std::ofstream output{std::filesystem::path{argv[2]} / name, std::ios::binary};
            output.write(reinterpret_cast<const char*>(screen.pixels.data()), screen.pixels.size());
            output.close();
            completed &= output.good();
            std::cout << name << " source_width=" << screen.width << " source_height=" << screen.height << '\n';
        }
    }
    if (system.IsPoweredOn()) system.Shutdown();
    Common::Log::Stop();
    return completed ? 0 : 1;
}
