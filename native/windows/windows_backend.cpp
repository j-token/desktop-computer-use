#include "capture.hpp"
#include "indicator.hpp"
#include "uia.hpp"
#include "watchdog.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#include <dwmapi.h>
#include <shellapi.h>
#include <shlobj.h>
#include <uiautomationcore.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <cctype>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <filesystem>
#include <map>
#include <mutex>
#include <optional>
#include <unordered_map>
#include <vector>

namespace dcu::windows {
namespace {

using Clock = std::chrono::steady_clock;

class ComScope final {
public:
    ComScope() : result_(CoInitializeEx(nullptr, COINIT_MULTITHREADED)) {}
    ~ComScope() {
        if (SUCCEEDED(result_)) CoUninitialize();
    }
    bool ok() const { return SUCCEEDED(result_) || result_ == RPC_E_CHANGED_MODE; }

private:
    HRESULT result_;
};

std::string utf8(const std::wstring& value) {
    if (value.empty()) return {};
    const int count = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0,
                                          nullptr, nullptr);
    if (count <= 0) return {};
    std::string result(static_cast<std::size_t>(count), '\0');
    WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                        static_cast<int>(value.size()), result.data(), count, nullptr,
                        nullptr);
    return result;
}

std::wstring wide(const std::string& value) {
    if (value.empty()) return {};
    const int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0);
    if (count <= 0) throw Error("invalid_text", "Input is not valid UTF-8");
    std::wstring result(static_cast<std::size_t>(count), L'\0');
    MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                        static_cast<int>(value.size()), result.data(), count);
    return result;
}

