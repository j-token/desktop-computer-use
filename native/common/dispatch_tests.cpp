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
    std::string lastMethod;
    Json lastParams;
    Json execute(const std::string& method, const Json& parameters, Context& context) override {
        lastMethod = method;
        lastParams = parameters;
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
// Holds get-app-state or click open until released, so a test can tell whether
// another agent's operation ran meanwhile.
struct ConcurrentBackend : Backend {
    std::atomic_bool holdObservation{false};
    std::atomic_bool holdClick{false};
    std::atomic_int observationsRunning{0};
    std::atomic_int clicksRunning{0};
    bool supports_concurrent_execute() const noexcept override { return true; }
    Json execute(const std::string& method, const Json& parameters, Context& context) override {
        if (method == "session.start") return {{"ready", true}};
        const auto hold = [&](std::atomic_bool& gate, std::atomic_int& running) {
            ++running;
            while (gate) {
                if (context.cancelled) { --running; context.check(); }
                std::this_thread::sleep_for(std::chrono::milliseconds(1));
            }
            --running;
        };
        if (method == "get-app-state") hold(holdObservation, observationsRunning);
        if (method == "click") hold(holdClick, clicksRunning);
        return {{"delivered", true}, {"headless", parameters.value("headless", false)}};
    }
    void interrupt() noexcept override {}
    void set_toggle(const std::string&, bool, Context&) override {}
    void release_toggles() noexcept override {}
};
void require(bool condition, const char* name) { if (!condition) throw std::runtime_error(name); }
void wait_until(const std::function<bool()>& condition, const char* name) {
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
    while (!condition()) {
        if (std::chrono::steady_clock::now() > deadline) throw std::runtime_error(name);
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
}
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
        {
            FakeBackend shared;
            Dispatcher desktop(shared, token, std::chrono::seconds(120), std::chrono::milliseconds(200));
            auto send = [&](std::string method, Json params = Json::object()) { return sendTo(desktop, method, params); };
            const auto alphaStart = send("session.start", {{"multiAgent", true}, {"agentId", "alpha"}});
            require(alphaStart["ok"] && alphaStart["result"]["agentId"] == "alpha" && alphaStart["result"]["joined"] == false,
                    "first multi-agent session starts the desktop");
            const auto alpha = alphaStart["result"]["sessionId"].get<std::string>();
            require(send("session.start")["error"]["code"] == "session_busy", "single-agent start cannot join a shared desktop");
            require(send("session.start", {{"multiAgent", true}, {"agentId", "alpha"}})["error"]["code"] == "agent_id_in_use",
                    "agent ids are unique");
            require(send("session.start", {{"multiAgent", true}, {"agentId", "a b"}})["error"]["code"] == "invalid_argument",
                    "agent id must be printable");
            const auto betaStart = send("session.start", {{"multiAgent", true}, {"headless", true}});
            const auto beta = betaStart["result"]["sessionId"].get<std::string>();
            require(betaStart["result"]["joined"] == true && betaStart["result"]["agentCount"] == 2 &&
                    betaStart["result"]["agentId"] == "agent-1" && betaStart["result"]["headless"] == true,
                    "second agent joins with a generated id");
            require(beta != alpha, "each agent gets its own session id");

            const auto status = send("session.status", {{"sessionId", alpha}})["result"];
            require(status["sessionId"] == alpha && status["agentCount"] == 2, "status shows the caller's session");
            require(send("session.status")["result"]["sessionId"].is_null(), "status hides shared session ids");

            require(send("click", {{"sessionId", beta}, {"app", "test"}, {"observationId", "o"}, {"elementIndex", 2}})["ok"],
                    "headless element click accepted");
            require(shared.lastParams["headless"] == true && shared.lastParams["agentSessionId"] == beta &&
                    shared.lastParams["sessionId"] != beta, "backend sees the headless flag and the owning agent");
            require(send("type-text", {{"sessionId", beta}, {"app", "test"}, {"text", "x"}})["error"]["code"] == "headless_unavailable",
                    "session default headless refuses typing");
            require(send("click", {{"sessionId", beta}, {"app", "test"}, {"x", 1}, {"y", 1}})["error"]["code"] == "headless_unavailable",
                    "headless refuses a coordinate click");
            require(send("click", {{"sessionId", beta}, {"app", "test"}, {"observationId", "o"}, {"elementIndex", 2}, {"button", "right"}})["error"]["code"] == "headless_unavailable",
                    "headless refuses a right click");
            const int actionsBefore = shared.actions;
            require(send("type-text", {{"sessionId", beta}, {"app", "test"}, {"text", "x"}, {"headless", false}})["ok"],
                    "request headless false overrides the session default");
            const auto physical = send("click", {{"sessionId", alpha}, {"app", "test"}, {"x", 1}, {"y", 1}});
            require(physical["result"]["delivery"] == "physical" && physical["result"]["sessionId"] == alpha &&
                    shared.actions == actionsBefore + 1 && shared.lastParams["headless"] == false,
                    "physical input reports its delivery");

            require(send("toggle.set", {{"sessionId", alpha}, {"key", "shift"}, {"on", true}})["ok"], "owner holds shift");
            const auto refused = send("click", {{"sessionId", beta}, {"app", "test"}, {"x", 1}, {"y", 1}, {"headless", false}});
            require(refused["error"]["code"] == "toggles_active" &&
                    refused["error"]["details"]["notice"].get<std::string>().find("Agent alpha") != std::string::npos,
                    "another agent's toggle blocks physical input");
            require(send("click", {{"sessionId", beta}, {"app", "test"}, {"observationId", "o"}, {"elementIndex", 2}})["ok"],
                    "headless input ignores another agent's toggles");
            require(send("set-state", {{"sessionId", beta}, {"app", "test"}, {"observationId", "o"}, {"elementIndex", 2},
                                       {"state", "checked"}, {"headless", false}})["ok"] &&
                        shared.lastMethod == "set-state",
                    "set-state ignores another agent's toggles even when not headless");
            require(send("set-state", {{"sessionId", beta}, {"app", "test"}, {"observationId", "o"}, {"elementIndex", 2},
                                       {"state", "pressed"}})["error"]["code"] == "invalid_argument",
                    "set-state refuses an unknown state");
            require(send("set-state", {{"sessionId", beta}, {"app", "test"}, {"observationId", "o"}, {"elementIndex", 2}})
                        ["error"]["code"] == "invalid_argument",
                    "set-state requires a state");
            require(send("scroll-to", {{"sessionId", beta}, {"app", "test"}})["error"]["code"] == "invalid_argument",
                    "scroll-to requires an element");
            require(send("toggle.release-all", {{"sessionId", beta}})["error"]["code"] == "toggles_active",
                    "only the owner changes toggles");
            require(send("session.stop", {{"sessionId", alpha}})["error"]["code"] == "toggles_active",
                    "the toggle owner cannot leave with keys held");
            require(send("toggle.release-all", {{"sessionId", alpha}})["ok"], "owner releases toggles");

            const auto betaStop = send("session.stop", {{"sessionId", beta}});
            require(betaStop["ok"] && betaStop["result"]["agentCount"] == 1, "stopping one agent keeps the desktop");
            require(send("session.status")["result"]["active"] == true, "desktop survives one agent leaving");
            require(send("click", {{"sessionId", beta}, {"app", "test"}, {"x", 1}, {"y", 1}})["error"]["code"] == "session_required",
                    "a stopped agent cannot input");
            require(send("session.stop", {{"sessionId", "unknown"}})["error"]["code"] == "session_required",
                    "an unknown id cannot stop other agents");
            require(send("session.stop", {{"sessionId", alpha}})["ok"] && send("session.status")["result"]["active"] == false,
                    "the last agent stops the desktop");
            desktop.shutdown();
        }
        {
            FakeBackend serialized;
            Dispatcher desktop(serialized, token, std::chrono::seconds(120), std::chrono::milliseconds(100));
            auto send = [&](std::string method, Json params = Json::object()) { return sendTo(desktop, method, params); };
            const auto alpha = send("session.start", {{"multiAgent", true}})["result"]["sessionId"];
            const auto beta = send("session.start", {{"multiAgent", true}})["result"]["sessionId"];
            serialized.entered = false; serialized.block = true;
            Json blocked;
            std::thread worker([&] { blocked = send("click", {{"sessionId", alpha}, {"app", "test"}, {"x", 1}, {"y", 1}}); });
            wait_until([&] { return serialized.entered.load(); }, "alpha click entered");
            require(send("get-app-state", {{"sessionId", beta}, {"app", "test"}})["error"]["code"] == "busy",
                    "a serial backend makes other agents wait, then gives up");
            std::thread release([&] { std::this_thread::sleep_for(std::chrono::milliseconds(30)); serialized.block = false; });
            require(send("get-app-state", {{"sessionId", beta}, {"app", "test"}})["ok"], "a waiting agent runs once its turn comes");
            release.join(); worker.join();
            require(blocked["ok"], "the first agent's click completed");
            desktop.shutdown();
        }
        {
            ConcurrentBackend parallel;
            Dispatcher desktop(parallel, token, std::chrono::seconds(120), std::chrono::milliseconds(100));
            auto send = [&](std::string method, Json params = Json::object()) { return sendTo(desktop, method, params); };
            const auto alpha = send("session.start", {{"multiAgent", true}})["result"]["sessionId"];
            const auto beta = send("session.start", {{"multiAgent", true}})["result"]["sessionId"];
            const auto gamma = send("session.start", {{"multiAgent", true}})["result"]["sessionId"];

            parallel.holdObservation = true;
            Json slowObservation;
            std::thread first([&] { slowObservation = send("get-app-state", {{"sessionId", alpha}, {"app", "test"}}); });
            Json secondObservation;
            std::thread second([&] { secondObservation = send("get-app-state", {{"sessionId", beta}, {"app", "test"}}); });
            wait_until([&] { return parallel.observationsRunning == 2; }, "two agents observe at the same time");
            parallel.holdObservation = false;
            first.join(); second.join();
            require(slowObservation["ok"] && secondObservation["ok"], "concurrent observations both succeed");

            parallel.holdClick = true;
            Json physicalClick;
            std::thread clicker([&] { physicalClick = send("click", {{"sessionId", alpha}, {"app", "test"}, {"x", 1}, {"y", 1}}); });
            wait_until([&] { return parallel.clicksRunning == 1; }, "alpha physical click running");
            require(send("get-app-state", {{"sessionId", beta}, {"app", "test"}})["ok"], "observation runs during physical input");
            require(send("click", {{"sessionId", beta}, {"app", "test"}, {"x", 1}, {"y", 1}})["error"]["code"] == "busy",
                    "physical input takes turns");
            require(send("set-state", {{"sessionId", beta}, {"app", "test"}, {"observationId", "o"},
                                       {"elementIndex", 1}, {"state", "expanded"}})["ok"],
                    "set-state runs during another agent's physical input without headless");
            require(send("scroll-to", {{"sessionId", beta}, {"app", "test"}, {"observationId", "o"},
                                       {"elementIndex", 1}})["ok"],
                    "scroll-to runs during another agent's physical input without headless");
            require(send("select-option", {{"sessionId", beta}, {"app", "test"}, {"observationId", "o"},
                                           {"elementIndex", 1}, {"option", "Medium"}})["ok"],
                    "select-option runs during another agent's physical input without headless");
            require(send("select-option", {{"sessionId", beta}, {"app", "test"}, {"observationId", "o"},
                                           {"elementIndex", 1}, {"option", ""}})["error"]["code"] == "invalid_argument",
                    "select-option refuses an empty option");
            Json headlessClick;
            std::thread headless([&] {
                headlessClick = send("click", {{"sessionId", gamma}, {"app", "test"}, {"observationId", "o"},
                                               {"elementIndex", 1}, {"headless", true}});
            });
            wait_until([&] { return parallel.clicksRunning == 2; }, "headless click runs during physical input");
            const auto gammaStop = send("session.stop", {{"sessionId", gamma}});
            headless.join();
            require(gammaStop["ok"] && headlessClick["error"]["code"] == "cancelled", "stopping one agent cancels only its operation");
            require(parallel.clicksRunning == 1, "the other agent's click keeps running");
            parallel.holdClick = false;
            clicker.join();
            require(physicalClick["ok"], "physical click completes after another agent stopped");
            desktop.shutdown();
        }
        {
            FakeBackend idleBackend;
            Dispatcher idle(idleBackend, token, std::chrono::milliseconds(100));
            const auto alpha = sendTo(idle, "session.start", {{"multiAgent", true}})["result"]["sessionId"];
            const auto beta = sendTo(idle, "session.start", {{"multiAgent", true}})["result"]["sessionId"];
            std::this_thread::sleep_for(std::chrono::milliseconds(150));
            require(sendTo(idle, "list-apps", {{"sessionId", beta}})["ok"], "beta stays active");
            idle.tick();
            const auto status = sendTo(idle, "session.status", {{"sessionId", beta}})["result"];
            require(status["active"] == true && status["agentCount"] == 1, "an idle agent expires alone");
            require(sendTo(idle, "list-apps", {{"sessionId", alpha}})["error"]["code"] == "session_required", "the expired agent is gone");
            idle.shutdown();
        }
        std::cout << "Contract tests passed\n";
        return 0;
    } catch (const std::exception& e) { std::cerr << e.what() << '\n'; return 1; }
}
