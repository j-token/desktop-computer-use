cmake_minimum_required(VERSION 3.20)

# Linux platform implementation for desktop-computer-use.  The platform
# library deliberately keeps every desktop integration optional: a headless
# build can still answer --help/doctor, while an installed desktop gets the
# strongest backend available on that machine.
find_package(PkgConfig QUIET)
option(DCU_HEADLESS_TEST "Build a diagnostic-only backend without desktop development libraries" OFF)

set(DCU_LINUX_SOURCES
    backend.cpp
    eis_client.cpp
)

add_library(dcu_platform STATIC ${DCU_LINUX_SOURCES})
target_compile_features(dcu_platform PRIVATE cxx_std_20)
if(TARGET dcu_contract)
    target_link_libraries(dcu_platform PUBLIC dcu_contract)
endif()
target_include_directories(dcu_platform
    PRIVATE
        ${CMAKE_CURRENT_SOURCE_DIR}/../include
)

if(PkgConfig_FOUND)
    # gio-unix-2.0 supplies gunixfdlist.h, which is needed for portal file
    # descriptors and is a separate pkg-config module on Ubuntu 22.04.
    pkg_check_modules(DCU_GIO QUIET IMPORTED_TARGET gio-2.0 gio-unix-2.0 glib-2.0 gobject-2.0)
    pkg_check_modules(DCU_X11 QUIET IMPORTED_TARGET x11)
    pkg_check_modules(DCU_XTEST QUIET IMPORTED_TARGET xtst)
    pkg_check_modules(DCU_XRANDR QUIET IMPORTED_TARGET xrandr)
    pkg_check_modules(DCU_JPEG QUIET IMPORTED_TARGET libjpeg)
    pkg_check_modules(DCU_PNG QUIET IMPORTED_TARGET libpng)
    pkg_check_modules(DCU_PIPEWIRE QUIET IMPORTED_TARGET libpipewire-0.3)
    # AT-SPI's pkg-config name differs between Ubuntu releases.  The
    # implementation also works without it and reports accessibility as
    # unavailable from doctor/capabilities.
    pkg_check_modules(DCU_ATSPI QUIET IMPORTED_TARGET atspi-2)
    if(NOT DCU_ATSPI_FOUND)
        pkg_check_modules(DCU_ATSPI QUIET IMPORTED_TARGET atspi-2.0)
    endif()
endif()

if(DCU_GIO_FOUND)
    target_link_libraries(dcu_platform PRIVATE PkgConfig::DCU_GIO)
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_GIO=1)
else()
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_GIO=0)
endif()

if(DCU_X11_FOUND)
    target_link_libraries(dcu_platform PRIVATE PkgConfig::DCU_X11)
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_X11=1)
else()
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_X11=0)
endif()

if(DCU_XTEST_FOUND)
    target_link_libraries(dcu_platform PRIVATE PkgConfig::DCU_XTEST)
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_XTEST=1)
else()
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_XTEST=0)
endif()

if(DCU_XRANDR_FOUND)
    target_link_libraries(dcu_platform PRIVATE PkgConfig::DCU_XRANDR)
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_XRANDR=1)
else()
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_XRANDR=0)
endif()

if(DCU_JPEG_FOUND)
    target_link_libraries(dcu_platform PRIVATE PkgConfig::DCU_JPEG)
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_JPEG=1)
else()
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_JPEG=0)
endif()

if(DCU_PNG_FOUND)
    target_link_libraries(dcu_platform PRIVATE PkgConfig::DCU_PNG)
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_PNG=1)
else()
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_PNG=0)
endif()

if(DCU_PIPEWIRE_FOUND)
    target_link_libraries(dcu_platform PRIVATE PkgConfig::DCU_PIPEWIRE)
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_PIPEWIRE=1)
else()
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_PIPEWIRE=0)
endif()

if(DCU_ATSPI_FOUND)
    target_link_libraries(dcu_platform PRIVATE PkgConfig::DCU_ATSPI)
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_ATSPI=1)
else()
    target_compile_definitions(dcu_platform PRIVATE DCU_HAVE_ATSPI=0)
endif()

if(NOT DCU_HEADLESS_TEST)
    set(_dcu_missing)
    if(NOT DCU_GIO_FOUND)
        list(APPEND _dcu_missing DCU_GIO_FOUND)
    endif()
    if(NOT DCU_X11_FOUND)
        list(APPEND _dcu_missing DCU_X11_FOUND)
    endif()
    if(NOT DCU_XTEST_FOUND)
        list(APPEND _dcu_missing DCU_XTEST_FOUND)
    endif()
    if(NOT DCU_JPEG_FOUND)
        list(APPEND _dcu_missing DCU_JPEG_FOUND)
    endif()
    if(NOT DCU_PNG_FOUND)
        list(APPEND _dcu_missing DCU_PNG_FOUND)
    endif()
    if(NOT DCU_PIPEWIRE_FOUND)
        list(APPEND _dcu_missing DCU_PIPEWIRE_FOUND)
    endif()
    if(NOT DCU_ATSPI_FOUND)
        list(APPEND _dcu_missing DCU_ATSPI_FOUND)
    endif()
    if(_dcu_missing)
        message(FATAL_ERROR
            "Linux desktop backend dependencies are missing: ${_dcu_missing}. "
            "Install the documented development packages or configure -DDCU_HEADLESS_TEST=ON for diagnostics only.")
    endif()
endif()

if(UNIX AND NOT APPLE)
    target_link_libraries(dcu_platform PRIVATE dl pthread)
endif()

set_target_properties(dcu_platform PROPERTIES POSITION_INDEPENDENT_CODE ON)
