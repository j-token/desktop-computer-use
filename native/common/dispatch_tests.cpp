#include "dispatch.hpp"
#include <iostream>
#include <thread>
#include <condition_variable>
#include <set>

using namespace dcu;
struct FakeBackend : Backend {
    int captures = 0;
    int actions = 0;
    std::atomic_bool entered{false};
    std::atomic_bool block{false};
    bool failObservation = false;
    bool failStart = false;
    std::set<std::string> held;
    int toggleCalls = 0;
    int releaseToggleCalls = 0;
    Context* sessionContext = nullptr;
    Json execute(const std::string& method, const Json& parameters, Context& context) override {
        if (method == "session.start" && failStart) throw Error("setup_required", "Indicator unavailable");
        if (method == "session.start") { sessionContext = &context; return {{"ready", true}}; }
        if (method == "session.stop") held.clear();
        if (method == "get-full-screenshot") return {{"notice", "Use --coords full."}};
        if (method == "get-app-state") {
            ++captures;
            if (failObservation) throw Error("screenshot_failed", "capture failed");
            return {{"includeText", parameters.value("includeText", false)}};
        }
        if (method == "drag" || method == "click") {
            ++actions;
            entered = true;
            while (block) { context.check(); std::this_thread::sleep_for(std::chrono::milliseconds(1)); }
            return {{"delivered", true}, {"verification", {{"state", "unverified"}}}};
        }
        return Json::object();
    }
    // Real backends release held input, toggles included, on interrupt.
    void interrupt() noexcept override { held.clear(); }
    void set_toggle(const std::string& key, bool down, Context&) override {
        ++toggleCalls;
        if (down) held.insert(key); else held.erase(key);
    }
    void release_toggles() noexcept override { ++releaseToggleCalls; held.clear(); }
};
void require(bool condition, const char* name) { if (!condition) throw std::runtime_error(name); }
int main() {
    try {
        FakeBackend backend;
        Dispatcher dispatcher(backend, std::string(64, 'a'));
        auto sendRequest = [&](std::string method, Json params = Json::object()) { return dispatcher.handle({{"id", "test"}, {"token", std::string(64, 'a')}, {"method", method}, {"params", params}}); };
        require(!dispatcher.handle({{"id", "bad"}, {"token", "wrong"}, {"method", "session.start"}})["ok"], "reject auth");
        require(!sendRequest("click", {{"app", "test"}, {"x", 1}, {"y", 1}})["ok"], "reject no session");
        backend.failStart = true;
        require(!sendRequest("session.start")["ok"], "indicator required");
        require(sendRequest("session.status")["result"]["active"] == false, "failed start inactive");
        backend.failStart = false;
        const auto session = sendRequest("session.start")["result"]["sessionId"];
        require(!sendRequest("session.start")["ok"], "one desktop owner");
        FakeBackend otherBackend;
        Dispatcher other(otherBackend, std::string(64, 'b'));
        const auto competing = other.handle({{"id", "other"}, {"token", std::string(64, 'b')}, {"method", "session.start"}});
        require(competing["error"]["code"] == "session_busy", "cross-daemon desktop ownership");
        Json parameters{{"sessionId", session}, {"app", "test"}, {"x", 1}, {"y", 1}};
        require(sendRequest("click", parameters)["ok"], "click delivered");
        require(backend.captures == 0, "actions skip capture by default");
        parameters["observe"] = "screenshot";
        require(sendRequest("click", parameters)["result"]["observation"]["includeText"] == false, "screenshot skips accessibility");
        backend.failObservation = true;
        const auto result = sendRequest("click", parameters);
        require(result["ok"] && result["result"]["delivered"] && result["result"].contains("observationError"), "capture failure preserves delivery outcome");
        parameters["elementIndex"] = 3;
        require(!sendRequest("click", parameters)["ok"], "element requires observation");
        parameters.erase("elementIndex"); parameters.erase("observe");
        const int actionsBefore = backend.actions;
        parameters["coords"] = "window";
        require(!sendRequest("click", parameters)["ok"] && backend.actions == actionsBefore, "unknown coords rejected before input");
        parameters["coords"] = "full";
        require(sendRequest("click", parameters)["ok"], "full coords accepted");
        parameters.erase("coords");
        backend.failObservation = false;
        const int capturesBefore = backend.captures;
        require(!sendRequest("get-full-screenshot", {{"sessionId", session}, {"app", "test"}})["ok"], "full screenshot requires observation");
        require(sendRequest("get-full-screenshot", {{"sessionId", session}, {"app", "test"}, {"observationId", "o"}, {"observe", "screenshot"}})["ok"], "full screenshot served");
        require(backend.captures == capturesBefore, "full screenshot never recaptures");
        parameters["x"] = -1;
        require(!sendRequest("click", parameters)["ok"], "invalid coordinate rejected before input");
        parameters["x"] = 1;
        backend.entered = false; backend.block = true;
        Json cancelledResponse;
        std::thread worker([&] { cancelledResponse = sendRequest("click", parameters); });
        while (!backend.entered) std::this_thread::sleep_for(std::chrono::milliseconds(1));
        require(sendRequest("click", parameters)["error"]["code"] == "busy", "overlapping input rejected");
        require(sendRequest("session.stop")["ok"], "stop bypasses active operation");
        worker.join();
        require(cancelledResponse["error"]["code"] == "cancelled", "running action cancelled");
        require(!sendRequest("click", parameters)["ok"], "stopped owner cannot input");
        require(sendRequest("doctor")["ok"], "doctor works after cancellation");
        dispatcher.shutdown();

        const std::string token(64, 'c');
        auto sendTo = [&](Dispatcher& target, std::string method, Json params = Json::object()) {
            return target.handle({{"id", "toggle"}, {"token", token}, {"method", method}, {"params", params}});
        };
        const std::string notice = ". Release with `dcu toggle off --all` before ending.";
        {
            FakeBackend toggleBackend;
            Dispatcher toggled(toggleBackend, token);
            auto send = [&](std::string method, Json params = Json::object()) { return sendTo(toggled, method, params); };
            require(send("toggle.set", {{"key", "shift"}, {"on", true}})["error"]["code"] == "session_required", "toggle requires session");
            const auto id = send("session.start")["result"]["sessionId"];
            require(send("session.status")["result"]["toggles"] == Json::array(), "status lists no toggles");
            require(!send("session.status")["result"].contains("notice"), "no notice without toggles");
            require(send("toggle.set", {{"sessionId", id}, {"key", "capslock"}, {"on", true}})["error"]["code"] == "invalid_argument", "unknown toggle key rejected");
            require(send("toggle.set", {{"sessionId", id}, {"key", "shift"}})["error"]["code"] == "invalid_argument", "toggle requires on");
            require(send("drag", {{"sessionId", id}, {"app", "test"}, {"fromX", 1}, {"fromY", 1}, {"toX", 2}, {"toY", 2}, {"modifiers", "shift+space"}})["error"]["code"] == "invalid_argument", "unknown drag modifier rejected");
            require(send("drag", {{"sessionId", id}, {"app", "test"}, {"fromX", 1}, {"fromY", 1}, {"toX", 2}, {"toY", 2}, {"modifiers", "Shift+ctrl"}})["ok"], "drag modifiers accepted");
            const auto on = send("toggle.set", {{"sessionId", id}, {"key", "shift"}, {"on", true}});
            require(on["ok"] && on["result"]["changed"] == true && toggleBackend.held.contains("shift"), "toggle pressed");
            require(on["result"]["toggles"] == Json{"shift"}, "toggle listed");
            require(on["result"]["notice"] == "Toggle still on: shift" + notice, "toggle notice");
            const int callsBefore = toggleBackend.toggleCalls;
            require(send("toggle.set", {{"sessionId", id}, {"key", "SHIFT"}, {"on", true}})["result"]["changed"] == false &&
                    toggleBackend.toggleCalls == callsBefore, "toggle on is idempotent");
            require(send("toggle.set", {{"sessionId", id}, {"key", "super"}, {"on", true}})["result"]["toggles"] == Json({"shift", "win"}), "super aliases win");
            require(send("session.status")["result"]["toggles"] == Json({"shift", "win"}), "status lists toggles");
            require(send("toggle.status")["result"]["notice"] == "Toggle still on: shift, win" + notice, "toggle.status notice");
            require(send("click", {{"sessionId", id}, {"app", "test"}, {"x", 1}, {"y", 1}})["result"]["toggles"] == Json({"shift", "win"}), "action lists toggles");
            const auto full = send("get-full-screenshot", {{"sessionId", id}, {"app", "test"}, {"observationId", "o"}});
            require(full["result"]["notice"] == "Use --coords full. Toggle still on: shift, win" + notice, "toggle notice appends to an existing notice");
            const auto failed = send("click", {{"sessionId", id}, {"app", "test"}});
            require(failed["error"]["code"] == "invalid_argument" && failed["error"]["details"]["toggles"] == Json({"shift", "win"}) &&
                    failed["error"]["details"].contains("notice"), "errors carry toggles");
            const auto unauthorized = toggled.handle({{"id", "bad"}, {"token", "wrong"}, {"method", "session.status"}});
            require(!unauthorized["error"].contains("details"), "unauthorized errors reveal nothing");
            for (const char* method : {"session.stop", "daemon.shutdown"}) {
                const auto refused = send(method);
                require(refused["error"]["code"] == "toggles_active", "stop refused while toggles are on");
                require(refused["error"]["details"]["toggles"] == Json({"shift", "win"}), "refusal lists toggles");
                require(refused["error"]["message"].get<std::string>().find("dcu toggle off --all") != std::string::npos, "refusal names the release command");
            }
            require(!toggled.quitting && send("session.status")["result"]["active"] == true, "refused stop keeps the session");
            require(send("toggle.set", {{"sessionId", id}, {"key", "shift"}, {"on", false}})["result"]["toggles"] == Json{"win"} &&
                    !toggleBackend.held.contains("shift"), "toggle off releases one key");
            const auto released = send("toggle.release-all", {{"sessionId", id}});
            require(released["result"]["released"] == Json{"win"} && released["result"]["toggles"] == Json::array() &&
                    !released["result"].contains("notice") && toggleBackend.held.empty(), "release-all clears toggles");
            require(send("session.stop")["ok"], "stop succeeds after release");
            toggled.shutdown();
        }
        {
            FakeBackend escapeBackend;
            Dispatcher escaped(escapeBackend, token);
            const auto id = sendTo(escaped, "session.start")["result"]["sessionId"];
            require(sendTo(escaped, "toggle.set", {{"sessionId", id}, {"key", "space"}, {"on", true}})["ok"], "space toggled");
            escapeBackend.sessionContext->cancelled = true; // The backend's double-Esc stop.
            require(sendTo(escaped, "session.stop")["ok"], "stop allowed once the session is cancelled");
            escaped.tick();
            const auto status = sendTo(escaped, "session.status")["result"];
            require(status["active"] == false && status["toggles"] == Json::array(), "Esc stop clears toggles");
            require(escapeBackend.held.empty() && escapeBackend.releaseToggleCalls > 0, "Esc stop releases toggled keys");
            escaped.shutdown();
        }
        {
            FakeBackend idleBackend;
            Dispatcher idle(idleBackend, token, std::chrono::milliseconds(50));
            const auto id = sendTo(idle, "session.start")["result"]["sessionId"];
            require(sendTo(idle, "toggle.set", {{"sessionId", id}, {"key", "ctrl"}, {"on", true}})["ok"], "ctrl toggled");
            std::this_thread::sleep_for(std::chrono::milliseconds(120));
            idle.tick();
            const auto status = sendTo(idle, "session.status")["result"];
            require(status["active"] == false && status["toggles"] == Json::array(), "idle expiry clears toggles");
            require(idleBackend.held.empty() && idleBackend.releaseToggleCalls > 0, "idle expiry releases toggled keys");
            idle.shutdown();
        }
        std::cout << "Contract tests passed\n";
        return 0;
    } catch (const std::exception& e) { std::cerr << e.what() << '\n'; return 1; }
}
