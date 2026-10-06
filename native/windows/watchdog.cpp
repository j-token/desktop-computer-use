#include "watchdog.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <array>
#include <cstring>
#include <vector>

namespace dcu::windows {
namespace {

// Version 2 adds the space bit (16) to modifiers for toggled keys.
constexpr std::uint32_t kVersion = 2;

#pragma pack(push, 1)
struct SharedInputState {
    std::uint32_t version = kVersion;
    volatile LONG alive = 1;
    volatile LONG buttons = 0;
    volatile LONG modifiers = 0;
    volatile LONG key = 0;
    volatile LONG keyFlags = 0;
};
#pragma pack(pop)

std::wstring widen(const std::string& value) {
    if (value.empty()) return {};
    const int size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                                         static_cast<int>(value.size()), nullptr, 0);
    if (size <= 0) return {};
    std::wstring result(static_cast<std::size_t>(size), L'\0');
    MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                        static_cast<int>(value.size()), result.data(), size);
    return result;
}

void release_tracked(std::uint32_t buttons, std::uint32_t modifiers,
                     std::uint16_t key, std::uint32_t keyFlags) noexcept {
    struct Button { std::uint32_t bit; DWORD flag; };
    for (const auto button : std::array<Button, 3>{{{1, MOUSEEVENTF_LEFTUP},
                                                    {2, MOUSEEVENTF_RIGHTUP},
                                                    {4, MOUSEEVENTF_MIDDLEUP}}}) {
        if (!(buttons & button.bit)) continue;
        INPUT input{};
        input.type = INPUT_MOUSE;
        input.mi.dwFlags = button.flag;
        SendInput(1, &input, sizeof(INPUT));
    }
    struct Modifier { std::uint32_t bit; WORD key; };
    for (const auto modifier : std::array<Modifier, 5>{{{1, VK_SHIFT},
                                                        {2, VK_CONTROL},
                                                        {4, VK_MENU},
                                                        {8, VK_LWIN},
                                                        {16, VK_SPACE}}}) {
        if (!(modifiers & modifier.bit)) continue;
        INPUT input{};
        input.type = INPUT_KEYBOARD;
        input.ki.wVk = modifier.key;
        input.ki.dwFlags = KEYEVENTF_KEYUP | ((modifier.key == VK_LWIN) ? KEYEVENTF_EXTENDEDKEY : 0);
        SendInput(1, &input, sizeof(INPUT));
    }
    if (key) {
        INPUT input{};
        input.type = INPUT_KEYBOARD;
        if (keyFlags & KEYEVENTF_UNICODE) input.ki.wScan = key;
        else input.ki.wVk = key;
        input.ki.dwFlags = keyFlags | KEYEVENTF_KEYUP;
        SendInput(1, &input, sizeof(INPUT));
    }

}
} // namespace

struct InputWatchdog::Impl {
    HANDLE mappingHandle = nullptr;
    SharedInputState* state = nullptr;
    HANDLE process = nullptr;
    std::wstring mappingName;
};

InputWatchdog::InputWatchdog() : impl_(new Impl()) {
    wchar_t name[96]{};
    swprintf_s(name, L"Local\\DCU.Input.%lu.%llu", GetCurrentProcessId(),
               static_cast<unsigned long long>(GetTickCount64()));
    impl_->mappingName = name;
    impl_->mappingHandle = CreateFileMappingW(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE,
                                               0, sizeof(SharedInputState), name);
    if (!impl_->mappingHandle) return;
    impl_->state = static_cast<SharedInputState*>(MapViewOfFile(
        impl_->mappingHandle, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(SharedInputState)));
    if (!impl_->state) return;
    std::memset(impl_->state, 0, sizeof(*impl_->state));
    impl_->state->version = kVersion;
    impl_->state->alive = 1;

    wchar_t executable[MAX_PATH * 4]{};
    const DWORD length = GetModuleFileNameW(nullptr, executable, static_cast<DWORD>(std::size(executable)));
    if (!length || length >= std::size(executable)) return;
    std::wstring command = L"\"" + std::wstring(executable, length) + L"\" --input-watchdog " +
                           std::to_wstring(GetCurrentProcessId()) + L" \"" + impl_->mappingName + L"\"";
    std::vector<wchar_t> commandLine(command.begin(), command.end());
    commandLine.push_back(L'\0');
    STARTUPINFOW startup{sizeof(startup)};
    PROCESS_INFORMATION processInfo{};
    if (CreateProcessW(executable, commandLine.data(), nullptr, nullptr, FALSE,
                       CREATE_NO_WINDOW, nullptr, nullptr, &startup, &processInfo)) {
        CloseHandle(processInfo.hThread);
        impl_->process = processInfo.hProcess;
    }
}