std::string lower(std::string value) {
    std::transform(value.begin(), value.end(), value.begin(),
                   [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
    return value;
}

std::string json_string(const Json& params, const char* key, std::string fallback = {}) {
    const auto it = params.find(key);
    return it != params.end() && it->is_string() ? it->get<std::string>() : fallback;
}

int json_int(const Json& params, const char* key, int fallback) {
    const auto it = params.find(key);
    return it != params.end() && it->is_number_integer() ? it->get<int>() : fallback;
}

bool json_bool(const Json& params, const char* key, bool fallback) {
    const auto it = params.find(key);
    return it != params.end() && it->is_boolean() ? it->get<bool>() : fallback;
}

std::string guid_string() {
    GUID guid{};
    if (FAILED(CoCreateGuid(&guid))) {
        return "observation-" + std::to_string(GetTickCount64());
    }
    wchar_t value[40]{};
    StringFromGUID2(guid, value, static_cast<int>(std::size(value)));
    return utf8(value);
}

std::string iso8601_now() {
    SYSTEMTIME time{};
    GetSystemTime(&time);
    char buffer[64]{};
    std::snprintf(buffer, sizeof(buffer), "%04u-%02u-%02uT%02u:%02u:%02u.%03uZ",
                  time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute,
                  time.wSecond, time.wMilliseconds);
    return buffer;
}

std::string iso8601_from_unix_ms(std::int64_t unixMilliseconds) {
    if (unixMilliseconds <= 0) return iso8601_now();
    const std::time_t seconds = static_cast<std::time_t>(unixMilliseconds / 1000);
    std::tm utc{};
    if (gmtime_s(&utc, &seconds) != 0) return iso8601_now();
    const auto milliseconds = static_cast<unsigned>(unixMilliseconds % 1000);
    char buffer[64]{};
    std::snprintf(buffer, sizeof(buffer), "%04d-%02d-%02dT%02d:%02d:%02d.%03uZ",
                  utc.tm_year + 1900, utc.tm_mon + 1, utc.tm_mday, utc.tm_hour,
                  utc.tm_min, utc.tm_sec, milliseconds);
    return buffer;
}

std::string process_name(DWORD pid) {
    HANDLE handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
    if (!handle) return {};
    wchar_t path[MAX_PATH * 4]{};
    DWORD length = static_cast<DWORD>(std::size(path));
    const bool ok = QueryFullProcessImageNameW(handle, 0, path, &length) != FALSE;
    CloseHandle(handle);
    if (!ok) return {};
    std::wstring value(path, length);
    const auto slash = value.find_last_of(L"\\/");
    if (slash != std::wstring::npos) value.erase(0, slash + 1);
    return utf8(value);
}

bool is_session_indicator_window(HWND hwnd) {
    wchar_t className[128]{};
    const int length = GetClassNameW(hwnd, className, static_cast<int>(std::size(className)));
    return length > 0 &&
           std::wstring(className, static_cast<std::size_t>(length)) ==
               L"DesktopComputerUse.SessionIndicator.1";
}

struct WindowInfo {
    HWND hwnd = nullptr;
    DWORD pid = 0;
    std::string app;
    std::string title;
    RECT rect{};
    bool minimized = false;
};

std::optional<WindowInfo> inspect_window(HWND hwnd) {
    if (!IsWindow(hwnd) || !IsWindowVisible(hwnd) || GetWindow(hwnd, GW_OWNER) ||
        is_session_indicator_window(hwnd)) {
        return std::nullopt;
    }
    DWORD pid = 0;
    GetWindowThreadProcessId(hwnd, &pid);
    if (!pid) return std::nullopt;
    RECT rect{};
    if (!GetWindowRect(hwnd, &rect) || rect.right <= rect.left || rect.bottom <= rect.top) {
        return std::nullopt;
    }
    // GetWindowRect includes the invisible resize border on modern DWM
    // windows.  WGC and the pixels a user sees use the extended frame, so a
    // single canonical rectangle keeps screenshots, UIA bounds, and native
    // input in the same coordinate space.  Older/non-DWM windows fall back to
    // the Win32 rectangle above.
    RECT frame{};
    if (SUCCEEDED(DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS,
                                        &frame, sizeof(frame))) &&
        frame.right > frame.left && frame.bottom > frame.top) {
        rect = frame;
    }
    const int titleLength = GetWindowTextLengthW(hwnd);
    std::wstring title(static_cast<std::size_t>(std::max(0, titleLength)) + 1, L'\0');
    if (titleLength > 0) {
        const int copied = GetWindowTextW(hwnd, title.data(), titleLength + 1);
        title.resize(static_cast<std::size_t>(std::max(0, copied)));
    } else {
        title.clear();
    }
    WindowInfo info;
    info.hwnd = hwnd;
    info.pid = pid;
    info.app = process_name(pid);
    info.title = utf8(title);
    info.rect = rect;
    info.minimized = IsIconic(hwnd) != FALSE;
    return info;
}

BOOL CALLBACK enum_windows_proc(HWND hwnd, LPARAM raw) {
    auto* windows = reinterpret_cast<std::vector<WindowInfo>*>(raw);
    if (const auto info = inspect_window(hwnd)) windows->push_back(*info);
    return TRUE;
}

std::vector<WindowInfo> enumerate_windows() {
    std::vector<WindowInfo> windows;
    EnumWindows(enum_windows_proc, reinterpret_cast<LPARAM>(&windows));
    return windows;
}

bool app_name_matches(const WindowInfo& window, const std::string& requested) {
    const auto name = lower(window.app);
    if (name == requested) return true;
    return name.size() > 4 && name.ends_with(".exe") &&
           name.substr(0, name.size() - 4) == requested;
}

std::string window_id(HWND hwnd) {
    return "hwnd:" + std::to_string(reinterpret_cast<std::uintptr_t>(hwnd));
}

HWND parse_window_id(const std::string& id) {
    if (id.rfind("hwnd:", 0) != 0) return nullptr;
    try {
        const auto value = std::stoull(id.substr(5));
        return reinterpret_cast<HWND>(static_cast<std::uintptr_t>(value));
    } catch (...) {
        return nullptr;
    }
}

bool foreground_matches(HWND hwnd) {
    const HWND foreground = GetForegroundWindow();
    return foreground == hwnd || (foreground && GetAncestor(foreground, GA_ROOT) == hwnd);
}

Json window_json(const WindowInfo& info) {
    return Json{{"id", window_id(info.hwnd)},
                {"app", info.app},
                {"pid", info.pid},
                {"title", info.title},
                {"x", info.rect.left},
                {"y", info.rect.top},
                {"width", std::max<LONG>(0, info.rect.right - info.rect.left)},
                {"height", std::max<LONG>(0, info.rect.bottom - info.rect.top)},
                {"isMinimized", info.minimized},
                {"isForeground", foreground_matches(info.hwnd)},
                {"isOffscreen", false}};
}

std::string private_capture_path(const std::string& observationId,
                                 const std::string& format, const char* suffix = "") {
    PWSTR known = nullptr;
    std::filesystem::path directory;
    if (SUCCEEDED(SHGetKnownFolderPath(FOLDERID_LocalAppData, KF_FLAG_DEFAULT, nullptr,
                                       &known)) && known) {
        directory = std::filesystem::path(known) / L"DesktopComputerUse" / L"captures";
        CoTaskMemFree(known);
    } else {
        wchar_t temporary[MAX_PATH]{};
        const DWORD length = GetTempPathW(static_cast<DWORD>(std::size(temporary)), temporary);
        if (!length) throw Error("capture_path", "Cannot determine a private capture directory");
        directory = std::filesystem::path(temporary) / L"DesktopComputerUse" / L"captures";
    }
    std::error_code error;
    std::filesystem::create_directories(directory, error);
    if (error) throw Error("capture_path", "Cannot create the private capture directory");
    const std::string filename = observationId + suffix + (format == "png" ? ".png" : ".jpg");
    return utf8((directory / std::filesystem::path(wide(filename))).wstring());
}

void remove_capture_file(const std::string& path) noexcept {
    if (path.empty()) return;
    try {
        std::error_code ignored;
        std::filesystem::remove(std::filesystem::path(wide(path)), ignored);
    } catch (...) {
    }
}

Json action_transform(double scaleX, double scaleY) {
    return Json{{"scaleX", scaleX}, {"scaleY", scaleY}, {"offsetX", 0}, {"offsetY", 0}};
}

DWORD modifier_mask(const std::vector<WORD>& keys) {
    DWORD value = 0;
    for (const auto key : keys) {
        if (key == VK_SHIFT) value |= 1;
        else if (key == VK_CONTROL) value |= 2;
        else if (key == VK_MENU) value |= 4;
        else if (key == VK_LWIN) value |= 8;
    }
    return value;
}

bool same_rect(const RECT& a, const RECT& b) {
    return a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom;
}

// A request is served on a detached worker thread.  Windows allows a
// foreground change from that thread only when it shares the input queue with
// the current foreground or target GUI thread.  Attachments are temporary:
// keeping one after activation would merge unrelated keyboard state and would
// make cancellation and thread teardown unsafe.
class ScopedThreadInputAttachments final {
public:
    ScopedThreadInputAttachments() = default;
    ~ScopedThreadInputAttachments() noexcept {
        for (std::size_t index = attachmentCount_; index > 0; --index) {
            const auto& attachment = attachments_[index - 1];
            AttachThreadInput(attachment.first, attachment.second, FALSE);
        }
    }

    ScopedThreadInputAttachments(const ScopedThreadInputAttachments&) = delete;
    ScopedThreadInputAttachments& operator=(const ScopedThreadInputAttachments&) = delete;

    bool attach(DWORD firstThread, DWORD secondThread) noexcept {
        if (!firstThread || !secondThread || firstThread == secondThread ||
            attachmentCount_ == attachments_.size()) {
            return false;
        }
        for (std::size_t index = 0; index < attachmentCount_; ++index) {
            const auto& attachment = attachments_[index];
            if ((attachment.first == firstThread && attachment.second == secondThread) ||
                (attachment.first == secondThread && attachment.second == firstThread)) {
                return true;
            }
        }
        if (!AttachThreadInput(firstThread, secondThread, TRUE)) return false;
        attachments_[attachmentCount_++] = {firstThread, secondThread};
        return true;
    }

private:
    struct Attachment {
        DWORD first = 0;
        DWORD second = 0;
    };
    std::array<Attachment, 2> attachments_{};
    std::size_t attachmentCount_ = 0;
};

void ensure_message_queue() noexcept {
    MSG message{};
    // AttachThreadInput requires both threads to have message queues.  A
    // detached pipe worker may not have created one yet; PM_NOREMOVE leaves
    // the queue and all messages untouched.
    PeekMessageW(&message, nullptr, 0, 0, PM_NOREMOVE);
}

enum class MouseButton { left, right, middle };

MouseButton mouse_button(const std::string& name) {
    const auto value = lower(name.empty() ? "left" : name);
    if (value == "left") return MouseButton::left;
    if (value == "right") return MouseButton::right;
    if (value == "middle") return MouseButton::middle;
    throw Error("invalid_button", "button must be left, right, or middle");
}

DWORD button_down_flag(MouseButton button) {
    switch (button) {
    case MouseButton::right: return MOUSEEVENTF_RIGHTDOWN;
    case MouseButton::middle: return MOUSEEVENTF_MIDDLEDOWN;
    default: return MOUSEEVENTF_LEFTDOWN;
    }
}

DWORD button_up_flag(MouseButton button) {
    switch (button) {
    case MouseButton::right: return MOUSEEVENTF_RIGHTUP;
    case MouseButton::middle: return MOUSEEVENTF_MIDDLEUP;
    default: return MOUSEEVENTF_LEFTUP;
    }
}

DWORD button_bit(MouseButton button) {
    switch (button) {
    case MouseButton::right: return 2;
    case MouseButton::middle: return 4;
    default: return 1;
    }
}

WORD modifier_vk(const std::string& value) {
    const auto key = lower(value);
    if (key == "shift") return VK_SHIFT;
    if (key == "ctrl" || key == "control") return VK_CONTROL;
    if (key == "alt" || key == "option") return VK_MENU;
    if (key == "win" || key == "meta" || key == "super") return VK_LWIN;
    return 0;
}

WORD key_vk(const std::string& value) {
    const auto key = lower(value);
    static const std::map<std::string, WORD> names{
        {"backspace", VK_BACK}, {"tab", VK_TAB},       {"enter", VK_RETURN},
        {"return", VK_RETURN}, {"escape", VK_ESCAPE}, {"esc", VK_ESCAPE},
        {"space", VK_SPACE},   {"left", VK_LEFT},     {"right", VK_RIGHT},
        {"up", VK_UP},         {"down", VK_DOWN},     {"home", VK_HOME},
        {"end", VK_END},       {"pageup", VK_PRIOR},  {"pagedown", VK_NEXT},
        {"delete", VK_DELETE}, {"insert", VK_INSERT}, {"pause", VK_PAUSE},
        {"capslock", VK_CAPITAL}, {"numlock", VK_NUMLOCK}, {"printscreen", VK_SNAPSHOT},
    };
    if (const auto found = names.find(key); found != names.end()) return found->second;
    if (key.size() == 2 && key[0] == 'f' && key[1] >= '1' && key[1] <= '9') {
        return static_cast<WORD>(VK_F1 + key[1] - '1');
    }
    if (key.size() == 3 && key[0] == 'f' && key[1] >= '1' && key[1] <= '2' &&
        key[2] >= '0' && key[2] <= '4') {
        const int number = std::stoi(key.substr(1));
        if (number >= 10 && number <= 24) return static_cast<WORD>(VK_F1 + number - 1);
    }
    if (key.size() == 1) {
        const wchar_t character = static_cast<wchar_t>(key[0]);
        const SHORT translated = VkKeyScanW(character);
        if (translated != -1) return LOBYTE(translated);
    }
    throw Error("invalid_key", "Unknown key: " + value);
}

bool extended_key(WORD key) {
    return key == VK_LEFT || key == VK_RIGHT || key == VK_UP || key == VK_DOWN ||
           key == VK_HOME || key == VK_END || key == VK_PRIOR || key == VK_NEXT ||
           key == VK_INSERT || key == VK_DELETE || key == VK_DIVIDE || key == VK_NUMLOCK;
}

void send_input(INPUT& input, const char* errorCode = "input_failed") {
    if (SendInput(1, &input, sizeof(INPUT)) != 1) {
        throw Error(errorCode, "Windows SendInput failed: " + std::to_string(GetLastError()));
    }
}

void send_key(WORD key, bool down) {
    INPUT input{};
    input.type = INPUT_KEYBOARD;
    input.ki.wVk = key;
    input.ki.dwFlags = down ? (extended_key(key) ? KEYEVENTF_EXTENDEDKEY : 0)
                            : KEYEVENTF_KEYUP | (extended_key(key) ? KEYEVENTF_EXTENDEDKEY : 0);
    send_input(input);
}

void send_mouse_button(MouseButton button, bool down) {
    INPUT input{};
    input.type = INPUT_MOUSE;
    input.mi.dwFlags = down ? button_down_flag(button) : button_up_flag(button);
    send_input(input);
}

void send_mouse_move(int x, int y) {
    if (!SetCursorPos(x, y)) {
        throw Error("input_failed", "SetCursorPos failed: " + std::to_string(GetLastError()));
    }
}

void sleep_context(Context& context, int milliseconds) {
    const auto deadline = Clock::now() + std::chrono::milliseconds(std::max(0, milliseconds));
    while (Clock::now() < deadline) {
        context.check();
        const auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(deadline - Clock::now());
        Sleep(static_cast<DWORD>(std::clamp<long long>(remaining.count(), 1, 10)));
    }
    context.check();
}

class ModifierGuard final {
public:
    explicit ModifierGuard(const std::vector<WORD>& modifiers) : keys_(modifiers) {
        try {
            for (const auto key : keys_) send_key(key, true);
        } catch (...) {
            for (auto it = keys_.rbegin(); it != keys_.rend(); ++it) {
                INPUT input{};
                input.type = INPUT_KEYBOARD;
                input.ki.wVk = *it;
                input.ki.dwFlags = KEYEVENTF_KEYUP | (extended_key(*it) ? KEYEVENTF_EXTENDEDKEY : 0);
                SendInput(1, &input, sizeof(INPUT));
            }
            throw;
        }
    }
    ~ModifierGuard() {
        for (auto it = keys_.rbegin(); it != keys_.rend(); ++it) {
            INPUT input{};
            input.type = INPUT_KEYBOARD;
            input.ki.wVk = *it;
            input.ki.dwFlags = KEYEVENTF_KEYUP | (extended_key(*it) ? KEYEVENTF_EXTENDEDKEY : 0);
            SendInput(1, &input, sizeof(INPUT));
        }
    }

private:
    std::vector<WORD> keys_;
};

} // namespace

