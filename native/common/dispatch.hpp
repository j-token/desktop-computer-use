#pragma once
#include "dcu/backend.hpp"
#include "input_lease.hpp"
#include <chrono>
#include <mutex>
#include <functional>
#include <set>

namespace dcu {
class Dispatcher {
    Backend& backend_;
    std::string token_;
    std::mutex operationMutex_;
    std::mutex stateMutex_;
    Context context_;
    InputLease inputLease_;
    std::string sessionId_;
    bool active_ = false;
    bool stopping_ = false;
    std::chrono::steady_clock::time_point lastActivity_ = std::chrono::steady_clock::now();
    std::chrono::milliseconds idleTimeout_;
    std::set<std::string> toggles_; // Guarded by stateMutex_; empty whenever no session is active.
    std::atomic<std::int64_t> operationStartedMilliseconds_{0};
    std::atomic_bool stopRequested_{false};
    Json stop();
    void finish_pending_stop();
    void append_observation(Json& result, Json params);
    Json execute_serialized(const std::string& method, Json params);
    Json execute_toggle(const std::string& method, const Json& params);
    Json toggle_details();
    Json error_details(bool authenticated);
public:
    std::atomic_bool quitting{false};
    Dispatcher(Backend& backend, std::string token,
               std::chrono::milliseconds idleTimeout = std::chrono::seconds(120))
        : backend_(backend), token_(std::move(token)), idleTimeout_(idleTimeout) {}
    Json handle(const Json& request);
    void tick();
    void shutdown();
};
void validate_params(const std::string& method, const Json& params);
bool is_input_action(const std::string& method);
// Canonical toggle key name (shift, ctrl, alt, win, space), or empty when unknown.
std::string canonical_toggle_key(const std::string& key);
int serve(const std::string& endpoint, const std::string& token);
}
