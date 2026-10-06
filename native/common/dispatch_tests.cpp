#include "dispatch.hpp"
#include <iostream>
#include <thread>
#include <condition_variable>

using namespace dcu;
struct FakeBackend : Backend {
    int captures = 0;
    int actions = 0;
    std::atomic_bool entered{false};
    std::atomic_bool block{false};
    bool failObservation = false;
    bool failStart = false;
    Json execute(const std::string& method, const Json& parameters, Context& context) override {
        if (method == "session.start" && failStart) throw Error("setup_required", "Indicator unavailable");
        if (method == "session.start") return {{"ready", true}};
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
    void interrupt() noexcept override {}
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
        std::cout << "Contract tests passed\n";
        return 0;
    } catch (const std::exception& e) { std::cerr << e.what() << '\n'; return 1; }
}
