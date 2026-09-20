#pragma once
#include <atomic>
#include <cstdint>
#include <memory>
#include <stdexcept>
#include <string>
#include <nlohmann/json.hpp>

namespace dcu {
using Json = nlohmann::json;
struct Error : std::runtime_error {
    std::string code;
    Error(std::string c, std::string message) : std::runtime_error(std::move(message)), code(std::move(c)) {}
};
struct Context {
    std::atomic_bool cancelled{false};
    void check() const { if (cancelled.load()) throw Error("cancelled", "Session stopped"); }
};
// Main daemon serializes execute(); interrupt() must be thread-safe and nonblocking.
class Backend {
public:
    virtual ~Backend() = default;
    virtual Json execute(const std::string& method, const Json& params, Context& context) = 0;
    virtual void interrupt() noexcept = 0;
};
std::unique_ptr<Backend> make_backend();
#ifdef _WIN32
namespace windows {
int run_input_watchdog(std::uint32_t parent_pid, const std::string& mapping_name) noexcept;
}
#endif
}
