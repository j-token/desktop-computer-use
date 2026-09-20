#pragma once
#include "dcu/backend.hpp"
#include "input_lease.hpp"
#include <chrono>
#include <mutex>
#include <functional>

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
    std::atomic<std::int64_t> operationStartedMilliseconds_{0};
    std::atomic_bool stopRequested_{false};
    Json stop();
    void finish_pending_stop();
    void append_observation(Json& result, Json params);
    Json execute_serialized(const std::string& method, Json params);
public:
    std::atomic_bool quitting{false};
    Dispatcher(Backend& backend, std::string token) : backend_(backend), token_(std::move(token)) {}
    Json handle(const Json& request);
    void tick();
    void shutdown();
};
void validate_params(const std::string& method, const Json& params);
bool is_input_action(const std::string& method);
int serve(const std::string& endpoint, const std::string& token);
}