class WindowsBackend final : public Backend {
public:
    WindowsBackend() = default;
    ~WindowsBackend() override {
        interrupt();
        indicator_.stop();
        release_held_input();
    }

    Json execute(const std::string& method, const Json& params, Context& context) override;
    void interrupt() noexcept override;

private:
    struct Observation {
        std::string id;
        HWND hwnd = nullptr;
        RECT rect{};
        Clock::time_point created;
        Json window;
        std::vector<ElementRecord> elements;
        // Present when the observation captured a screenshot. Each image's
        // scale maps window-local coordinates to its pixels.
        bool hasScreenshot = false;
        std::string mimeType;
        CapturedImage reduced;
        CapturedImage full;
    };

    // Window-local point resolved from screenshot pixel coordinates.
    struct CoordinateTransform {
        std::string space;
        double scaleX = 1.0;
        double scaleY = 1.0;
        int imageWidth = 0;
        int imageHeight = 0;
    };

    struct ContextScope {
        WindowsBackend& owner;
        Context* context;
        ContextScope(WindowsBackend& backend, Context& value) : owner(backend), context(&value) {
            owner.currentContext_.store(context, std::memory_order_release);
            owner.interrupted_.store(false, std::memory_order_release);
        }
        ~ContextScope() { owner.currentContext_.store(nullptr, std::memory_order_release); }
    };

    void ensure_session(const Json& params) const;
    std::optional<WindowInfo> target_window(const Json& params) const;
    WindowInfo require_target_window(const Json& params) const;
    Observation& require_observation(const Json& params, HWND hwnd, const RECT& rect);
    ElementRecord& require_element(const Json& params, Observation& observation, const char* key);
    void purge_observations();
    void clear_observations() noexcept;
    CoordinateTransform coordinate_transform(const Json& params, const WindowInfo& target,
                                             const Observation* observation);
    bool activate(HWND hwnd, Context& context) const;
    Json observe(const Json& params, Context& context);
    Json full_screenshot(const Json& params);
    Json action_result();
    std::vector<WORD> modifiers(const Json& params) const;
    void click_at(HWND hwnd, POINT point, MouseButton button, const Json& params,
                  Context& context);
    void drag_at(HWND hwnd, POINT from, POINT to, MouseButton button,
                 int duration, int steps, int holdBefore, int holdAfter,
                 Context& context);
    void send_chord(const std::string& chord, Context& context);
    void send_text(const std::string& text, Context& context);
    Json list_apps() const;
    Json list_windows(const Json& params) const;
    Json doctor() const;
    Json capabilities() const;
    Json handle_action(const std::string& method, const Json& params, Context& context);
    void stop_session() noexcept;
    void on_stop_key() noexcept;
    void release_held_input() noexcept;
    void publish_input_state() noexcept;

    mutable std::mutex observationsMutex_;
    std::unordered_map<std::string, Observation> observations_;
    SessionIndicator indicator_;
    WindowCapture capture_;
    UiaSnapshotBuilder uia_;
    std::atomic<Context*> currentContext_{nullptr};
    std::atomic<Context*> sessionContext_{nullptr};
    std::atomic_bool interrupted_{false};
    std::atomic_bool sessionActive_{false};
    std::string sessionId_;
    std::atomic<DWORD> heldButtons_{0};
    std::atomic<DWORD> heldModifiers_{0};
    std::atomic<WORD> heldKey_{0};
    std::atomic<DWORD> heldKeyFlags_{0};
    InputWatchdog watchdog_;
};

