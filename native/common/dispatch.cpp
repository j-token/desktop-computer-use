#include "dispatch.hpp"
#include <algorithm>
#include <iomanip>
#include <random>
#include <set>
#include <sstream>
#include <vector>

namespace dcu {
namespace {
constexpr std::int64_t operationTimeoutMilliseconds = 120000;
constexpr std::size_t maximumAgentSessions = 16;

std::int64_t monotonic_milliseconds() {
    const auto elapsed = std::chrono::steady_clock::now().time_since_epoch();
    return std::chrono::duration_cast<std::chrono::milliseconds>(elapsed).count();
}

bool tokens_match(const std::string& supplied, const std::string& expected) {
    if (supplied.size() != expected.size()) return false;

    unsigned char difference = 0;
    for (std::size_t index = 0; index < supplied.size(); ++index) {
        difference |= static_cast<unsigned char>(supplied[index] ^ expected[index]);
    }
    return difference == 0;
}

void authenticate_request(const Json& request, const Json& requestId, const std::string& token) {
    const bool hasValidId = requestId.is_string() && requestId.get_ref<const std::string&>().size() <= 256;
    if (!request.is_object() || !hasValidId) {
        throw Error("unauthorized", "Invalid request credentials");
    }
    if (!request.contains("token") || !request["token"].is_string()) {
        throw Error("unauthorized", "Invalid request credentials");
    }
    if (!tokens_match(request["token"].get_ref<const std::string&>(), token)) {
        throw Error("unauthorized", "Invalid request credentials");
    }
}

std::string create_session_id() {
    std::random_device random;
    std::ostringstream identifier;
    for (int part = 0; part < 8; ++part) {
        identifier << std::hex << std::setfill('0') << std::setw(8) << random();
    }
    return identifier.str();
}

Json error_response(const Json& requestId, const std::string& code, const std::string& message,
                    const Json& details = Json()) {
    Json error{{"code", code}, {"message", message}};
    if (details.is_object()) error["details"] = details;
    return {{"id", requestId}, {"ok", false}, {"error", std::move(error)}};
}

std::string join_toggles(const std::set<std::string>& toggles) {
    std::string text;
    for (const auto& key : toggles) text += (text.empty() ? "" : ", ") + key;
    return text;
}

std::string string_param(const Json& params, const char* key) {
    const auto found = params.find(key);
    return found != params.end() && found->is_string() ? found->get<std::string>() : std::string{};
}

// Moving the pointer, pressing keys, or bringing a window to the front would
// interleave with another agent's physical input, so these take turns.
bool needs_physical_input(const std::string& method, const Json& params) {
    if (method.starts_with("toggle.") && method != "toggle.status") return true;
    if (is_input_action(method)) return !params.value("headless", false);
    return method == "get-app-state" && params.value("activate", false);
}
} // namespace

Json Dispatcher::stop() {
    context_.cancelled = true;
    stopRequested_ = true;
    {
        std::lock_guard lock(stateMutex_);
        stopping_ = !sessions_.empty();
        sessions_.clear();
        // Every stop path (user Esc, idle expiry, operation timeout, client
        // disconnect, shutdown) ends the toggles; interrupt() and the backend's
        // session.stop release the physical keys.
        toggles_.clear();
        toggleOwner_.clear();
        for (auto& [operation, running] : operations_) operation->cancelled = true;
    }
    orphanedTogglesPending_ = false;
    backend_.interrupt();
    return {{"stopped", true}};
}

std::optional<Json> Dispatcher::stop_agent_session(const std::string& sessionId) {
    std::lock_guard lock(stateMutex_);
    const auto found = sessions_.find(sessionId);
    if (found == sessions_.end()) {
        if (sessions_.empty()) return std::nullopt;
        throw Error("session_required",
                    "Unknown sessionId; omit sessionId to stop every session on this desktop");
    }
    // The last session, or a desktop that is already ending, takes the full stop path.
    if (sessions_.size() == 1 || context_.cancelled) return std::nullopt;
    if (toggleOwner_ == sessionId && !toggles_.empty()) {
        throw Error("toggles_active", "Toggle still on: " + join_toggles(toggles_) +
                                          ". Run `dcu toggle off --all` before stopping the session.");
    }
    const auto agentId = found->second.agentId;
    sessions_.erase(found);
    for (auto& [operation, running] : operations_) {
        if (running.sessionId == sessionId) operation->cancelled = true;
    }
    return Json{{"stopped", true}, {"agentId", agentId}, {"agentCount", sessions_.size()}};
}

void Dispatcher::settle_pending_cleanup() {
    if (!stopRequested_ && !orphanedTogglesPending_) return;
    std::unique_lock desktop(desktopMutex_, std::defer_lock);
    if (!desktop.try_lock_for(queueTimeout_)) {
        throw Error("busy", "The desktop session is still stopping; retry shortly");
    }
    finish_pending_stop();
    release_orphaned_toggles();
}

void Dispatcher::finish_pending_stop() {
    // Caller owns desktopMutex_ exclusively. Urgent stop never waits for this
    // cleanup, and the desktop lease remains held until backend cleanup completes.
    if (!stopRequested_.exchange(false)) return;

    backend_.release_toggles();
    try {
        backend_.execute("session.stop", Json::object(), context_);
    } catch (...) {
        // interrupt() already requested input release. Cleanup is best effort
        // when a provider has disconnected or its target window has closed.
    }
    inputLease_.release();

    std::lock_guard lock(stateMutex_);
    stopping_ = false;
    desktopSessionId_.clear();
}

void Dispatcher::release_orphaned_toggles() {
    // Caller owns desktopMutex_ exclusively, so no physical input is running.
    if (!orphanedTogglesPending_.exchange(false)) return;
    backend_.release_toggles();
    std::lock_guard lock(stateMutex_);
    if (toggleOwner_.empty()) toggles_.clear();
}

void Dispatcher::append_observation(Json& result, Json params, Context& operation) {
    const auto observationMode = params.value("observe", "none");
    if (observationMode == "none") return;

    params["includeScreenshot"] = observationMode != "text";
    params["includeText"] = observationMode != "screenshot";
    if (params.value("headless", false)) params.erase("activate");
    try {
        result["observation"] = backend_.execute("get-app-state", params, operation);
    } catch (const std::exception& error) {
        // A capture failure must not turn delivered input into a retryable failure.
        result["observationError"] = {{"message", error.what()}};
    }
}

Json Dispatcher::start_session(const Json& params) {
    // Caller owns desktopMutex_ exclusively.
    const bool multiAgent = params.value("multiAgent", false);
    const bool headless = params.value("headless", false);
    std::string agentId = string_param(params, "agentId");
    bool joining = false;
    {
        std::lock_guard lock(stateMutex_);
        joining = !sessions_.empty();
        if (joining) {
            const bool everyoneShares = std::all_of(sessions_.begin(), sessions_.end(),
                                                    [](const auto& entry) { return entry.second.multiAgent; });
            if (!multiAgent || !everyoneShares) {
                throw Error("session_busy", "An active session already owns this desktop; start every "
                                            "agent's session with multiAgent to share it");
            }
            context_.check();
            if (sessions_.size() >= maximumAgentSessions) {
                throw Error("session_busy", "This desktop already has " + std::to_string(maximumAgentSessions) +
                                                " agent sessions");
            }
        }
        const auto inUse = [&](const std::string& candidate) {
            return std::any_of(sessions_.begin(), sessions_.end(),
                               [&](const auto& entry) { return entry.second.agentId == candidate; });
        };
        if (!agentId.empty() && inUse(agentId)) {
            throw Error("agent_id_in_use", "Agent " + agentId + " already has a session on this desktop; "
                                           "pick another agentId");
        }
        while (agentId.empty() || inUse(agentId)) agentId = "agent-" + std::to_string(nextAgentNumber_++);
        if (!joining) {
            inputLease_.acquire();
            context_.cancelled = false;
            toggles_.clear();
            toggleOwner_.clear();
            desktopSessionId_ = create_session_id();
        }
    }

    Json result{{"ready", true}};
    if (!joining) {
        try {
            result = backend_.execute("session.start", {{"sessionId", desktopSessionId_}}, context_);
            if (!result.is_object() || !result.value("ready", false)) {
                throw Error("setup_required", "Provider did not confirm a ready usage indicator");
            }
            context_.check();
        } catch (...) {
            stop();
            throw;
        }
    }

    const auto sessionId = create_session_id();
    bool cancelled = false;
    {
        std::lock_guard lock(stateMutex_);
        cancelled = context_.cancelled;
        if (!cancelled) {
            sessions_[sessionId] = AgentSession{agentId, multiAgent, headless, false,
                                                std::chrono::steady_clock::now()};
            result["agentCount"] = sessions_.size();
        }
    }
    if (cancelled) {
        // Without a session nothing else would end the desktop session just started.
        if (!joining) stop();
        throw Error("cancelled", "Session stopped");
    }
    result["sessionId"] = sessionId;
    result["agentId"] = agentId;
    result["multiAgent"] = multiAgent;
    result["headless"] = headless;
    result["joined"] = joining;
    return result;
}

Json Dispatcher::execute_operation(const std::string& method, Json params) {
    const bool startsSession = method == "session.start";
    const bool needsSession = !startsSession && method != "doctor" && method != "capabilities";
    const std::string sessionId = needsSession ? string_param(params, "sessionId") : std::string{};

    // An agent runs one operation at a time; a second one would race its own
    // observation. Other agents are not affected by this flag.
    if (needsSession) {
        std::lock_guard lock(stateMutex_);
        const auto found = sessions_.find(sessionId);
        if (found == sessions_.end()) throw Error("session_required", "Start a session and provide its sessionId");
        if (found->second.busy) throw Error("busy", "Another operation is in progress; observe before retrying an input");
        found->second.busy = true;
        found->second.lastActivity = std::chrono::steady_clock::now();
    }
    struct BusyReset {
        Dispatcher& owner;
        const std::string& sessionId;
        ~BusyReset() {
            if (sessionId.empty()) return;
            std::lock_guard lock(owner.stateMutex_);
            const auto found = owner.sessions_.find(sessionId);
            if (found == owner.sessions_.end()) return;
            found->second.busy = false;
            found->second.lastActivity = std::chrono::steady_clock::now();
        }
    } busyReset{*this, sessionId};

    settle_pending_cleanup();
    std::shared_lock sharedDesktop(desktopMutex_, std::defer_lock);
    std::unique_lock exclusiveDesktop(desktopMutex_, std::defer_lock);
    const bool desktopLocked = startsSession ? exclusiveDesktop.try_lock_for(queueTimeout_)
                                             : sharedDesktop.try_lock_for(queueTimeout_);
    if (!desktopLocked) throw Error("busy", "The desktop session is starting or stopping; retry shortly");
    if (startsSession) {
        finish_pending_stop();
        release_orphaned_toggles();
    }

    // Registering in the same critical section that validates the session
    // means a concurrent stop either fails this check or cancels the operation.
    Context operation;
    {
        std::lock_guard lock(stateMutex_);
        if (needsSession) {
            const auto found = sessions_.find(sessionId);
            if (found == sessions_.end()) throw Error("session_required", "Start a session and provide its sessionId");
            context_.check();
            const auto requested = params.find("headless");
            params["headless"] = requested != params.end() ? requested->get<bool>() : found->second.headless;
        }
        operations_[&operation] = RunningOperation{sessionId, 0};
    }
    struct OperationReset {
        Dispatcher& owner;
        Context* operation;
        ~OperationReset() {
            std::lock_guard lock(owner.stateMutex_);
            owner.operations_.erase(operation);
        }
    } operationReset{*this, &operation};

    if (needsSession && is_input_action(method)) validate_headless_input(method, params);

    std::unique_lock physicalInput(physicalInputMutex_, std::defer_lock);
    if (needs_physical_input(method, params) && !physicalInput.try_lock_for(queueTimeout_)) {
        throw Error("busy", "Another agent's physical input is still running; retry, or use a headless action");
    }
    std::unique_lock serializedBackend(backendMutex_, std::defer_lock);
    if (!backend_.supports_concurrent_execute() && !serializedBackend.try_lock_for(queueTimeout_)) {
        throw Error("busy", "Another agent's operation is still running; this platform runs operations one at a time");
    }
    operation.check();
    if (needsSession) {
        context_.check();
        enforce_toggle_owner(method, params, sessionId);
        params["agentSessionId"] = sessionId;
        params["sessionId"] = desktopSessionId_;
    }
    {
        std::lock_guard lock(stateMutex_);
        operations_[&operation].startedMilliseconds = monotonic_milliseconds();
    }

    if (startsSession) return start_session(params);
    Json result = method.starts_with("toggle.") ? execute_toggle(method, params, sessionId, operation)
                                                : backend_.execute(method, params, operation);
    if (is_input_action(method)) {
        operation.check();
        if (result.is_object() && !result.contains("delivery")) result["delivery"] = "physical";
        append_observation(result, params, operation);
    }
    if (needsSession && result.is_object()) result["sessionId"] = sessionId;
    return result;
}

Json Dispatcher::execute_toggle(const std::string& method, const Json& params, const std::string& sessionId,
                                Context& operation) {
    // Caller owns physicalInputMutex_ and has verified the active session.
    if (method == "toggle.release-all") {
        backend_.release_toggles();
        std::lock_guard lock(stateMutex_);
        Json released(toggles_);
        toggles_.clear();
        toggleOwner_.clear();
        return {{"released", std::move(released)}};
    }
    const auto key = canonical_toggle_key(params["key"].get<std::string>());
    const bool on = params["on"].get<bool>();
    bool held = false;
    {
        std::lock_guard lock(stateMutex_);
        held = toggles_.contains(key);
    }
    if (held == on) return {{"key", key}, {"on", on}, {"changed", false}};

    backend_.set_toggle(key, on, operation);
    bool kept = true;
    {
        std::lock_guard lock(stateMutex_);
        if (!on) {
            toggles_.erase(key);
            if (toggles_.empty()) toggleOwner_.clear();
        } else if (sessions_.contains(sessionId) && !context_.cancelled) {
            toggles_.insert(key);
            toggleOwner_ = sessionId;
        } else {
            kept = false;
        }
    }
    if (!kept) {
        // A desktop stop owns the key through its release paths; a stop of
        // this agent alone leaves nobody else to release it.
        if (!context_.cancelled) {
            Context release;
            try { backend_.set_toggle(key, false, release); } catch (...) { }
        }
        throw Error("cancelled", "Session stopped");
    }
    return {{"key", key}, {"on", on}, {"changed", true}};
}

void Dispatcher::enforce_toggle_owner(const std::string& method, const Json& params, const std::string& sessionId) {
    std::lock_guard lock(stateMutex_);
    if (toggles_.empty() || toggleOwner_ == sessionId) return;
    const auto owner = sessions_.find(toggleOwner_);
    const std::string holder = owner != sessions_.end() ? "Agent " + owner->second.agentId
                                                        : "An ended agent session";
    if (method.starts_with("toggle.") && method != "toggle.status") {
        throw Error("toggles_active", holder + " holds toggle " + join_toggles(toggles_) +
                                          "; only that agent can change toggles");
    }
    if (needs_physical_input(method, params)) {
        throw Error("toggles_active", holder + " holds toggle " + join_toggles(toggles_) +
                                          ", which would join this physical input; retry with headless "
                                          "true or after that agent releases them");
    }
}

Json Dispatcher::session_status(const std::string& requester) {
    std::lock_guard lock(stateMutex_);
    const auto now = std::chrono::steady_clock::now();
    Json agents = Json::array();
    for (const auto& [id, session] : sessions_) {
        agents.push_back({{"agentId", session.agentId},
                          {"multiAgent", session.multiAgent},
                          {"headless", session.headless},
                          {"busy", session.busy},
                          {"idleMs", std::chrono::duration_cast<std::chrono::milliseconds>(
                                         now - session.lastActivity).count()},
                          {"current", id == requester}});
    }
    // A session id is a credential, so only its owner or a lone single-agent
    // session (whose id the CLI saved anyway) sees it.
    std::string visible;
    if (sessions_.contains(requester)) visible = requester;
    else if (sessions_.size() == 1 && !sessions_.begin()->second.multiAgent) visible = sessions_.begin()->first;
    return {{"active", !sessions_.empty()},
            {"stopping", stopping_},
            {"sessionId", visible.empty() ? Json(nullptr) : Json(visible)},
            {"idleTimeoutMs", idleTimeout_.count()},
            {"agentCount", sessions_.size()},
            {"agents", std::move(agents)}};
}

Json Dispatcher::toggle_details(const std::string& requester) {
    std::lock_guard lock(stateMutex_);
    Json details{{"toggles", Json(toggles_)}};
    if (toggles_.empty()) return details;
    const auto owner = sessions_.find(toggleOwner_);
    const bool heldByAnotherAgent = owner != sessions_.end() && requester != toggleOwner_ &&
                                    sessions_.contains(requester);
    details["notice"] = heldByAnotherAgent
        ? "Agent " + owner->second.agentId + " holds toggle " + join_toggles(toggles_) +
              "; your physical input is refused until it releases them, headless actions still work."
        : "Toggle still on: " + join_toggles(toggles_) + ". Release with `dcu toggle off --all` before ending.";
    return details;
}

Json Dispatcher::handle(const Json& request) {
    const Json requestId = request.is_object() ? request.value("id", Json(nullptr)) : Json(nullptr);
    bool authenticated = false;
    std::string requester;
    try {
        authenticate_request(request, requestId, token_);
        authenticated = true;
        if (!request.contains("method") || !request["method"].is_string()) {
            throw Error("invalid_argument", "method must be a string");
        }
        const auto method = request["method"].get<std::string>();
        const Json params = request.value("params", Json::object());
        if (params.is_object()) requester = string_param(params, "sessionId");
        validate_params(method, params);

        Json result;
        if (method == "session.stop" || method == "daemon.shutdown") {
            std::optional<Json> agentStop;
            if (method == "session.stop" && !requester.empty()) agentStop = stop_agent_session(requester);
            if (agentStop) {
                result = std::move(*agentStop);
            } else {
                {
                    // A cancelled session (Esc, lost indicator) is already ending
                    // and releasing its toggles; let the stop through.
                    std::lock_guard lock(stateMutex_);
                    if (!toggles_.empty() && !context_.cancelled) {
                        throw Error("toggles_active", "Toggle still on: " + join_toggles(toggles_) +
                                                          ". Run `dcu toggle off --all` before stopping the session.");
                    }
                }
                result = stop();
                if (method == "daemon.shutdown") quitting = true;
            }
        } else if (method == "session.status") {
            result = session_status(requester);
        } else if (method == "toggle.status") {
            std::lock_guard lock(stateMutex_);
            result = {{"active", !sessions_.empty()}};
        } else {
            result = execute_operation(method, params);
        }
        if (result.is_object()) {
            auto details = toggle_details(requester);
            result["toggles"] = std::move(details["toggles"]);
            if (details.contains("notice")) {
                const auto notice = details["notice"].get<std::string>();
                const auto existing = result.find("notice");
                result["notice"] = existing != result.end() && existing->is_string()
                    ? existing->get<std::string>() + " " + notice : notice;
            }
        }
        return {{"id", requestId}, {"ok", true}, {"result", result}};
    } catch (const Error& error) {
        return error_response(requestId, error.code, error.what(), error_details(authenticated, requester));
    } catch (const std::exception& error) {
        return error_response(requestId, "provider_error", error.what(), error_details(authenticated, requester));
    }
}

Json Dispatcher::error_details(bool authenticated, const std::string& requester) {
    // Unauthenticated callers learn nothing about the desktop state.
    if (!authenticated) return Json();
    auto details = toggle_details(requester);
    return details.contains("notice") ? details : Json();
}

void Dispatcher::tick() {
    bool stopAll = false;
    {
        std::lock_guard lock(stateMutex_);
        const auto now = std::chrono::steady_clock::now();
        std::vector<std::string> expired;
        for (const auto& [id, session] : sessions_) {
            if (!session.busy && now - session.lastActivity > idleTimeout_) expired.push_back(id);
        }
        stopAll = !sessions_.empty() && (context_.cancelled || expired.size() == sessions_.size());
        if (!stopAll) {
            for (const auto& id : expired) {
                sessions_.erase(id);
                if (id != toggleOwner_) continue;
                toggleOwner_.clear();
                if (!toggles_.empty()) orphanedTogglesPending_ = true;
            }
        }
        const auto nowMilliseconds = monotonic_milliseconds();
        const bool operationExpired = std::any_of(operations_.begin(), operations_.end(), [&](const auto& entry) {
            const auto started = entry.second.startedMilliseconds;
            return started != 0 && nowMilliseconds - started > operationTimeoutMilliseconds;
        });
        if (operationExpired && !context_.cancelled) stopAll = true;
    }
    if (stopAll) stop();

    if (!stopRequested_ && !orphanedTogglesPending_) return;
    std::unique_lock desktop(desktopMutex_, std::try_to_lock);
    if (!desktop.owns_lock()) return;
    finish_pending_stop();
    release_orphaned_toggles();
}

void Dispatcher::shutdown() {
    stop();
    tick();
}
} // namespace dcu
