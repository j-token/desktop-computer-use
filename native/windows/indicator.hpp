#pragma once

#include <atomic>
#include <condition_variable>
#include <functional>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

namespace dcu::windows {

// A non-activating, click-through topmost overlay used to make an active
// computer-use session visible to the person at the desktop.  The overlay is
// kept on its own message thread so the low-level Escape hook and repainting
// never block the daemon's serialized operation thread.
class SessionIndicator final {
public:
    SessionIndicator() = default;
    ~SessionIndicator();

    SessionIndicator(const SessionIndicator&) = delete;
    SessionIndicator& operator=(const SessionIndicator&) = delete;

    // The callback is invoked on the indicator message thread for a physical
    // Escape press. It must be short and non-blocking.
    bool start(std::function<void()> onStop, std::string& error);
    void stop() noexcept;

    // Used from the low-level keyboard hook. PostMessage is deliberately used
    // instead of joining the message thread, because the hook callback is
    // delivered on that thread.
    void post_escape_message() noexcept;
    void request_stop_async() noexcept;
    void invoke_stop_callback() noexcept;
    void handle_escape() noexcept;
    void repaint() noexcept;

    bool running() const noexcept { return running_.load(std::memory_order_acquire); }
    bool ready() const noexcept { return ready_.load(std::memory_order_acquire); }

private:
    void thread_main(std::function<void()> onStop);
    bool rebuild_edges() noexcept;

    std::thread thread_;
    std::mutex stateMutex_;
    std::condition_variable stateCv_;
    std::function<void()> onStop_;
    void* window_ = nullptr; // HWND, kept opaque in the header.
    void* cursorWindow_ = nullptr; // HWND, kept opaque in the header.
    void* keyboardHook_ = nullptr; // HHOOK, kept opaque in the header.
    std::vector<void*> edgeWindows_; // HWNDs, four click-through strips per monitor.
    bool threadExited_ = false;
    bool startOk_ = false;
    std::string startError_;
    std::atomic_bool running_{false};
    std::atomic_bool ready_{false};
};

} // namespace dcu::windows
