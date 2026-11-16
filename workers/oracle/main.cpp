// Thin test driver for the unmodified pinned Azahar core. Not a production worker.
#include <chrono>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <memory>
#include "common/file_util.h"
#include "common/logging/backend.h"
#include "common/settings.h"
#include "core/core.h"
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

int main(int argc, char** argv) {
    if (argc != 3) {
        std::cerr << "usage: lotus-stock-oracle <homebrew.3dsx> <private-root>\n";
        return 2;
    }
    std::filesystem::create_directories(argv[2]);
    FileUtil::SetUserPath(std::string{argv[2]} + "/");
    Common::Log::Initialize();
    Common::Log::Start();
    Settings::values.graphics_api = Settings::GraphicsAPI::Software;
    Settings::values.is_new_3ds = false;
    Settings::values.output_type = AudioCore::SinkType::Null;
    Settings::values.use_gdbstub = false;
    Settings::values.init_ticks_type = Settings::InitTicks::Fixed;
    Settings::values.init_ticks_override = 0;
    Settings::values.init_clock = Settings::InitClock::FixedTime;
    Settings::values.init_time = 946684800;
    for (const auto& module : Service::service_module_map) {
        Settings::values.lle_modules.emplace(module.name, false);
    }
    auto& system = Core::System::GetInstance();
    Frontend::RegisterDefaultApplets(system);
    system.RegisterImageInterface(std::make_shared<Frontend::ImageInterface>());
    OracleWindow window;
    auto result = system.Load(window, argv[1]);
    std::cout << "load_status=" << static_cast<unsigned>(result) << std::endl;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(20);
    if (result == Core::System::ResultStatus::Success) {
        while (window.frames < 60 && std::chrono::steady_clock::now() < deadline) {
            result = system.RunLoop();
            if (result != Core::System::ResultStatus::Success) break;
        }
        std::cout << "presented_frames=" << window.frames << " result=" << static_cast<unsigned>(result) << std::endl;
        const auto& renderer = static_cast<const SwRenderer::RendererSoftware&>(system.GPU().Renderer());
        for (const auto& [name, id] : {std::pair{"top.rgba", VideoCore::ScreenId::TopLeft},
                                     std::pair{"bottom.rgba", VideoCore::ScreenId::Bottom}}) {
            const auto& screen = renderer.Screen(id);
            std::ofstream output{std::filesystem::path{argv[2]} / name, std::ios::binary};
            output.write(reinterpret_cast<const char*>(screen.pixels.data()), screen.pixels.size());
            std::cout << name << " source_width=" << screen.width << " source_height=" << screen.height << '\n';
        }
    }
    if (system.IsPoweredOn()) system.Shutdown();
    Common::Log::Stop();
    return result == Core::System::ResultStatus::Success && window.frames >= 60 ? 0 : 1;
}