Json WindowsBackend::execute(const std::string& method, const Json& params,
                             Context& context) {
    ContextScope scope(*this, context);
    ComScope com;
    if (!com.ok()) throw Error("com_unavailable", "COM initialization failed");

    if (method == "doctor") return doctor();
    if (method == "capabilities") return capabilities();
    if (method == "session.status") {
        return Json{{"active", sessionActive_.load()}, {"sessionId", sessionId_},
                    {"indicatorReady", indicator_.ready()},
                    {"hotkey", "Escape x2"}, {"stopKey", "Escape x2"}};
    }
    if (method == "session.start") {
        if (sessionActive_.load()) throw Error("session_active", "A computer-use session is already active");
        const std::string requested = json_string(params, "sessionId", guid_string());
        std::string indicatorError;
        if (!indicator_.start([this] { on_stop_key(); }, indicatorError)) {
            throw Error("setup_required", indicatorError);
        }
        sessionId_ = requested;
        sessionContext_.store(&context, std::memory_order_release);
        sessionActive_.store(true, std::memory_order_release);
        interrupted_.store(false, std::memory_order_release);
        return Json{{"ready", true}, {"sessionId", sessionId_},
                    {"indicator", { {"ready", true}, {"hotkey", "Escape x2"},
                                     {"stopKey", "Escape x2"},
                                     {"clickThrough", true}, {"cursorHighlight", true},
                                     {"edgeBorder", true} }}};
    }
    if (method == "session.stop") {
        stop_session();
        return Json{{"stopped", true}};
    }
    if (method == "daemon.shutdown") {
        stop_session();
        return Json{{"stopped", true}};
    }
    ensure_session(params);
    if (method == "list-apps") return list_apps();
    if (method == "list-windows") return list_windows(params);
    if (method == "get-app-state") return observe(params, context);
    if (method == "get-full-screenshot") return full_screenshot(params);
    return handle_action(method, params, context);
}

void WindowsBackend::ensure_session(const Json& params) const {
    if (!sessionActive_.load(std::memory_order_acquire)) {
        throw Error("session_required", "Start a computer-use session first");
    }
    const auto supplied = json_string(params, "sessionId");
    if (!supplied.empty() && supplied != sessionId_) {
        throw Error("wrong_session", "The request does not own the active session");
    }
}

std::optional<WindowInfo> WindowsBackend::target_window(const Json& params) const {
    const auto windows = enumerate_windows();
    const auto id = json_string(params, "windowId");
    if (!id.empty()) {
        const HWND hwnd = parse_window_id(id);
        for (const auto& info : windows) {
            if (info.hwnd == hwnd) {
                const auto app = json_string(params, "app");
                if (!app.empty() && !app_name_matches(info, lower(app))) {
                    return std::nullopt;
                }
                return info;
            }
        }
        return std::nullopt;
    }
    const auto app = lower(json_string(params, "app"));
    if (!app.empty()) {
        for (const auto& info : windows) {
            if (app_name_matches(info, app)) return info;
        }
        if (app.rfind("pid:", 0) == 0) {
            try {
                const DWORD pid = static_cast<DWORD>(std::stoul(app.substr(4)));
                for (const auto& info : windows) if (info.pid == pid) return info;
            } catch (...) {
            }
        }
        return std::nullopt;
    }
    const HWND foreground = GetForegroundWindow();
    for (const auto& info : windows) if (info.hwnd == foreground) return info;
    return windows.empty() ? std::nullopt : std::optional<WindowInfo>(windows.front());
}

WindowInfo WindowsBackend::require_target_window(const Json& params) const {
    if (const auto info = target_window(params)) return *info;
    throw Error("window_not_found", "No matching visible top-level window was found");
}

void WindowsBackend::purge_observations() {
    const auto now = Clock::now();
    std::vector<std::string> removed;
    {
        std::lock_guard lock(observationsMutex_);
        const auto evict = [&](auto it) {
            removed.push_back(it->second.reduced.path);
            removed.push_back(it->second.full.path);
            return observations_.erase(it);
        };
        for (auto it = observations_.begin(); it != observations_.end();) {
            if (now - it->second.created > std::chrono::minutes(2)) it = evict(it);
            else ++it;
        }
        while (observations_.size() > 32) {
            const auto oldest = std::min_element(
                observations_.begin(), observations_.end(),
                [](const auto& a, const auto& b) { return a.second.created < b.second.created; });
            evict(oldest);
        }
    }
    for (const auto& path : removed) remove_capture_file(path);
}

void WindowsBackend::clear_observations() noexcept {
    std::vector<std::string> removed;
    {
        std::lock_guard lock(observationsMutex_);
        for (const auto& entry : observations_) {
            removed.push_back(entry.second.reduced.path);
            removed.push_back(entry.second.full.path);
        }
        observations_.clear();
    }
    for (const auto& path : removed) remove_capture_file(path);
}

WindowsBackend::CoordinateTransform WindowsBackend::coordinate_transform(
    const Json& params, const WindowInfo& target, const Observation* observation) {
    const bool full = json_string(params, "coords", "reduced") == "full";
    const Observation* source = observation;
    if (!source) purge_observations();
    std::lock_guard lock(observationsMutex_);
    if (!source) {
        // Without an observationId, pixel coordinates refer to the most recent
        // screenshot of this window.
        for (const auto& entry : observations_) {
            const auto& candidate = entry.second;
            if (candidate.hasScreenshot && candidate.hwnd == target.hwnd &&
                (!source || candidate.created > source->created)) {
                source = &candidate;
            }
        }
        if (!source) {
            throw Error("observation_required",
                        "Observe the window with a screenshot before clicking by coordinates");
        }
        const RECT& old = source->rect;
        if (old.right - old.left != target.rect.right - target.rect.left ||
            old.bottom - old.top != target.rect.bottom - target.rect.top) {
            throw Error("stale_observation",
                        "Window size changed since the last screenshot; observe again");
        }
    } else if (!source->hasScreenshot) {
        throw Error("observation_required",
                    "Observe the window with a screenshot before clicking by coordinates");
    }
    const auto& image = full ? source->full : source->reduced;
    return CoordinateTransform{full ? "full" : "reduced", image.scaleX, image.scaleY,
                               image.width, image.height};
}

WindowsBackend::Observation& WindowsBackend::require_observation(const Json& params, HWND hwnd,
                                                                   const RECT& rect) {
    const auto id = json_string(params, "observationId");
    if (id.empty()) throw Error("observation_required", "An observationId is required for indexed input");
    purge_observations();
    std::lock_guard lock(observationsMutex_);
    const auto found = observations_.find(id);
    if (found == observations_.end()) throw Error("stale_observation", "Observation is missing or expired");
    if (found->second.hwnd != hwnd || !same_rect(found->second.rect, rect) || !IsWindow(hwnd)) {
        throw Error("stale_observation", "Window geometry changed; observe again");
    }
    return found->second;
}

namespace {
// A degenerate bounding rectangle would centre on the window corner, so every
// coordinate derived from an element has to refuse it instead of clicking there.
void require_clickable_bounds(const ElementRecord& record) {
    if (record.bounds.right <= record.bounds.left || record.bounds.bottom <= record.bounds.top) {
        throw Error("element_not_actionable",
                    "Element has no clickable area; observe again or choose another element");
    }
}
} // namespace

