#include "dispatch.hpp"
#include <iomanip>
#include <set>
#include <random>
#include <sstream>

namespace dcu {
namespace {
constexpr std::int64_t operationTimeoutMilliseconds = 120000;

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
} // namespace

Json Dispatcher::stop() {
    context_.cancelled = true;
    stopRequested_ = true;
    {
        std::lock_guard lock(stateMutex_);
        stopping_ = active_;
        active_ = false;
        // Every stop path (user Esc, idle expiry, operation timeout, client
        // disconnect, shutdown) ends the toggles; interrupt() and the backend's
        // session.stop release the physical keys.
        toggles_.clear();
    }
    backend_.interrupt();
    return {{"stopped", true}};
}

void Dispatcher::finish_pending_stop() {
    // Caller owns operationMutex_. Urgent stop never waits for this cleanup,
    // and the desktop lease remains held until backend cleanup completes.
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
}

void Dispatcher::append_observation(Json& result, Json params) {
    const auto observationMode = params.value("observe", "none");
    if (observationMode == "none") return;

    params["includeScreenshot"] = observationMode != "text";
    params["includeText"] = observationMode != "screenshot";
    try {
        result["observation"] = backend_.execute("get-app-state", params, context_);
    } catch (const std::exception& error) {
        // A capture failure must not turn delivered input into a retryable failure.
        result["observationError"] = {{"message", error.what()}};
    }
}

Json Dispatcher::execute_serialized(const std::string& method, Json params) {
    std::unique_lock operationLock(operationMutex_, std::try_to_lock);
    if (!operationLock.owns_lock()) {
        throw Error("busy", "Another operation is in progress; observe before retrying an input");
    }
    finish_pending_stop();

    const bool startsSession = method == "session.start";
    if (startsSession) {
        std::lock_guard lock(stateMutex_);
        if (active_) throw Error("session_busy", "An active session already owns this desktop");
        inputLease_.acquire();
        context_.cancelled = false;
        toggles_.clear();
        params["sessionId"] = create_session_id();
    } else if (method != "doctor" && method != "capabilities") {
        std::lock_guard lock(stateMutex_);
        if (!active_ || params.value("sessionId", "") != sessionId_) {
            throw Error("session_required", "Start a session and provide its sessionId");
        }
        context_.check();
        lastActivity_ = std::chrono::steady_clock::now();
    }

    operationStartedMilliseconds_ = monotonic_milliseconds();
    try {
        Json result = method.starts_with("toggle.") ? execute_toggle(method, params)
                                                    : backend_.execute(method, params, context_);
        if (startsSession) {
            if (!result.is_object() || !result.value("ready", false)) {
                throw Error("setup_required", "Provider did not confirm a ready usage indicator");
            }
            context_.check();
            std::lock_guard lock(stateMutex_);
            sessionId_ = params["sessionId"].get<std::string>();
            active_ = true;
            lastActivity_ = std::chrono::steady_clock::now();
        }
        if (is_input_action(method)) {
            context_.check();
            append_observation(result, params);
        }
        {
            std::lock_guard lock(stateMutex_);
            if (active_) result["sessionId"] = sessionId_;
        }
        operationStartedMilliseconds_ = 0;
        return result;
    } catch (...) {
        operationStartedMilliseconds_ = 0;
        if (startsSession) stop();
        throw;
    }
}

Json Dispatcher::execute_toggle(const std::string& method, const Json& params) {
    // Caller owns operationMutex_ and has verified the active session.
    if (method == "toggle.release-all") {
        backend_.release_toggles();
        std::lock_guard lock(stateMutex_);
        Json released(toggles_);
        toggles_.clear();
        return {{"released", std::move(released)}};
    }
    const auto key = canonical_toggle_key(params["key"].get<std::string>());
    const bool on = params["on"].get<bool>();
    bool held = false;
    {
        std::lock_guard lock(stateMutex_);
        held = toggles_.contains(key);
    }
    if (held != on) {
        backend_.set_toggle(key, on, context_);
        std::lock_guard lock(stateMutex_);
        if (!on) {
            toggles_.erase(key);
        } else if (active_ && !context_.cancelled) {
            toggles_.insert(key);
        } else {
            // A stop raced the press; its release paths own the key now.
            throw Error("cancelled", "Session stopped");
        }
    }
    return {{"key", key}, {"on", on}, {"changed", held != on}};
}

Json Dispatcher::toggle_details() {
    std::lock_guard lock(stateMutex_);
    Json details{{"toggles", Json(toggles_)}};
    if (!toggles_.empty()) {
        details["notice"] = "Toggle still on: " + join_toggles(toggles_) +
                            ". Release with `dcu toggle off --all` before ending.";
    }
    return details;
}

Json Dispatcher::handle(const Json& request) {
    const Json requestId = request.is_object() ? request.value("id", Json(nullptr)) : Json(nullptr);
    bool authenticated = false;
    try {
        authenticate_request(request, requestId, token_);
        authenticated = true;
        if (!request.contains("method") || !request["method"].is_string()) {
            throw Error("invalid_argument", "method must be a string");
        }
        const auto method = request["method"].get<std::string>();
        const Json params = request.value("params", Json::object());
        validate_params(method, params);

        Json result;
        if (method == "session.stop" || method == "daemon.shutdown") {
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
        } else if (method == "session.status") {
            std::lock_guard lock(stateMutex_);
            result = {
                {"active", active_}, {"stopping", stopping_},
                {"sessionId", active_ ? Json(sessionId_) : Json(nullptr)},
                {"idleTimeoutMs", idleTimeout_.count()}
            };
        } else if (method == "toggle.status") {
            std::lock_guard lock(stateMutex_);
            result = {{"active", active_}};
        } else {
            result = execute_serialized(method, params);
        }
        if (result.is_object()) {
            auto details = toggle_details();
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
        return error_response(requestId, error.code, error.what(), error_details(authenticated));
    } catch (const std::exception& error) {
        return error_response(requestId, "provider_error", error.what(), error_details(authenticated));
    }
}

Json Dispatcher::error_details(bool authenticated) {
    // Unauthenticated callers learn nothing about the desktop state.
    if (!authenticated) return Json();
    auto details = toggle_details();
    return details.contains("notice") ? details : Json();
}

void Dispatcher::tick() {
    bool sessionExpired = false;
    {
        std::lock_guard lock(stateMutex_);
        const bool idleExpired = std::chrono::steady_clock::now() - lastActivity_ > idleTimeout_;
        sessionExpired = active_ && (context_.cancelled || idleExpired);
    }
    const auto operationStarted = operationStartedMilliseconds_.load();
    const bool operationExpired = operationStarted != 0 &&
        monotonic_milliseconds() - operationStarted > operationTimeoutMilliseconds;
    if (sessionExpired || (operationExpired && !context_.cancelled)) stop();

    if (!stopRequested_) return;
    std::unique_lock operationLock(operationMutex_, std::try_to_lock);
    if (operationLock.owns_lock()) finish_pending_stop();
}

void Dispatcher::shutdown() {
    stop();
    tick();
}
} // namespace dcu