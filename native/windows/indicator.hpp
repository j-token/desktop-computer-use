#pragma once

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
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

    // Two physical Escape presses within this window stop the session.
    static constexpr std::uint32_t kDoubleEscapeWindowMs = 1000;

    // The callback is invoked on the indicator message thread for the second
    // physical Escape press within kDoubleEscapeWindowMs. It must be short and
    // non-blocking.
    bool start(std::function<void()> onStop, std::string& error);
    void stop() noexcept;

    // Used from the low-level keyboard hook. PostMessage is deliberately used
    // instead of joining the message thread, because the hook callback is
    // delivered on that thread. eventTimeMs is the hook event time
    // (KBDLLHOOKSTRUCT::time), so queueing delay does not affect the window.
    void post_escape_message(std::uint32_t eventTimeMs) noexcept;
    void request_stop_async() noexcept;
    void invoke_stop_callback() noexcept;
    void handle_escape(std::uint32_t eventTimeMs) noexcept;
    void repaint() noexcept;

    // Replaces the banner text until the duration elapses. Thread-safe.
    void show_transient_text(std::wstring text, std::chrono::milliseconds duration);
    std::wstring banner_text();

    // Enabled while any modifier toggle is on: physical Escape still counts
    // toward the stop but is not delivered to applications.
    static void set_swallow_user_escape(bool enabled) noexcept;

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
    std::wstring transientText_; // Guarded by stateMutex_.
    std::chrono::steady_clock::time_point transientUntil_{}; // Guarded by stateMutex_.
    // Indicator-thread only: the pending first Escape of a double press.
    bool escapeArmed_ = false;
    std::uint32_t firstEscapeTimeMs_ = 0;
    bool threadExited_ = false;
    bool startOk_ = false;
    std::string startError_;
    std::atomic_bool running_{false};
    std::atomic_bool ready_{false};
};

} // namespace dcu::windows