ElementRecord& WindowsBackend::require_element(const Json& params, Observation& observation,
                                               const char* key) {
    const auto it = params.find(key);
    if (it == params.end() || !it->is_number_integer()) {
        throw Error("element_required", std::string(key) + " is required");
    }
    const int index = it->get<int>();
    if (index < 0 || index >= static_cast<int>(observation.elements.size())) {
        throw Error("element_not_found", "Element index is not present in the observation");
    }
    return observation.elements[static_cast<std::size_t>(index)];
}

bool WindowsBackend::activate(HWND hwnd, Context& context) const {
    if (!IsWindow(hwnd)) throw Error("window_not_found", "The target window no longer exists");
    if (IsIconic(hwnd)) ShowWindow(hwnd, SW_RESTORE);
    context.check();

    const auto deadline = Clock::now() + std::chrono::milliseconds(750);

    // Try the normal path first.  It avoids changing input queues when the
    // daemon already owns the foreground permission.
    SetForegroundWindow(hwnd);
    if (foreground_matches(hwnd)) return true;

    // The dispatcher handles requests on detached worker threads.  Establish
    // a temporary relationship with the foreground and target GUI threads so
    // SetForegroundWindow can pass the foreground-lock check.  The scope
    // detaches on every return path, including Context cancellation.
    ensure_message_queue();
    const DWORD requestThread = GetCurrentThreadId();
    const HWND foregroundWindow = GetForegroundWindow();
    const DWORD foregroundThread = foregroundWindow
                                       ? GetWindowThreadProcessId(foregroundWindow, nullptr)
                                       : 0;
    const DWORD targetThread = GetWindowThreadProcessId(hwnd, nullptr);
    ScopedThreadInputAttachments attachments;
    attachments.attach(requestThread, foregroundThread);
    attachments.attach(requestThread, targetThread);

    context.check();
    if (!IsWindow(hwnd)) throw Error("window_not_found", "The target window no longer exists");
    BringWindowToTop(hwnd);
    SetActiveWindow(hwnd);
    SetForegroundWindow(hwnd);

    while (Clock::now() < deadline) {
        context.check();
        if (!IsWindow(hwnd)) {
            throw Error("window_not_found", "The target window no longer exists");
        }
        if (foreground_matches(hwnd)) return true;
        Sleep(10);
    }
    throw Error("focus_denied", "The target window could not be brought to the foreground");
}

Json WindowsBackend::observe(const Json& params, Context& context) {
    purge_observations();
    auto target = require_target_window(params);
    if (json_bool(params, "activate", false)) {
        // Backgrounded UWP/WinUI windows collapse their UIA tree, so an opt-in
        // activation exists.  It restores the window and can move it, so the
        // rectangle every coordinate below is built from must be re-read.
        activate(target.hwnd, context);
        if (const auto activated = inspect_window(target.hwnd)) target = *activated;
    }
    const bool observeText = json_bool(params, "includeText", false) ||
                             json_string(params, "observe") == "text" ||
                             json_string(params, "observe") == "both";
    const bool includeScreenshot = json_bool(params, "includeScreenshot", true) &&
                                   json_string(params, "observe") != "text";
    const std::string format = lower(json_string(params, "format", "jpeg")) == "png" ? "png" : "jpeg";
    const int quality = std::clamp(json_int(params, "quality", 85), 1, 100);
    // maxEdge is an optional cap on the reduced image only; 0 means no cap.
    const int maxEdge = std::max(0, json_int(params, "maxEdge", 0));
    const std::string id = guid_string();
    const auto started = Clock::now();

    Json result{{"observationId", id},
                {"coordinateSpace", "window"},
                {"window", window_json(target)},
                {"overlayRegions", Json::array()},
                {"timings", Json::object()}};
    std::int64_t capturedAtUnixMs = 0;
    Observation observation;
    observation.id = id;
    observation.hwnd = target.hwnd;
    observation.rect = target.rect;
    if (includeScreenshot) {
        const auto captureStarted = Clock::now();
        const auto shot = capture_.capture(target.hwnd, target.rect,
                                           private_capture_path(id, format, "-full"),
                                           private_capture_path(id, format), format, quality,
                                           maxEdge, context);
        observation.hasScreenshot = true;
        observation.mimeType = shot.mimeType;
        observation.reduced = shot.reduced;
        observation.full = shot.full;
        result["screenshot"] = Json{{"path", shot.path}, {"mimeType", shot.mimeType},
                                     {"width", shot.width}, {"height", shot.height},
                                     {"variant", "reduced"},
                                     {"actionTransform", action_transform(shot.scaleX, shot.scaleY)},
                                     {"fullAvailable", true},
                                     {"fullWidth", shot.full.width},
                                     {"fullHeight", shot.full.height},
                                     {"scale", shot.scale}, {"backend", shot.backend},
                                     {"frameTimestamp100ns", shot.frameTimestamp100ns},
                                     {"freshFrame", shot.freshFrame},
                                     {"cachedFrame", shot.cachedFrame},
                                     {"sourceWidth", shot.sourceWidth},
                                     {"sourceHeight", shot.sourceHeight},
                                     {"scaleX", shot.scaleX},
                                     {"scaleY", shot.scaleY}};
        capturedAtUnixMs = shot.capturedAtUnixMs;
        result["timings"]["captureMs"] = std::chrono::duration_cast<std::chrono::milliseconds>(
            Clock::now() - captureStarted).count();
    }
    result["capturedAt"] = iso8601_from_unix_ms(capturedAtUnixMs);
    result["observedAt"] = iso8601_now();
    observation.created = Clock::now();
    observation.window = result["window"];
    if (observeText) {
        const auto accessibilityStarted = Clock::now();
        try {
            const auto accessibility = uia_.build(target.hwnd, target.rect, context);
            result["accessibility"] = accessibility.value;
            observation.elements = accessibility.elements;
        } catch (...) {
            // The observation is not cached, so nothing else would delete its images.
            remove_capture_file(observation.reduced.path);
            remove_capture_file(observation.full.path);
            throw;
        }
        result["timings"]["accessibilityMs"] =
            std::chrono::duration_cast<std::chrono::milliseconds>(Clock::now() - accessibilityStarted).count();
    }
    result["timings"]["totalMs"] = std::chrono::duration_cast<std::chrono::milliseconds>(
        Clock::now() - started).count();
    result["timings"]["accessibilityIncluded"] = observeText;
    {
        std::lock_guard lock(observationsMutex_);
        observations_[id] = std::move(observation);
    }
    return result;
}

Json WindowsBackend::full_screenshot(const Json& params) {
    const auto id = json_string(params, "observationId");
    if (id.empty()) throw Error("observation_required", "get-full-screenshot requires observationId");
    purge_observations();
    const auto target = target_window(params);
    std::lock_guard lock(observationsMutex_);
    const auto found = observations_.find(id);
    if (found == observations_.end()) {
        throw Error("stale_observation", "Observation is missing or expired; observe again");
    }
    const auto& observation = found->second;
    if (target && target->hwnd != observation.hwnd) {
        throw Error("stale_observation", "Observation belongs to a different window; observe again");
    }
    std::error_code error;
    if (!observation.hasScreenshot ||
        !std::filesystem::exists(std::filesystem::path(wide(observation.full.path)), error)) {
        throw Error("stale_observation",
                    "Observation has no full screenshot; observe again with a screenshot");
    }
    return Json{{"observationId", id},
                {"screenshot", {{"path", observation.full.path},
                                {"mimeType", observation.mimeType},
                                {"width", observation.full.width},
                                {"height", observation.full.height},
                                {"variant", "full"},
                                {"actionTransform", action_transform(observation.full.scaleX,
                                                                     observation.full.scaleY)}}},
                {"notice", "To click a point read from this image, pass --coords full."}};
}