InputWatchdog::~InputWatchdog() {
    if (!impl_) return;
    if (impl_->state) {
        InterlockedExchange(&impl_->state->buttons, 0);
        InterlockedExchange(&impl_->state->modifiers, 0);
        InterlockedExchange(&impl_->state->key, 0);
        InterlockedExchange(&impl_->state->keyFlags, 0);
        InterlockedExchange(&impl_->state->alive, 0);
        UnmapViewOfFile(impl_->state);
    }
    if (impl_->process) {
        TerminateProcess(impl_->process, 0);
        CloseHandle(impl_->process);
    }
    if (impl_->mappingHandle) CloseHandle(impl_->mappingHandle);
    delete impl_;
}

void InputWatchdog::publish(std::uint32_t buttons, std::uint32_t modifiers,
                            std::uint16_t key, std::uint32_t keyFlags) noexcept {
    if (!impl_ || !impl_->state) return;
    InterlockedExchange(&impl_->state->buttons, static_cast<LONG>(buttons));
    InterlockedExchange(&impl_->state->modifiers, static_cast<LONG>(modifiers));
    InterlockedExchange(&impl_->state->key, static_cast<LONG>(key));
    InterlockedExchange(&impl_->state->keyFlags, static_cast<LONG>(keyFlags));
}

int run_input_watchdog(std::uint32_t parentPid, const std::string& mappingName) noexcept {
    try {
        const std::wstring name = widen(mappingName);
        if (name.empty()) return 2;
        HANDLE mapping = OpenFileMappingW(FILE_MAP_READ | FILE_MAP_WRITE, FALSE, name.c_str());
        if (!mapping) return 3;
        auto* state = static_cast<SharedInputState*>(MapViewOfFile(
            mapping, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, sizeof(SharedInputState)));
        if (!state || state->version != kVersion) {
            if (state) UnmapViewOfFile(state);
            CloseHandle(mapping);
            return 4;
        }
        HANDLE parent = OpenProcess(SYNCHRONIZE, FALSE, parentPid);
        if (!parent) {
            const auto buttons = static_cast<std::uint32_t>(InterlockedCompareExchange(
                &state->buttons, 0, 0));
            const auto modifiers = static_cast<std::uint32_t>(InterlockedCompareExchange(
                &state->modifiers, 0, 0));
            const auto key = static_cast<std::uint16_t>(InterlockedCompareExchange(
                &state->key, 0, 0));
            const auto keyFlags = static_cast<std::uint32_t>(InterlockedCompareExchange(
                &state->keyFlags, 0, 0));
            release_tracked(buttons, modifiers, key, keyFlags);
            InterlockedExchange(&state->buttons, 0);
            InterlockedExchange(&state->modifiers, 0);
            InterlockedExchange(&state->key, 0);
            InterlockedExchange(&state->keyFlags, 0);
            UnmapViewOfFile(state);
            CloseHandle(mapping);
            return 0;
        }
        for (;;) {
            const DWORD wait = WaitForSingleObject(parent, 250);
            if (wait == WAIT_OBJECT_0 ||
                InterlockedCompareExchange(&state->alive, 1, 1) == 0) {
                const auto buttons = static_cast<std::uint32_t>(InterlockedCompareExchange(
                    &state->buttons, 0, 0));
                const auto modifiers = static_cast<std::uint32_t>(InterlockedCompareExchange(
                    &state->modifiers, 0, 0));
                const auto key = static_cast<std::uint16_t>(InterlockedCompareExchange(
                    &state->key, 0, 0));
                const auto keyFlags = static_cast<std::uint32_t>(InterlockedCompareExchange(
                    &state->keyFlags, 0, 0));
                if (wait == WAIT_OBJECT_0) release_tracked(buttons, modifiers, key, keyFlags);
                InterlockedExchange(&state->buttons, 0);
                InterlockedExchange(&state->modifiers, 0);
                InterlockedExchange(&state->key, 0);
                InterlockedExchange(&state->keyFlags, 0);
                break;
            }
            if (wait == WAIT_FAILED) break;
        }
        CloseHandle(parent);
        UnmapViewOfFile(state);
        CloseHandle(mapping);
        return 0;
    } catch (...) {
        return 5;
    }
}

} // namespace dcu::windows

namespace dcu {
int run_input_watchdog(unsigned long parentPid, const std::string& mappingName) {
    return windows::run_input_watchdog(static_cast<std::uint32_t>(parentPid), mappingName);
}
} // namespace dcu
