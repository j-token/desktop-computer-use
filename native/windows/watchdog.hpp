#pragma once

#include <cstdint>
#include <string>

namespace dcu::windows {

// Called by the common executable's early --input-watchdog branch. The
// watchdog opens the parent's named state mapping and releases only the
// buttons, modifiers (including toggled keys), and key recorded by the DCU
// process after the parent exits.
int run_input_watchdog(std::uint32_t parentPid, const std::string& mappingName) noexcept;

class InputWatchdog final {
public:
    InputWatchdog();
    ~InputWatchdog();

    InputWatchdog(const InputWatchdog&) = delete;
    InputWatchdog& operator=(const InputWatchdog&) = delete;

    void publish(std::uint32_t buttons, std::uint32_t modifiers,
                 std::uint16_t key = 0, std::uint32_t keyFlags = 0) noexcept;

private:
    struct Impl;
    Impl* impl_ = nullptr;
};

} // namespace dcu::windows
