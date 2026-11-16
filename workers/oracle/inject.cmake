# Injected after project(citra), leaving the stock checkout unchanged.
add_executable(lotus-stock-oracle "${CMAKE_CURRENT_LIST_DIR}/main.cpp")
target_include_directories(lotus-stock-oracle PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/src")
target_link_libraries(lotus-stock-oracle PRIVATE citra_core)
target_compile_features(lotus-stock-oracle PRIVATE cxx_std_20)
target_compile_definitions(lotus-stock-oracle PRIVATE ENABLE_SOFTWARE_RENDERER)
if (WIN32)
    target_link_libraries(lotus-stock-oracle PRIVATE crypt32)
endif()