Json WindowsBackend::action_result() {
    // The next observation must not consume a frame captured before this
    // action completed. Capture keeps this watermark independently of the
    // serialized dispatcher so a follow-up observe can wait for a new frame.
    capture_.mark_input_complete();
    return Json{{"delivered", true}, {"verification", {{"state", "unverified"}}}};
}

std::vector<WORD> WindowsBackend::modifiers(const Json& params) const {
    std::vector<std::string> names;
    const auto it = params.find("modifiers");
    if (it != params.end()) {
        if (it->is_array()) {
            for (const auto& item : *it) {
                if (item.is_string()) names.push_back(item.get<std::string>());
            }
        } else if (it->is_string()) {
            std::string value = it->get<std::string>();
            std::size_t start = 0;
            while (start < value.size()) {
                const auto end = value.find('+', start);
                names.push_back(value.substr(start, end == std::string::npos ? end : end - start));
                if (end == std::string::npos) break;
                start = end + 1;
            }
        }
    }
    std::vector<WORD> result;
    for (const auto& name : names) {
        const WORD key = modifier_vk(name);
        if (!key) throw Error("invalid_key", "Unknown modifier: " + name);
        if (std::find(result.begin(), result.end(), key) == result.end()) result.push_back(key);
    }
    return result;
}

void WindowsBackend::click_at(HWND hwnd, POINT point, MouseButton button,
                              const Json& params, Context& context) {
    activate(hwnd, context);
    send_mouse_move(point.x, point.y);
    const auto modifierKeys = modifiers(params);
    heldModifiers_.store(modifier_mask(modifierKeys), std::memory_order_release);
    publish_input_state();
    const DWORD buttonBit = button_bit(button);
    bool buttonDown = false;
    // Publish before the down event so a process termination in the tiny gap
    // cannot leave a button held without the watchdog knowing about it.
    heldButtons_.fetch_or(buttonBit, std::memory_order_release);
    publish_input_state();
    try {
        ModifierGuard modifierGuard(modifierKeys);
        context.check();
        send_mouse_button(button, true);
        buttonDown = true;
        send_mouse_button(button, false);
        buttonDown = false;
        heldButtons_.fetch_and(~buttonBit, std::memory_order_release);
        publish_input_state();
    } catch (...) {
        if (!buttonDown) heldButtons_.fetch_and(~buttonBit, std::memory_order_release);
        heldModifiers_.store(0, std::memory_order_release);
        publish_input_state();
        throw;
    }
    heldModifiers_.store(0, std::memory_order_release);
    publish_input_state();
}

void WindowsBackend::drag_at(HWND hwnd, POINT from, POINT to, MouseButton button,
                             int duration, int steps, int holdBefore, int holdAfter,
                             Context& context) {
    activate(hwnd, context);
    send_mouse_move(from.x, from.y);
    context.check();
    if (!foreground_matches(hwnd)) {
        throw Error("focus_lost", "Target window lost foreground focus before drag");
    }
    const DWORD buttonBit = button_bit(button);
    heldButtons_.fetch_or(buttonBit, std::memory_order_release);
    publish_input_state();
    try {
        send_mouse_button(button, true);
    } catch (...) {
        heldButtons_.fetch_and(~buttonBit, std::memory_order_release);
        publish_input_state();
        throw;
    }
    struct HeldButtonGuard {
        WindowsBackend& owner;
        MouseButton button;
        ~HeldButtonGuard() noexcept {
            INPUT input{};
            input.type = INPUT_MOUSE;
            input.mi.dwFlags = button_up_flag(button);
            if (SendInput(1, &input, sizeof(INPUT)) == 1) {
                owner.heldButtons_.fetch_and(~button_bit(button), std::memory_order_release);
            }
            owner.publish_input_state();
        }
    } guard{*this, button};
    sleep_context(context, holdBefore);
    const int safeSteps = std::max(1, steps);
    for (int step = 1; step <= safeSteps; ++step) {
        context.check();
        if (!foreground_matches(hwnd)) {
            throw Error("focus_lost", "Target window lost foreground focus during drag");
        }
        const double fraction = static_cast<double>(step) / safeSteps;
        const int x = static_cast<int>(std::lround(from.x + (to.x - from.x) * fraction));
        const int y = static_cast<int>(std::lround(from.y + (to.y - from.y) * fraction));
        send_mouse_move(x, y);
        const int elapsed = static_cast<int>(std::lround(
            static_cast<double>(duration) * step / safeSteps));
        const int previous = static_cast<int>(std::lround(
            static_cast<double>(duration) * (step - 1) / safeSteps));
        sleep_context(context, elapsed - previous);
    }
    sleep_context(context, holdAfter);
}

void WindowsBackend::send_chord(const std::string& chord, Context& context) {
    std::vector<std::string> parts;
    std::size_t start = 0;
    while (start < chord.size()) {
        const auto end = chord.find('+', start);
        parts.push_back(chord.substr(start, end == std::string::npos ? end : end - start));
        if (end == std::string::npos) break;
        start = end + 1;
    }
    if (parts.empty()) throw Error("invalid_key", "Empty key chord");
    std::vector<WORD> modifierKeys;
    std::vector<std::string> bases;
    for (const auto& part : parts) {
        if (const WORD modifier = modifier_vk(part)) modifierKeys.push_back(modifier);
        else bases.push_back(part);
    }
    if (bases.size() != 1) throw Error("invalid_key", "A key chord needs one non-modifier key");
    const WORD base = key_vk(bases.front());
    heldModifiers_.store(modifier_mask(modifierKeys), std::memory_order_release);
    publish_input_state();
    bool baseDown = false;
    try {
        ModifierGuard guard(modifierKeys);
        context.check();
        heldKey_.store(base, std::memory_order_release);
        heldKeyFlags_.store(extended_key(base) ? KEYEVENTF_EXTENDEDKEY : 0,
                             std::memory_order_release);
        publish_input_state();
        send_key(base, true);
        baseDown = true;
        send_key(base, false);
        baseDown = false;
        heldKey_.store(0, std::memory_order_release);
        heldKeyFlags_.store(0, std::memory_order_release);
        publish_input_state();
    } catch (...) {
        if (!baseDown) {
            heldKey_.store(0, std::memory_order_release);
            heldKeyFlags_.store(0, std::memory_order_release);
        }
        heldModifiers_.store(0, std::memory_order_release);
        publish_input_state();
        throw;
    }
    heldModifiers_.store(0, std::memory_order_release);
    publish_input_state();
}

void WindowsBackend::send_text(const std::string& text, Context& context) {
    const auto value = wide(text);
    for (const wchar_t character : value) {
        context.check();
        heldKey_.store(static_cast<WORD>(character), std::memory_order_release);
        heldKeyFlags_.store(KEYEVENTF_UNICODE, std::memory_order_release);
        publish_input_state();
        INPUT input{};
        input.type = INPUT_KEYBOARD;
        input.ki.wScan = static_cast<WORD>(character);
        input.ki.dwFlags = KEYEVENTF_UNICODE;
        bool keyDown = false;
        try {
            send_input(input);
            keyDown = true;
            input.ki.dwFlags = KEYEVENTF_UNICODE | KEYEVENTF_KEYUP;
            send_input(input);
            heldKey_.store(0, std::memory_order_release);
            heldKeyFlags_.store(0, std::memory_order_release);
            publish_input_state();
        } catch (...) {
            if (!keyDown) {
                heldKey_.store(0, std::memory_order_release);
                heldKeyFlags_.store(0, std::memory_order_release);
                publish_input_state();
            }
            throw;
        }
    }
}

