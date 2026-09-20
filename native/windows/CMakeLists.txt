add_library(dcu_platform STATIC
    capture.cpp
    indicator.cpp
    uia.cpp
    watchdog.cpp
    windows_backend.cpp
)

target_compile_features(dcu_platform PRIVATE cxx_std_20)
if (TARGET dcu_contract)
    target_link_libraries(dcu_platform PUBLIC dcu_contract)
endif()
target_include_directories(dcu_platform
    PUBLIC
        ${CMAKE_CURRENT_SOURCE_DIR}/../include
    PRIVATE
        ${CMAKE_CURRENT_SOURCE_DIR}
)

if (MSVC)
    target_compile_definitions(dcu_platform PRIVATE
        UNICODE
        _UNICODE
        NOMINMAX
        WIN32_LEAN_AND_MEAN
        _WIN32_WINNT=0x0A00
    )
endif()

# cppwinrt ships with the Windows SDK. CMake's Visual Studio generator adds
# the UM/shared include roots automatically, but cppwinrt is a separate root.
if (CMAKE_VS_WINDOWS_TARGET_PLATFORM_VERSION)
    set(DCU_WINSDK_ROOT "${CMAKE_WINDOWS_KITS_10_DIR}/Include/${CMAKE_VS_WINDOWS_TARGET_PLATFORM_VERSION}")
    if (EXISTS "${DCU_WINSDK_ROOT}/cppwinrt")
        target_include_directories(dcu_platform PRIVATE "${DCU_WINSDK_ROOT}/cppwinrt")
    endif()
endif()

target_link_libraries(dcu_platform PRIVATE
    advapi32
    d3d11
    dwmapi
    gdi32
    ole32
    oleaut32
    runtimeobject
    shcore
    shell32
    uiautomationcore
    user32
    windowscodecs
)
