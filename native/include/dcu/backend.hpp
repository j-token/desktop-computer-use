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
// The daemon serializes execute() unless supports_concurrent_execute() is true;
// it never runs session.start or session.stop alongside another call, and it
// runs at most one physical input (or toggle call) at a time. interrupt() must
// be thread-safe and nonblocking, and must cancel every running call.
class Backend {
public:
    virtual ~Backend() = default;
    virtual bool supports_concurrent_execute() const noexcept { return false; }
    virtual Json execute(const std::string& method, const Json& params, Context& context) = 0;
    virtual void interrupt() noexcept = 0;
    // Toggle keys stay held across requests: shift, ctrl, alt, win, space.
    // Both calls are serialized like execute(). Every session end (interrupt,
    // session.stop, destruction) must also release toggled keys.
    virtual void set_toggle(const std::string& key, bool down, Context& context) = 0;
    virtual void release_toggles() noexcept = 0;
};
std::unique_ptr<Backend> make_backend();
#ifdef _WIN32
namespace windows {
int run_input_watchdog(std::uint32_t parent_pid, const std::string& mapping_name) noexcept;
}
#endif
}
