// Include the implementation to exercise the private hook without synthesizing
// hardware input. This verifies routing only, not physical keyboard delivery.
#include "../../native/windows/indicator.cpp"
#include <chrono>
#include <iostream>

namespace {

void send_escape(DWORD message, DWORD flags, DWORD time) {
    KBDLLHOOKSTRUCT escape{};
    escape.vkCode = VK_ESCAPE;
    escape.flags = flags;
    escape.time = time;
    dcu::windows::low_level_keyboard_proc(
        HC_ACTION, message, reinterpret_cast<LPARAM>(&escape));
}

void press_escape(DWORD time) {
    send_escape(WM_KEYDOWN, 0, time);
    send_escape(WM_KEYUP, 0, time + 30);
}

void settle() { std::this_thread::sleep_for(std::chrono::milliseconds(50)); }

} // namespace

int main() {
    dcu::windows::SessionIndicator indicator;
    std::atomic<int> stopCallbacks{0};
    std::string error;
    if (!indicator.start([&] { ++stopCallbacks; }, error)) {
        std::cerr << error << '\n';
        return 1;
    }

    // Injected Escape presses never count, even twice in a row.
    send_escape(WM_KEYDOWN, LLKHF_INJECTED, 1000);
    send_escape(WM_KEYUP, LLKHF_INJECTED, 1010);
    send_escape(WM_KEYDOWN, LLKHF_INJECTED, 1020);
    settle();
    if (stopCallbacks != 0 || !indicator.running()) return 2;

    // A single physical Escape only arms the stop and shows the hint.
    press_escape(2000);
    settle();
    if (stopCallbacks != 0 || !indicator.running()) return 3;
    if (indicator.banner_text().find(L"again") == std::wstring::npos) return 4;

    // Auto-repeat while held is latched and is not a second press.
    send_escape(WM_KEYDOWN, 0, 4000);
    send_escape(WM_KEYDOWN, 0, 4100);
    send_escape(WM_KEYUP, 0, 4200);
    settle();
    if (stopCallbacks != 0 || !indicator.running()) return 5;

    // A second press after the 1000 ms window re-arms instead of stopping.
    press_escape(5500);
    settle();
    if (stopCallbacks != 0 || !indicator.running()) return 6;

    // A second physical press within the window stops exactly once.
    press_escape(6200);
    for (int attempt = 0; attempt < 100 && indicator.running(); ++attempt) {
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    indicator.stop();
    if (stopCallbacks != 1 || indicator.running()) return 7;
    std::cout << "PASS injected Escape ignored; single and expired presses only arm; "
                 "auto-repeat latched; second press within 1000 ms routed once; indicator stopped. "
                 "Physical keyboard delivery not tested.\n";
}
