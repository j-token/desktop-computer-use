#pragma once
#include "dcu/backend.hpp"
#include "input_lease.hpp"
#include <chrono>
#include <map>
#include <mutex>
#include <functional>
#include <optional>
#include <set>
#include <shared_mutex>

namespace dcu {
// One desktop session (indicator, stop key, input lease) is shared by every
// agent session that joined it. Agents run their operations at the same time:
// observations and headless actions proceed in parallel, while physical input
// takes turns because there is only one pointer and one keyboard. A backend
// that cannot run calls concurrently is still called one at a time.
class Dispatcher {
    struct AgentSession {
        std::string agentId;
        bool multiAgent = false;
        bool headless = false;
        bool busy = false;
        std::chrono::steady_clock::time_point lastActivity;
    };
    struct RunningOperation {
        std::string sessionId;
        std::int64_t startedMilliseconds = 0; // 0 while the operation waits for its turn.
    };

    Backend& backend_;
    std::string token_;
    // Exclusive while the desktop session starts or stops; shared by every
    // other operation. Lock order: desktopMutex_, physicalInputMutex_,
    // backendMutex_, stateMutex_.
    std::shared_timed_mutex desktopMutex_;
    std::timed_mutex physicalInputMutex_;
    std::timed_mutex backendMutex_;
    std::mutex stateMutex_;
    // Desktop-wide cancellation (Esc, stop, lost indicator). Each operation
    // runs under its own Context so stopping one agent leaves the others alone.
    Context context_;
    InputLease inputLease_;
    std::string desktopSessionId_; // Written only under an exclusive desktopMutex_.
    std::map<std::string, AgentSession> sessions_;      // Guarded by stateMutex_.
    std::map<Context*, RunningOperation> operations_;    // Guarded by stateMutex_.
    int nextAgentNumber_ = 1;
    bool stopping_ = false;
    std::chrono::milliseconds idleTimeout_;
    std::chrono::milliseconds queueTimeout_;
    std::set<std::string> toggles_; // Guarded by stateMutex_; empty whenever no session is active.
    std::string toggleOwner_;       // Session that holds toggles_, empty once its session ended.
    std::atomic_bool stopRequested_{false};
    std::atomic_bool orphanedTogglesPending_{false};

    Json stop();
    std::optional<Json> stop_agent_session(const std::string& sessionId);
    void settle_pending_cleanup();
    void finish_pending_stop();
    void release_orphaned_toggles();
    void append_observation(Json& result, Json params, Context& operation);
    Json start_session(const Json& params);
    Json execute_operation(const std::string& method, Json params);
    Json execute_toggle(const std::string& method, const Json& params, const std::string& sessionId,
                        Context& operation);
    void enforce_toggle_owner(const std::string& method, const Json& params, const std::string& sessionId);
    Json session_status(const std::string& requester);
    Json toggle_details(const std::string& requester);
    Json error_details(bool authenticated, const std::string& requester);
public:
    std::atomic_bool quitting{false};
    Dispatcher(Backend& backend, std::string token,
               std::chrono::milliseconds idleTimeout = std::chrono::seconds(120),
               std::chrono::milliseconds queueTimeout = std::chrono::seconds(45))
        : backend_(backend), token_(std::move(token)), idleTimeout_(idleTimeout),
          queueTimeout_(queueTimeout) {}
    Json handle(const Json& request);
    void tick();
    void shutdown();
};
void validate_params(const std::string& method, const Json& params);
// Refuses, before any input, an action that cannot reach the target without the
// user's pointer or keyboard while params["headless"] is true.
void validate_headless_input(const std::string& method, const Json& params);
bool is_input_action(const std::string& method);
// Canonical toggle key name (shift, ctrl, alt, win, space), or empty when unknown.
std::string canonical_toggle_key(const std::string& key);
int serve(const std::string& endpoint, const std::string& token);
}