Json WindowsBackend::handle_action(const std::string& method, const Json& params,
                                   Context& context) {
    if (method == "click" || method == "drag" || method == "scroll" || method == "type-text" ||
        method == "press-key" || method == "hotkey" || method == "set-value" ||
        method == "paste-text") {
        const auto target = require_target_window(params);
        const auto observationId = json_string(params, "observationId");
        Observation* observation = nullptr;
        if (!observationId.empty()) observation = &require_observation(params, target.hwnd, target.rect);
        const int targetWidth = target.rect.right - target.rect.left;
        const int targetHeight = target.rect.bottom - target.rect.top;
        // x/y are screenshot pixels (reduced by default, full with coords=full).
        // The observation's transform converts them to window-local points.
        const auto windowPoint = [&](const char* xKey, const char* yKey,
                                     const CoordinateTransform& transform) {
            const auto read = [&](const char* key, int imageLimit, double scale, int windowLimit) {
                const auto it = params.find(key);
                if (it == params.end() || !it->is_number()) {
                    throw Error("invalid_argument", std::string(key) + " is required");
                }
                const double value = it->get<double>();
                if (!std::isfinite(value) || value < 0 || value > imageLimit) {
                    throw Error("invalid_argument", std::string(key) + " is outside the " +
                                                        transform.space + " screenshot");
                }
                return std::clamp(static_cast<int>(std::lround(value / scale)), 0, windowLimit);
            };
            return POINT{read(xKey, transform.imageWidth, transform.scaleX, targetWidth),
                         read(yKey, transform.imageHeight, transform.scaleY, targetHeight)};
        };
        const auto pointJson = [](POINT point) { return Json{{"x", point.x}, {"y", point.y}}; };

        if (method == "click") {
            POINT point{};
            const auto element = params.find("elementIndex");
            if (element != params.end()) {
                if (!observation) throw Error("observation_required", "elementIndex requires observationId");
                auto& record = require_element(params, *observation, "elementIndex");
                activate(target.hwnd, context);
                if (!uia_.invoke(record, context)) {
                    require_clickable_bounds(record);
                    point.x = target.rect.left + (record.bounds.left + record.bounds.right) / 2;
                    point.y = target.rect.top + (record.bounds.top + record.bounds.bottom) / 2;
                    click_at(target.hwnd, point, mouse_button(json_string(params, "button")), params, context);
                }
            } else {
                const auto transform = coordinate_transform(params, target, observation);
                const POINT local = windowPoint("x", "y", transform);
                point.x = target.rect.left + local.x;
                point.y = target.rect.top + local.y;
                click_at(target.hwnd, point, mouse_button(json_string(params, "button")), params, context);
                auto result = action_result();
                result["coordinateSpace"] = transform.space;
                result["windowPoint"] = pointJson(local);
                return result;
            }
            return action_result();
        }
        if (method == "drag") {
            std::optional<CoordinateTransform> transform;
            auto coordinate = [&](const char* xKey, const char* yKey, const char* elementKey) {
                POINT point{};
                const auto element = params.find(elementKey);
                if (element != params.end()) {
                    if (!observation) throw Error("observation_required", "Element drag requires observationId");
                    auto& record = require_element(params, *observation, elementKey);
                    require_clickable_bounds(record);
                    point.x = (record.bounds.left + record.bounds.right) / 2;
                    point.y = (record.bounds.top + record.bounds.bottom) / 2;
                } else {
                    if (!transform) transform = coordinate_transform(params, target, observation);
                    point = windowPoint(xKey, yKey, *transform);
                }
                return point;
            };
            const POINT localFrom = coordinate("fromX", "fromY", "fromElementIndex");
            const POINT localTo = coordinate("toX", "toY", "toElementIndex");
            const POINT from{target.rect.left + localFrom.x, target.rect.top + localFrom.y};
            const POINT to{target.rect.left + localTo.x, target.rect.top + localTo.y};
            drag_at(target.hwnd, from, to, mouse_button(json_string(params, "button")),
                    std::max(0, json_int(params, "durationMs", 240)),
                    std::max(1, json_int(params, "steps", 12)),
                    std::max(0, json_int(params, "holdBeforeMs", 50)),
                    std::max(0, json_int(params, "holdAfterMs", 50)), context);
            auto result = action_result();
            if (transform) {
                result["coordinateSpace"] = transform->space;
                result["windowFrom"] = pointJson(localFrom);
                result["windowTo"] = pointJson(localTo);
            }
            return result;
        }
        if (method == "scroll") {
            POINT local{targetWidth / 2, targetHeight / 2};
            std::optional<CoordinateTransform> transform;
            if (params.contains("x") || params.contains("y")) {
                transform = coordinate_transform(params, target, observation);
                local = windowPoint("x", "y", *transform);
            }
            activate(target.hwnd, context);
            send_mouse_move(target.rect.left + local.x, target.rect.top + local.y);
            const auto direction = lower(json_string(params, "direction", "down"));
            const int amount = std::clamp(json_int(params, "amount", 3), -100, 100);
            INPUT input{};
            input.type = INPUT_MOUSE;
            if (direction == "left" || direction == "right") {
                input.mi.dwFlags = MOUSEEVENTF_HWHEEL;
                input.mi.mouseData = static_cast<DWORD>((direction == "left" ? -1 : 1) * amount * WHEEL_DELTA);
            } else {
                input.mi.dwFlags = MOUSEEVENTF_WHEEL;
                input.mi.mouseData = static_cast<DWORD>((direction == "up" ? 1 : -1) * amount * WHEEL_DELTA);
            }
            send_input(input);
            auto result = action_result();
            if (transform) result["coordinateSpace"] = transform->space;
            result["windowPoint"] = pointJson(local);
            return result;
        }
        if (method == "type-text") {
            activate(target.hwnd, context);
            send_text(json_string(params, "text"), context);
            return action_result();
        }
        if (method == "press-key" || method == "hotkey") {
            activate(target.hwnd, context);
            std::string chord = json_string(params, "key");
            if (chord.empty()) chord = json_string(params, "keys");
            send_chord(chord, context);
            return action_result();
        }
        if (method == "set-value") {
            if (!observation) throw Error("observation_required", "set-value requires observationId");
            auto& record = require_element(params, *observation, "elementIndex");
            activate(target.hwnd, context);
            if (!uia_.set_value(record, wide(json_string(params, "value")), context)) {
                throw Error("pattern_unavailable", "The target element has no ValuePattern");
            }
            return action_result();
        }
        if (method == "paste-text") {
            activate(target.hwnd, context);
            const auto text = wide(json_string(params, "text"));
            HGLOBAL oldClipboard = nullptr;
            std::wstring oldText;
            if (OpenClipboard(nullptr)) {
                if (HANDLE data = GetClipboardData(CF_UNICODETEXT)) {
                    if (const auto* value = static_cast<const wchar_t*>(GlobalLock(data))) {
                        oldText.assign(value);
                        GlobalUnlock(data);
                    }
                }
                EmptyClipboard();
                const SIZE_T bytes = (text.size() + 1) * sizeof(wchar_t);
                HGLOBAL memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
                if (!memory) {
                    CloseClipboard();
                    throw Error("clipboard_failed", "GlobalAlloc for clipboard text failed");
                }
                std::memcpy(GlobalLock(memory), text.c_str(), bytes);
                GlobalUnlock(memory);
                SetClipboardData(CF_UNICODETEXT, memory);
                CloseClipboard();
            } else {
                throw Error("clipboard_failed", "OpenClipboard failed");
            }
            send_chord("ctrl+v", context);
            sleep_context(context, 150);
            if (OpenClipboard(nullptr)) {
                EmptyClipboard();
                if (!oldText.empty()) {
                    const SIZE_T bytes = (oldText.size() + 1) * sizeof(wchar_t);
                    HGLOBAL memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
                    if (memory) {
                        std::memcpy(GlobalLock(memory), oldText.c_str(), bytes);
                        GlobalUnlock(memory);
                        SetClipboardData(CF_UNICODETEXT, memory);
                    }
                }
                CloseClipboard();
            }
            return action_result();
        }
    }
    throw Error("unsupported_method", "Unsupported Windows computer-use method: " + method);
}

