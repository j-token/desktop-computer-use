#pragma once

#include <atomic>
#include <stdexcept>
#include <string>
#include <utility>

namespace dcu {

struct Error : std::runtime_error {
    std::string code;

    Error(std::string error_code, std::string message)
        : std::runtime_error(std::move(message)), code(std::move(error_code)) {}
};

struct Context {
    std::atomic_bool cancelled{false};

    void check() const {
        if (cancelled.load()) throw Error("cancelled", "Session stopped");
    }
};

} // namespace dcu
