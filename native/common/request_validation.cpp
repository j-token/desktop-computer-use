#include "dispatch.hpp"
#include <cctype>
#include <cmath>
#include <set>

namespace dcu {
namespace {
const std::set<std::string> inputActions{
    "click", "drag", "scroll", "type-text", "press-key", "hotkey", "set-value", "paste-text"
};
const std::set<std::string> supportedMethods{
    "doctor", "capabilities", "session.start", "session.status", "session.stop", "daemon.shutdown",
    "list-apps", "list-windows", "get-app-state", "get-full-screenshot", "click", "drag", "scroll", "type-text",
    "press-key", "hotkey", "set-value", "paste-text", "toggle.set", "toggle.release-all", "toggle.status"
};
const std::set<std::string> clickModifiers{
    "shift", "ctrl", "control", "alt", "option", "win", "meta", "super"
};
std::string lowercase(std::string value) {
    for (auto& character : value) character = static_cast<char>(std::tolower(static_cast<unsigned char>(character)));
    return value;
}
void validate_modifiers(const Json& params) {
    if (!params.contains("modifiers")) return;
    if (!params["modifiers"].is_string()) throw Error("invalid_argument", "modifiers must be a string such as shift+ctrl");
    const auto& value = params["modifiers"].get_ref<const std::string&>();
    std::size_t start = 0;
    while (start <= value.size()) {
        const auto end = value.find('+', start);
        const auto part = lowercase(value.substr(start, end == std::string::npos ? std::string::npos : end - start));
        if (!clickModifiers.contains(part)) {
            throw Error("invalid_argument", "Unknown modifier: " + part + " (use shift, ctrl, alt, or win joined by +)");
        }
        if (end == std::string::npos) break;
        start = end + 1;
    }
}
void validate_number(const Json& params, const char* key, double minimum, double maximum,
                     bool requiresInteger = false) {
    if (!params.contains(key)) return;
    if (!params[key].is_number()) throw Error("invalid_argument", std::string(key) + " must be numeric");
    const auto value = params[key].get<double>();
    const bool outsideRange = !std::isfinite(value) || value < minimum || value > maximum;
    const bool invalidInteger = requiresInteger && value != std::floor(value);
    if (outsideRange || invalidInteger) {
        throw Error("invalid_argument", std::string(key) + " is outside its supported range");
    }
}
void validate_choice(const Json& params, const char* key, const std::set<std::string>& choices) {
    if (params.contains(key) && (!params[key].is_string() || !choices.contains(params[key].get<std::string>())))
        throw Error("invalid_argument", std::string("Invalid ") + key);
}
}
void validate_params(const std::string& method, const Json& params) {
    if (!supportedMethods.contains(method)) throw Error("invalid_argument", "Unknown method: " + method);
    if (!params.is_object()) throw Error("invalid_argument", "params must be an object");
    for (auto key : {"x", "y", "fromX", "fromY", "toX", "toY"}) validate_number(params, key, 0, 1000000);
    for (auto key : {"durationMs", "holdBeforeMs", "holdAfterMs"}) validate_number(params, key, 0, 10000, true);
    validate_number(params, "steps", 1, 1000, true);
    validate_number(params, "quality", 1, 100, true);
    validate_number(params, "maxEdge", 0, 16384, true);
    validate_number(params, "amount", 1, 1000, true);
    for (auto key : {"elementIndex", "fromElementIndex", "toElementIndex"}) validate_number(params, key, 0, 1000000, true);
    for (auto key : {"includeText", "includeScreenshot", "restoreWindow", "activate"})
        if (params.contains(key) && !params[key].is_boolean()) throw Error("invalid_argument", std::string(key) + " must be boolean");
    for (auto key : {"app", "windowId", "observationId", "sessionId", "text", "key", "value"})
        if (params.contains(key) && (!params[key].is_string() || params[key].get_ref<const std::string&>().size() > 1048576))
            throw Error("invalid_argument", std::string("Invalid ") + key);
    validate_choice(params, "button", {"left", "right", "middle"});
    validate_choice(params, "format", {"jpeg", "png"});
    validate_choice(params, "observe", {"none", "screenshot", "text", "both"});
    validate_choice(params, "direction", {"up", "down", "left", "right"});
    validate_choice(params, "coords", {"reduced", "full"});
    validate_modifiers(params);
    if (method == "toggle.set") {
        if (!params.contains("key") || canonical_toggle_key(params["key"].get<std::string>()).empty()) {
            throw Error("invalid_argument", "toggle key must be shift, ctrl, alt, win, or space");
        }
        if (!params.contains("on") || !params["on"].is_boolean()) throw Error("invalid_argument", "on must be boolean");
    }
    const bool usesElementIndex = params.contains("elementIndex") ||
        params.contains("fromElementIndex") || params.contains("toElementIndex");
    if (usesElementIndex && !params.contains("observationId")) {
        throw Error("invalid_argument", "Element indexes require observationId");
    }
    if (inputActions.contains(method) || method == "get-app-state" || method == "get-full-screenshot") {
        if (!params.contains("windowId") && !params.contains("app")) throw Error("invalid_argument", "Specify app or windowId");
    }
    if (method == "drag") {
        for (const auto& [xKey, yKey] : {std::pair{"fromX", "fromY"}, std::pair{"toX", "toY"}}) {
            if (params.contains(xKey) != params.contains(yKey)) {
                throw Error("invalid_argument", "Drag coordinates require both x and y");
            }
        }
        const bool hasStart = params.contains("fromX") || params.contains("fromElementIndex");
        const bool hasEnd = params.contains("toX") || params.contains("toElementIndex");
        if (!hasStart || !hasEnd) {
            throw Error("invalid_argument", "Drag requires both endpoints");
        }
    }
    if (method == "get-full-screenshot" && !params.contains("observationId"))
        throw Error("invalid_argument", "get-full-screenshot requires observationId");
    if (method == "click" && !params.contains("elementIndex") && !(params.contains("x") && params.contains("y")))
        throw Error("invalid_argument", "Click requires an element or x/y");
    if ((method == "type-text" || method == "paste-text") && !params.contains("text")) throw Error("invalid_argument", "text is required");
    if ((method == "press-key" || method == "hotkey") && !params.contains("key")) throw Error("invalid_argument", "key is required");
    if (method == "set-value" && (!params.contains("elementIndex") || !params.contains("value"))) throw Error("invalid_argument", "set-value requires elementIndex and value");
    if (method == "scroll" && !params.contains("direction")) throw Error("invalid_argument", "direction is required");
}

bool is_input_action(const std::string& method) {
    return inputActions.contains(method);
}

std::string canonical_toggle_key(const std::string& key) {
    const auto name = lowercase(key);
    if (name == "shift" || name == "ctrl" || name == "alt" || name == "win" || name == "space") return name;
    if (name == "control") return "ctrl";
    if (name == "super" || name == "meta") return "win";
    return {};
}
} // namespace dcu