Json WindowsBackend::list_apps() const {
    std::map<std::string, Json> apps;
    for (const auto& info : enumerate_windows()) {
        const auto key = lower(info.app);
        if (key.empty()) continue;
        auto& app = apps[key];
        if (app.is_null()) app = Json{{"id", info.app}, {"name", info.app}, {"windows", Json::array()}};
        app["windows"].push_back(window_json(info));
    }
    Json result = Json::array();
    for (auto& entry : apps) result.push_back(std::move(entry.second));
    return Json{{"apps", std::move(result)}};
}

Json WindowsBackend::list_windows(const Json& params) const {
    const auto wanted = lower(json_string(params, "app"));
    Json result = Json::array();
    for (const auto& info : enumerate_windows()) {
        if (wanted.empty() || app_name_matches(info, wanted)) {
            result.push_back(window_json(info));
        }
    }
    return Json{{"windows", std::move(result)}};
}

Json WindowsBackend::doctor() const {
    HDESK desktop = OpenInputDesktop(0, FALSE, GENERIC_READ);
    const bool interactive = desktop != nullptr;
    if (desktop) CloseDesktop(desktop);
    return Json{{"platform", "windows"},
                {"interactiveDesktop", interactive},
                {"uia", true},
                {"windowsGraphicsCapture", true},
                {"d3d11", true},
                {"wic", true},
                {"stopKey", "Escape x2"},
                {"edgeBorder", interactive},
                {"indicator", interactive},
                {"ready", interactive},
                {"notes", interactive ? Json::array() : Json{"An unlocked interactive desktop is required"}}};
}

Json WindowsBackend::capabilities() const {
    return Json{{"platform", "windows"},
                {"supports", {{"screenshot", true}, {"accessibility", true},
                               {"wgc", true}, {"jpeg", true}, {"png", true},
                               {"drag", true}, {"nativeApps", true}, {"games", true},
                               {"overlay", true}, {"hotkeyStop", true},
                               {"stopKey", "Escape x2"}, {"edgeBorder", true}}},
                {"coordinateSpace", "reduced"},
                {"defaults", {{"durationMs", 240}, {"steps", 12},
                               {"holdBeforeMs", 50}, {"holdAfterMs", 50},
                               {"quality", 85}, {"maxEdge", 0}}}};
}

void WindowsBackend::stop_session() noexcept {
    sessionActive_.store(false, std::memory_order_release);
    interrupted_.store(true, std::memory_order_release);
    if (auto* context = currentContext_.load(std::memory_order_acquire)) context->cancelled.store(true);
    if (auto* context = sessionContext_.load(std::memory_order_acquire)) context->cancelled.store(true);
    release_held_input();
    indicator_.stop();
    clear_observations();
    {
        std::lock_guard lock(observationsMutex_);
        sessionId_.clear();
        sessionContext_.store(nullptr, std::memory_order_release);
    }
    capture_.reset();
}

void WindowsBackend::on_stop_key() noexcept {
    sessionActive_.store(false, std::memory_order_release);
    interrupted_.store(true, std::memory_order_release);
    if (auto* context = currentContext_.load(std::memory_order_acquire)) context->cancelled.store(true);
    if (auto* context = sessionContext_.load(std::memory_order_acquire)) context->cancelled.store(true);
    release_held_input();
}

void WindowsBackend::publish_input_state() noexcept {
    watchdog_.publish(heldButtons_.load(std::memory_order_acquire),
                      heldModifiers_.load(std::memory_order_acquire),
                      heldKey_.load(std::memory_order_acquire),
                      heldKeyFlags_.load(std::memory_order_acquire));
}

void WindowsBackend::release_held_input() noexcept {
    const DWORD buttons = heldButtons_.load(std::memory_order_acquire);
    for (const auto [bit, button] : {std::pair<DWORD, MouseButton>{1, MouseButton::left},
                                     {2, MouseButton::right}, {4, MouseButton::middle}}) {
        if (buttons & bit) {
            INPUT input{};
            input.type = INPUT_MOUSE;
            input.mi.dwFlags = button_up_flag(button);
            if (SendInput(1, &input, sizeof(INPUT)) == 1) {
                heldButtons_.fetch_and(~bit, std::memory_order_acq_rel);
            }
        }
    }
    const DWORD modifiers = heldModifiers_.load(std::memory_order_acquire);
    for (const auto [bit, key] : {std::pair<DWORD, WORD>{1, VK_SHIFT},
                                  {2, VK_CONTROL}, {4, VK_MENU}, {8, VK_LWIN}}) {
        if (modifiers & bit) {
            INPUT input{};
            input.type = INPUT_KEYBOARD;
            input.ki.wVk = key;
            input.ki.dwFlags = KEYEVENTF_KEYUP | (extended_key(key) ? KEYEVENTF_EXTENDEDKEY : 0);
            if (SendInput(1, &input, sizeof(INPUT)) == 1) {
                heldModifiers_.fetch_and(~bit, std::memory_order_acq_rel);
            }
        }
    }
    const WORD key = heldKey_.load(std::memory_order_acquire);
    const DWORD keyFlags = heldKeyFlags_.load(std::memory_order_acquire);
    if (key) {
        INPUT input{};
        input.type = INPUT_KEYBOARD;
        if (keyFlags & KEYEVENTF_UNICODE) input.ki.wScan = key;
        else input.ki.wVk = key;
        input.ki.dwFlags = keyFlags | KEYEVENTF_KEYUP;
        if (SendInput(1, &input, sizeof(INPUT)) == 1) {
            heldKey_.store(0, std::memory_order_release);
            heldKeyFlags_.store(0, std::memory_order_release);
        }
    }
    publish_input_state();
}

void WindowsBackend::interrupt() noexcept {
    interrupted_.store(true, std::memory_order_release);
    if (auto* context = currentContext_.load(std::memory_order_acquire)) context->cancelled.store(true);
    if (auto* context = sessionContext_.load(std::memory_order_acquire)) context->cancelled.store(true);
    release_held_input();
    indicator_.request_stop_async();
}

} // namespace dcu::windows

namespace dcu {
std::unique_ptr<Backend> make_backend() {
    return std::make_unique<windows::WindowsBackend>();
}
} // namespace dcu
