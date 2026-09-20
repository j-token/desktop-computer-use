// Include the implementation to exercise the private hook without synthesizing
// hardware input. This verifies routing only, not physical keyboard delivery.
#include "../../native/windows/indicator.cpp"
#include <chrono>
#include <iostream>

int main() {
    dcu::windows::SessionIndicator indicator;
    std::atomic<int> stopCallbacks{0};
    std::string error;
    if (!indicator.start([&] { ++stopCallbacks; }, error)) {
        std::cerr << error << '\n';
        return 1;
    }

    KBDLLHOOKSTRUCT escape{};
    escape.vkCode = VK_ESCAPE;
    escape.flags = LLKHF_INJECTED;
    dcu::windows::low_level_keyboard_proc(
        HC_ACTION, WM_KEYDOWN, reinterpret_cast<LPARAM>(&escape));
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    if (stopCallbacks != 0 || !indicator.running()) return 2;

    escape.flags = 0;
    dcu::windows::low_level_keyboard_proc(
        HC_ACTION, WM_KEYDOWN, reinterpret_cast<LPARAM>(&escape));
    for (int attempt = 0; attempt < 100 && indicator.running(); ++attempt) {
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    indicator.stop();
    if (stopCallbacks != 1 || indicator.running()) return 3;
    std::cout << "PASS injected Escape ignored; non-injected callback routed once; indicator stopped. Physical keyboard delivery not tested.\n";
}
