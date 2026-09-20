#pragma once

#include "dcu/backend.hpp"

#include <string>
#include <vector>

#include <windows.h>
#include <wrl/client.h>
#include <UIAutomationClient.h>

namespace dcu::windows {

struct ElementRecord {
    Json description;
    RECT bounds{};
    Microsoft::WRL::ComPtr<IUIAutomationElement> element;
};

struct AccessibilitySnapshot {
    Json value;
    std::vector<ElementRecord> elements;
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
};

} // namespace dcu::windows
