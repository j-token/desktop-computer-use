#pragma once

#include "dcu/backend.hpp"

#include <cstdint>
#include <string>

#ifdef _WIN32
#include <windows.h>
#endif

namespace dcu::windows {

// One encoded image of a captured frame. scaleX/scaleY map window-local
// coordinates to this image's pixels: pixel = window * scale.
struct CapturedImage {
    std::string path;
    int width = 0;
    int height = 0;
    double scaleX = 1.0;
    double scaleY = 1.0;
};

// width/height/scale/scaleX/scaleY and path describe the reduced image.
struct CaptureResult {
    CapturedImage full;
    CapturedImage reduced;
    std::string path;
    std::string mimeType;
    int width = 0;
    int height = 0;
    double scale = 1.0;
    std::string backend;
    std::int64_t frameTimestamp100ns = 0;
    std::int64_t capturedAtUnixMs = 0;
    bool freshFrame = false;
    bool cachedFrame = false;
    int sourceWidth = 0;
    int sourceHeight = 0;
    double scaleX = 1.0;
    double scaleY = 1.0;
};

// Persistent Windows.Graphics.Capture + D3D11 frame-pool capture. The object
// is reused for successive observations of the same HWND and falls back to a
// visible desktop BitBlt only when WGC cannot be created (for example on an
// unsupported pre-1903 host).
class WindowCapture final {
public:
    WindowCapture();
    ~WindowCapture();

    WindowCapture(const WindowCapture&) = delete;
    WindowCapture& operator=(const WindowCapture&) = delete;

    // Encodes one frame twice: the full-resolution original to fullPath and a
    // reduced copy (0.5x above 1280x720, otherwise 1x, then capped by a
    // positive maxEdge) to reducedPath.
    CaptureResult capture(void* hwnd, const RECT& windowRect,
                          const std::string& fullPath, const std::string& reducedPath,
                          const std::string& format, int quality, int maxEdge,
                          Context& context);

    // Stops the persistent frame pool and releases its capture resources.
    // Called on session stop so a stopped session does not keep receiving
    // frames or retaining a target HWND.
    void reset() noexcept;
    // Following observations must use a frame acquired after the last input.
    void mark_input_complete() noexcept;

    bool wgc_available() const noexcept;

private:
    struct Impl;
    Impl* impl_ = nullptr;
};

} // namespace dcu::windows
