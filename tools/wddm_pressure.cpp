#include <windows.h>
#include <cstdlib>
#include <iostream>
#include <vector>
#include <d3d12.h>
#include <dxgi1_4.h>
#include <wrl/client.h>

using Microsoft::WRL::ComPtr;

// Hold a bounded dedicated allocation until the acceptance runner closes stdin.
static void Check(HRESULT result, const char* operation) {
    if (FAILED(result)) {
        std::cerr << operation << " failed with HRESULT " << std::hex << result << '\n';
        std::exit(1);
    }
}

int main(int argc, char** argv) {
    if (argc != 2) return 2;
    const unsigned long mib = std::strtoul(argv[1], nullptr, 10);
    if (mib == 0 || mib > 2048 || mib % 128 != 0) return 2;

    ComPtr<IDXGIFactory4> factory;
    Check(CreateDXGIFactory1(IID_PPV_ARGS(&factory)), "create DXGI factory");
    ComPtr<IDXGIAdapter1> adapter;
    Check(factory->EnumAdapters1(0, &adapter), "select default adapter");
    DXGI_ADAPTER_DESC1 adapter_info{};
    Check(adapter->GetDesc1(&adapter_info), "query adapter");
    ComPtr<IDXGIAdapter3> adapter_budget;
    Check(adapter.As(&adapter_budget), "query adapter budget interface");
    ComPtr<ID3D12Device> device;
    Check(D3D12CreateDevice(adapter.Get(), D3D_FEATURE_LEVEL_11_0, IID_PPV_ARGS(&device)),
          "create D3D12 device");
    DXGI_QUERY_VIDEO_MEMORY_INFO before{};
    Check(adapter_budget->QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &before),
          "query initial memory budget");
    if (before.Budget < before.CurrentUsage + (mib + 1024ull) * (1ull << 20)) return 3;

    const D3D12_HEAP_PROPERTIES heap{D3D12_HEAP_TYPE_DEFAULT, D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
                                     D3D12_MEMORY_POOL_UNKNOWN, 1, 1};
    const D3D12_RESOURCE_DESC buffer{D3D12_RESOURCE_DIMENSION_BUFFER, 0, 128ull << 20, 1, 1, 1,
                                     DXGI_FORMAT_UNKNOWN, {1, 0}, D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                                     D3D12_RESOURCE_FLAG_NONE};
    std::vector<ComPtr<ID3D12Resource>> resources(mib / 128);
    for (auto& resource : resources) {
        Check(device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &buffer,
                                              D3D12_RESOURCE_STATE_COMMON, nullptr,
                                              IID_PPV_ARGS(&resource)), "allocate resident buffer");
    }
    DXGI_QUERY_VIDEO_MEMORY_INFO after{};
    Check(adapter_budget->QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &after),
          "query loaded memory budget");
    std::cout << "{\"vendor_id\":" << adapter_info.VendorId
              << ",\"device_id\":" << adapter_info.DeviceId << ",\"requested_mib\":" << mib
              << ",\"before_bytes\":" << before.CurrentUsage << ",\"after_bytes\":" << after.CurrentUsage
              << ",\"budget_bytes\":" << after.Budget << "}" << std::endl;
    std::cin.get();
}
