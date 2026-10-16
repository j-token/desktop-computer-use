#pragma once

#include "dcu/backend.hpp"

#include <chrono>
#include <optional>
#include <string>
#include <vector>

#include <windows.h>
#include <wrl/client.h>
#include <UIAutomationClient.h>

namespace dcu::windows {

struct ElementRecord {
    Json description;
    Microsoft::WRL::ComPtr<IUIAutomationElement> element;
};

struct ScrollPosition {
    double horizontalPercent = 0;
    double verticalPercent = 0;
    bool operator==(const ScrollPosition&) const = default;
};

struct AccessibilitySnapshot {
    Json value;
    std::vector<ElementRecord> elements;
};

// Records when a window's provider reports that it applied a change: layout
// invalidation, structure changes, or a scroll position change. Browsers
// publish moved locations up to about half a second after the input that moved
// them, so these events are what tells a pending update from no change at all.
class UiaChangeWatcher final {
public:
    explicit UiaChangeWatcher(HWND hwnd);
    ~UiaChangeWatcher();
    UiaChangeWatcher(const UiaChangeWatcher&) = delete;
    UiaChangeWatcher& operator=(const UiaChangeWatcher&) = delete;

    bool watching() const { return handler_ != nullptr; }
    std::optional<std::chrono::steady_clock::time_point> last_change() const;

private:
    class Handler;
    Microsoft::WRL::ComPtr<IUIAutomation> automation_;
    Handler* handler_ = nullptr;
};

// UIA is intentionally separate from screenshot capture. A screenshot-only
// observation never instantiates UIAutomation or walks the accessibility tree.
class UiaSnapshotBuilder final {
public:
    AccessibilitySnapshot build(HWND hwnd, const RECT& windowRect,
                                 Context& context) const;

    bool invoke(ElementRecord& element, Context& context) const;
    bool set_value(ElementRecord& element, const std::wstring& value,
                   Context& context) const;

    // Window-local bounds the provider reports now, clipped to every scrollable
    // ancestor, or nullopt once the element is gone. An observation's bounds go
    // stale when the page scrolls or reflows while the window keeps its
    // geometry, and browsers report unclipped bounds for content scrolled
    // under their toolbar, so the result can be empty.
    std::optional<RECT> visible_bounds(IUIAutomationElement* element,
                                       const RECT& windowRect) const;

    // The innermost scrollable container under a screen point, or null. A
    // browser hit-tests to a container that never moves while its content
    // scrolls, so a wheel scroll is observed through this position instead.
    Microsoft::WRL::ComPtr<IUIAutomationScrollPattern> scroll_container_at(POINT screenPoint) const;
    std::optional<ScrollPosition> scroll_position(IUIAutomationScrollPattern* container) const;
};

} // namespace dcu::windows
