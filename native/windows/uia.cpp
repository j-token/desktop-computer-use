#include "uia.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <algorithm>
#include <atomic>
#include <cmath>
#include <cwctype>
#include <iostream>
#include <sstream>
#include <winrt/base.h>

namespace dcu::windows {
namespace {

using Microsoft::WRL::ComPtr;

std::string utf8(const std::wstring& value) {
    if (value.empty()) return {};
    const int count = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0,
                                          nullptr, nullptr);
    if (count <= 0) return {};
    std::string result(static_cast<std::size_t>(count), '\0');
    WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                        static_cast<int>(value.size()), result.data(), count, nullptr,
                        nullptr);
    return result;
}

std::string bstr_to_utf8(BSTR value) {
    return value ? utf8(std::wstring(value, SysStringLen(value))) : std::string{};
}

std::wstring lower(std::wstring value) {
    std::transform(value.begin(), value.end(), value.begin(),
                   [](wchar_t ch) { return static_cast<wchar_t>(std::towlower(ch)); });
    return value;
}

bool looks_sensitive(const std::wstring& name, const std::wstring& value,
                     bool isPassword) {
    if (isPassword) return true;
    const std::wstring haystack = lower(name + L" " + value);
    for (const auto* token : {L"password", L"passcode", L"secret", L"token"}) {
        if (haystack.find(token) != std::wstring::npos) return true;
    }
    return false;
}

std::string control_type_name(int type) {
    switch (type) {
    case UIA_ButtonControlTypeId: return "Button";
    case UIA_CheckBoxControlTypeId: return "CheckBox";
    case UIA_ComboBoxControlTypeId: return "ComboBox";
    case UIA_EditControlTypeId: return "Edit";
    case UIA_HyperlinkControlTypeId: return "Hyperlink";
    case UIA_ImageControlTypeId: return "Image";
    case UIA_ListControlTypeId: return "List";
    case UIA_ListItemControlTypeId: return "ListItem";
    case UIA_MenuControlTypeId: return "Menu";
    case UIA_MenuItemControlTypeId: return "MenuItem";
    case UIA_PaneControlTypeId: return "Pane";
    case UIA_RadioButtonControlTypeId: return "RadioButton";
    case UIA_ScrollBarControlTypeId: return "ScrollBar";
    case UIA_SliderControlTypeId: return "Slider";
    case UIA_TabControlTypeId: return "Tab";
    case UIA_TabItemControlTypeId: return "TabItem";
    case UIA_TextControlTypeId: return "Text";
    case UIA_TreeControlTypeId: return "Tree";
    case UIA_TreeItemControlTypeId: return "TreeItem";
    case UIA_WindowControlTypeId: return "Window";
    case UIA_CalendarControlTypeId: return "Calendar";
    case UIA_MenuBarControlTypeId: return "MenuBar";
    case UIA_ProgressBarControlTypeId: return "ProgressBar";
    case UIA_SpinnerControlTypeId: return "Spinner";
    case UIA_StatusBarControlTypeId: return "StatusBar";
    case UIA_ToolBarControlTypeId: return "ToolBar";
    case UIA_ToolTipControlTypeId: return "ToolTip";
    case UIA_CustomControlTypeId: return "Custom";
    case UIA_GroupControlTypeId: return "Group";
    case UIA_ThumbControlTypeId: return "Thumb";
    case UIA_DataGridControlTypeId: return "DataGrid";
    case UIA_DataItemControlTypeId: return "DataItem";
    case UIA_DocumentControlTypeId: return "Document";
    case UIA_SplitButtonControlTypeId: return "SplitButton";
    case UIA_HeaderControlTypeId: return "Header";
    case UIA_HeaderItemControlTypeId: return "HeaderItem";
    case UIA_TableControlTypeId: return "Table";
    case UIA_TitleBarControlTypeId: return "TitleBar";
    case UIA_SeparatorControlTypeId: return "Separator";
    case UIA_SemanticZoomControlTypeId: return "SemanticZoom";
    case UIA_AppBarControlTypeId: return "AppBar";
    default: return "ControlType_" + std::to_string(type);
    }
}

bool get_string(IUIAutomationElement* element, PROPERTYID property,
                std::wstring& result) {
    VARIANT value;
    VariantInit(&value);
    const HRESULT hr = element->GetCachedPropertyValue(property, &value);
    if (SUCCEEDED(hr) && value.vt == VT_BSTR && value.bstrVal) {
        result.assign(value.bstrVal, SysStringLen(value.bstrVal));
    } else {
        result.clear();
    }
    VariantClear(&value);
    return SUCCEEDED(hr);
}

bool get_bool(IUIAutomationElement* element, PROPERTYID property) {
    VARIANT value;
    VariantInit(&value);
    const HRESULT hr = element->GetCachedPropertyValue(property, &value);
    const bool result = SUCCEEDED(hr) && value.vt == VT_BOOL && value.boolVal == VARIANT_TRUE;
    VariantClear(&value);
    return result;
}

int get_int(IUIAutomationElement* element, PROPERTYID property) {
    VARIANT value;
    VariantInit(&value);
    const HRESULT hr = element->GetCachedPropertyValue(property, &value);
    int result = 0;
    if (SUCCEEDED(hr)) {
        if (value.vt == VT_I4) result = value.lVal;
        if (value.vt == VT_UI4) result = static_cast<int>(value.ulVal);
    }
    VariantClear(&value);
    return result;
}

RECT get_bounds(IUIAutomationElement* element, const RECT& windowRect) {
    RECT result{};
    VARIANT value;
    VariantInit(&value);
    if (SUCCEEDED(element->GetCachedPropertyValue(UIA_BoundingRectanglePropertyId, &value)) &&
        value.vt == (VT_ARRAY | VT_R8) && value.parray) {
        LONG lowerBound = 0;
        LONG upperBound = -1;
        if (SUCCEEDED(SafeArrayGetLBound(value.parray, 1, &lowerBound)) &&
            SUCCEEDED(SafeArrayGetUBound(value.parray, 1, &upperBound)) &&
            upperBound - lowerBound + 1 >= 4) {
            double* p = nullptr;
            if (SUCCEEDED(SafeArrayAccessData(value.parray, reinterpret_cast<void**>(&p)))) {
                result.left = static_cast<LONG>(std::lround(p[0])) - windowRect.left;
                result.top = static_cast<LONG>(std::lround(p[1])) - windowRect.top;
                result.right = static_cast<LONG>(std::lround(p[0] + p[2])) - windowRect.left;
                result.bottom = static_cast<LONG>(std::lround(p[1] + p[3])) - windowRect.top;
                SafeArrayUnaccessData(value.parray);
            }
        }
    }
    VariantClear(&value);
    return result;
}

bool cached_pattern(IUIAutomationElement* element, PATTERNID pattern) {
    // A cached pattern the element does not support still succeeds and yields a
    // null interface, so the pointer decides support, not the return code.
    ComPtr<IUnknown> unknown;
    return SUCCEEDED(element->GetCachedPattern(pattern, &unknown)) && unknown;
}

std::vector<int> get_runtime_id(IUIAutomationElement* element) {
    VARIANT value;
    VariantInit(&value);
    std::vector<int> result;
    if (SUCCEEDED(element->GetCachedPropertyValue(UIA_RuntimeIdPropertyId, &value)) &&
        (value.vt == (VT_ARRAY | VT_I4)) && value.parray) {
        LONG lowerBound = 0;
        LONG upperBound = -1;
        if (SUCCEEDED(SafeArrayGetLBound(value.parray, 1, &lowerBound)) &&
            SUCCEEDED(SafeArrayGetUBound(value.parray, 1, &upperBound)) &&
            upperBound >= lowerBound) {
            result.resize(static_cast<std::size_t>(upperBound - lowerBound + 1));
            LONG index = lowerBound;
            SafeArrayGetElement(value.parray, &index, result.data());
            for (LONG i = lowerBound + 1; i <= upperBound; ++i) {
                SafeArrayGetElement(value.parray, &i,
                                    &result[static_cast<std::size_t>(i - lowerBound)]);
            }
        }
    }
    VariantClear(&value);
    return result;
}

std::string rect_string(const RECT& rect) {
    return std::to_string(rect.left) + "," + std::to_string(rect.top) + " " +
           std::to_string(std::max<LONG>(0, rect.right - rect.left)) + "x" +
           std::to_string(std::max<LONG>(0, rect.bottom - rect.top));
}

std::string description_field(const Json& description, const char* key) {
    if (!description.is_object()) return {};
    const auto found = description.find(key);
    return found != description.end() && found->is_string() ? found->get<std::string>()
                                                            : std::string{};
}

// One line per caught fault, in the daemon log next to the transport messages,
// so a provider that misbehaves is never silent again.
void report_pattern_fault(const char* call, const Json& description, DWORD code) {
    const auto type = description_field(description, "controlType");
    const auto name = description_field(description, "name");
    std::ostringstream line;
    line << "uia: " << call << " raised structured exception 0x" << std::uppercase << std::hex
         << code << std::dec << std::nouppercase << " on "
         << (type.empty() ? std::string("<unknown control type>") : type);
    if (!name.empty()) line << " \"" << name << "\"";
    line << "; treating the pattern as unusable\n";
    std::cerr << line.str();
}

// A pattern call crosses into the inspected application's UI Automation
// provider, and a fault there arrives as a structured exception, not a C++ one:
// under /EHsc `catch (...)` never sees it, so an unguarded call takes the whole
// daemon down.  MSVC refuses __try in a function that needs C++ object
// unwinding, so each guarded call lives in its own helper that takes raw
// pointers while every ComPtr stays in the caller.  Each helper returns 0, or
// the structured exception code that was caught.
DWORD guarded_pattern(IUIAutomationElement* element, PATTERNID pattern, REFIID iid,
                      void** result, HRESULT* status) noexcept {
    *result = nullptr;
    *status = E_FAIL;
    IUnknown* unknown = nullptr;
    __try {
        *status = element->GetCurrentPattern(pattern, &unknown);
        // A pattern the element does not support still succeeds and yields a
        // null interface, so the pointer decides support, not the return code.
        // Narrowing that null pointer is itself an access violation.
        if (SUCCEEDED(*status) && unknown) {
            *status = unknown->QueryInterface(iid, result);
            unknown->Release();
        } else if (SUCCEEDED(*status)) {
            *status = E_NOINTERFACE;
        }
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        // Nothing the faulted call wrote can be trusted.  Releasing a pointer
        // that may be garbage would be a second, real crash, so the reference
        // is abandoned deliberately.
        *result = nullptr;
        *status = E_FAIL;
        return GetExceptionCode();
    }
    return 0;
}

DWORD guarded_invoke(IUIAutomationInvokePattern* pattern, HRESULT* status) noexcept {
    __try {
        *status = pattern->Invoke();
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        *status = E_FAIL;
        return GetExceptionCode();
    }
    return 0;
}

DWORD guarded_select(IUIAutomationSelectionItemPattern* pattern, HRESULT* status) noexcept {
    __try {
        *status = pattern->Select();
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        *status = E_FAIL;
        return GetExceptionCode();
    }
    return 0;
}

DWORD guarded_toggle(IUIAutomationTogglePattern* pattern, HRESULT* status) noexcept {
    __try {
        *status = pattern->Toggle();
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        *status = E_FAIL;
        return GetExceptionCode();
    }
    return 0;
}

DWORD guarded_set_value(IUIAutomationValuePattern* pattern, BSTR value,
                        HRESULT* status) noexcept {
    __try {
        *status = pattern->SetValue(value);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        *status = E_FAIL;
        return GetExceptionCode();
    }
    return 0;
}

DWORD guarded_bounding_rectangle(IUIAutomationElement* element, RECT* bounds,
                                 HRESULT* status) noexcept {
    __try {
        *status = element->get_CurrentBoundingRectangle(bounds);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        *status = E_FAIL;
        return GetExceptionCode();
    }
    return 0;
}

DWORD guarded_element_from_point(IUIAutomation* automation, POINT point,
                                 IUIAutomationElement** element, HRESULT* status) noexcept {
    *element = nullptr;
    __try {
        *status = automation->ElementFromPoint(point, element);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        *element = nullptr;
        *status = E_FAIL;
        return GetExceptionCode();
    }
    return 0;
}

DWORD guarded_parent(IUIAutomationTreeWalker* walker, IUIAutomationElement* element,
                     IUIAutomationElement** parent, HRESULT* status) noexcept {
    *parent = nullptr;
    __try {
        *status = walker->GetParentElement(element, parent);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        *parent = nullptr;
        *status = E_FAIL;
        return GetExceptionCode();
    }
    return 0;
}

DWORD guarded_scroll_percents(IUIAutomationScrollPattern* pattern, double* horizontal,
                              double* vertical, HRESULT* status) noexcept {
    __try {
        *status = pattern->get_CurrentHorizontalScrollPercent(horizontal);
        if (SUCCEEDED(*status)) *status = pattern->get_CurrentVerticalScrollPercent(vertical);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        *status = E_FAIL;
        return GetExceptionCode();
    }
    return 0;
}

} // namespace

AccessibilitySnapshot UiaSnapshotBuilder::build(HWND hwnd, const RECT& windowRect,
                                                 Context& context) const {
    if (!IsWindow(hwnd)) throw Error("window_not_found", "The target window no longer exists");
    context.check();
    ComPtr<IUIAutomation> automation;
    HRESULT hr = CoCreateInstance(CLSID_CUIAutomation, nullptr, CLSCTX_INPROC_SERVER,
                                  IID_PPV_ARGS(&automation));
    if (FAILED(hr)) throw Error("uia_unavailable", "UI Automation is unavailable");

    ComPtr<IUIAutomationElement> root;
    winrt::check_hresult(automation->ElementFromHandle(hwnd, &root));
    ComPtr<IUIAutomationCacheRequest> cache;
    winrt::check_hresult(automation->CreateCacheRequest(&cache));
    for (const PROPERTYID property : {UIA_NamePropertyId, UIA_ControlTypePropertyId,
                                      UIA_AutomationIdPropertyId, UIA_ClassNamePropertyId,
                                      UIA_BoundingRectanglePropertyId,
                                      UIA_IsEnabledPropertyId, UIA_IsOffscreenPropertyId,
                                      UIA_HasKeyboardFocusPropertyId,
                                      UIA_IsPasswordPropertyId, UIA_ValueValuePropertyId,
                                      UIA_RuntimeIdPropertyId}) {
        winrt::check_hresult(cache->AddProperty(property));
    }
    for (const PATTERNID pattern : {UIA_InvokePatternId, UIA_ValuePatternId,
                                    UIA_SelectionItemPatternId, UIA_TogglePatternId}) {
        winrt::check_hresult(cache->AddPattern(pattern));
    }
    ComPtr<IUIAutomationCondition> condition;
    winrt::check_hresult(automation->CreateTrueCondition(&condition));
    ComPtr<IUIAutomationElementArray> all;
    winrt::check_hresult(root->FindAllBuildCache(TreeScope_Subtree, condition.Get(), cache.Get(),
                                                 &all));
    int length = 0;
    winrt::check_hresult(all->get_Length(&length));

    AccessibilitySnapshot result;
    result.value = Json::object();
    result.value["tree"] = "";
    result.value["elements"] = Json::array();
    result.value["elementCount"] = 0;
    for (int i = 0; i < length; ++i) {
        context.check();
        ComPtr<IUIAutomationElement> element;
        if (FAILED(all->GetElement(i, &element)) || !element) continue;
        std::wstring name, automationId, className, rawValue;
        get_string(element.Get(), UIA_NamePropertyId, name);
        get_string(element.Get(), UIA_AutomationIdPropertyId, automationId);
        get_string(element.Get(), UIA_ClassNamePropertyId, className);
        get_string(element.Get(), UIA_ValueValuePropertyId, rawValue);
        const bool password = get_bool(element.Get(), UIA_IsPasswordPropertyId);
        const bool sensitive = looks_sensitive(name, rawValue, password);
        const RECT bounds = get_bounds(element.Get(), windowRect);
        const int controlType = get_int(element.Get(), UIA_ControlTypePropertyId);
        const auto typeName = control_type_name(controlType);
        const int index = static_cast<int>(result.elements.size());

        Json description = Json::object();
        description["index"] = index;
        description["name"] = utf8(name);
        description["controlType"] = typeName;
        description["automationId"] = utf8(automationId);
        description["className"] = utf8(className);
        description["enabled"] = get_bool(element.Get(), UIA_IsEnabledPropertyId);
        description["offscreen"] = get_bool(element.Get(), UIA_IsOffscreenPropertyId);
        description["focused"] = get_bool(element.Get(), UIA_HasKeyboardFocusPropertyId);
        description["bounds"] = Json{{"x", bounds.left}, {"y", bounds.top},
                                      {"width", std::max<LONG>(0, bounds.right - bounds.left)},
                                      {"height", std::max<LONG>(0, bounds.bottom - bounds.top)}};
        if (!rawValue.empty() || sensitive) description["value"] = sensitive ? "[redacted]" : utf8(rawValue);
        Json patterns = Json::array();
        if (cached_pattern(element.Get(), UIA_InvokePatternId)) patterns.push_back("invoke");
        if (cached_pattern(element.Get(), UIA_ValuePatternId)) patterns.push_back("value");
        if (cached_pattern(element.Get(), UIA_SelectionItemPatternId)) patterns.push_back("select");
        if (cached_pattern(element.Get(), UIA_TogglePatternId)) patterns.push_back("toggle");
        description["patterns"] = std::move(patterns);

        const auto runtimeId = get_runtime_id(element.Get());
        if (!runtimeId.empty()) description["runtimeId"] = runtimeId;
        result.value["elements"].push_back(description);

        std::ostringstream line;
        line << "[" << index << "] " << typeName;
        if (!name.empty()) line << " \"" << utf8(name) << "\"";
        line << " rect=" << rect_string(bounds);
        if (sensitive) line << " value=[redacted]";
        else if (!rawValue.empty()) line << " value=\"" << utf8(rawValue) << "\"";
        line << "\n";
        result.value["tree"] = result.value["tree"].get<std::string>() + line.str();

        ElementRecord record;
        record.description = std::move(description);
        record.element = std::move(element);
        result.elements.push_back(std::move(record));
    }
    result.value["elementCount"] = static_cast<int>(result.elements.size());
    return result;
}

// A caught fault leaves the provider, and possibly this apartment, in an
// undefined state, so the element is abandoned on the first one instead of
// being probed with the remaining patterns.  Returning false is what lets the
// caller fall back to synthetic input at the element's coordinates.
bool UiaSnapshotBuilder::invoke(ElementRecord& record, Context& context) const {
    context.check();
    if (!record.element) return false;
    HRESULT status = E_FAIL;

    ComPtr<IUIAutomationInvokePattern> invokePattern;
    if (const DWORD fault = guarded_pattern(record.element.Get(), UIA_InvokePatternId,
                                            __uuidof(IUIAutomationInvokePattern),
                                            reinterpret_cast<void**>(invokePattern.GetAddressOf()),
                                            &status)) {
        report_pattern_fault("GetCurrentPattern(Invoke)", record.description, fault);
        return false;
    }
    if (invokePattern) {
        if (const DWORD fault = guarded_invoke(invokePattern.Get(), &status)) {
            report_pattern_fault("InvokePattern.Invoke", record.description, fault);
            return false;
        }
        if (SUCCEEDED(status)) return true;
    }

    context.check();
    ComPtr<IUIAutomationSelectionItemPattern> selection;
    if (const DWORD fault = guarded_pattern(record.element.Get(), UIA_SelectionItemPatternId,
                                            __uuidof(IUIAutomationSelectionItemPattern),
                                            reinterpret_cast<void**>(selection.GetAddressOf()),
                                            &status)) {
        report_pattern_fault("GetCurrentPattern(SelectionItem)", record.description, fault);
        return false;
    }
    if (selection) {
        if (const DWORD fault = guarded_select(selection.Get(), &status)) {
            report_pattern_fault("SelectionItemPattern.Select", record.description, fault);
            return false;
        }
        if (SUCCEEDED(status)) return true;
    }

    context.check();
    ComPtr<IUIAutomationTogglePattern> toggle;
    if (const DWORD fault = guarded_pattern(record.element.Get(), UIA_TogglePatternId,
                                            __uuidof(IUIAutomationTogglePattern),
                                            reinterpret_cast<void**>(toggle.GetAddressOf()),
                                            &status)) {
        report_pattern_fault("GetCurrentPattern(Toggle)", record.description, fault);
        return false;
    }
    if (toggle) {
        if (const DWORD fault = guarded_toggle(toggle.Get(), &status)) {
            report_pattern_fault("TogglePattern.Toggle", record.description, fault);
            return false;
        }
        if (SUCCEEDED(status)) return true;
    }
    return false;
}

bool UiaSnapshotBuilder::set_value(ElementRecord& record, const std::wstring& value,
                                   Context& context) const {
    context.check();
    if (!record.element) return false;
    HRESULT status = E_FAIL;
    ComPtr<IUIAutomationValuePattern> pattern;
    if (const DWORD fault = guarded_pattern(record.element.Get(), UIA_ValuePatternId,
                                            __uuidof(IUIAutomationValuePattern),
                                            reinterpret_cast<void**>(pattern.GetAddressOf()),
                                            &status)) {
        report_pattern_fault("GetCurrentPattern(Value)", record.description, fault);
        return false;
    }
    if (!pattern) return false;
    context.check();
    BSTR text = SysAllocString(value.c_str());
    if (!text) throw Error("out_of_memory", "Cannot allocate UIA value");
    const DWORD fault = guarded_set_value(pattern.Get(), text, &status);
    SysFreeString(text);
    if (fault) {
        report_pattern_fault("ValuePattern.SetValue", record.description, fault);
        return false;
    }
    return SUCCEEDED(status);
}

namespace {

constexpr int kMaximumAncestors = 32;

ComPtr<IUIAutomationTreeWalker> control_view_walker(IUIAutomation* automation) {
    ComPtr<IUIAutomationTreeWalker> walker;
    if (!automation || FAILED(automation->get_ControlViewWalker(&walker))) return nullptr;
    return walker;
}

ComPtr<IUIAutomation> create_automation() {
    ComPtr<IUIAutomation> automation;
    if (FAILED(CoCreateInstance(CLSID_CUIAutomation, nullptr, CLSCTX_INPROC_SERVER,
                                IID_PPV_ARGS(&automation)))) {
        return nullptr;
    }
    return automation;
}

ComPtr<IUIAutomationElement> parent_of(IUIAutomationTreeWalker* walker,
                                       IUIAutomationElement* element) {
    ComPtr<IUIAutomationElement> parent;
    HRESULT status = E_FAIL;
    if (guarded_parent(walker, element, parent.GetAddressOf(), &status) || FAILED(status)) {
        return nullptr;
    }
    return parent;
}

ComPtr<IUIAutomationScrollPattern> scroll_pattern_of(IUIAutomationElement* element) {
    ComPtr<IUIAutomationScrollPattern> pattern;
    HRESULT status = E_FAIL;
    if (guarded_pattern(element, UIA_ScrollPatternId, __uuidof(IUIAutomationScrollPattern),
                        reinterpret_cast<void**>(pattern.GetAddressOf()), &status)) {
        return nullptr;
    }
    return pattern;
}

std::optional<RECT> screen_bounds(IUIAutomationElement* element) {
    RECT bounds{};
    HRESULT status = E_FAIL;
    if (!element || guarded_bounding_rectangle(element, &bounds, &status) || FAILED(status)) {
        return std::nullopt;
    }
    return bounds;
}

} // namespace

std::optional<RECT> UiaSnapshotBuilder::visible_bounds(IUIAutomationElement* element,
                                                       const RECT& windowRect) const {
    const auto own = screen_bounds(element);
    if (!own) return std::nullopt;
    RECT visible = *own;
    const auto automation = create_automation();
    if (const auto walker = control_view_walker(automation.Get())) {
        auto node = parent_of(walker.Get(), element);
        for (int depth = 0; node && depth < kMaximumAncestors; ++depth) {
            if (scroll_pattern_of(node.Get())) {
                const auto viewport = screen_bounds(node.Get());
                if (viewport && !IntersectRect(&visible, &visible, &*viewport)) return RECT{};
            }
            node = parent_of(walker.Get(), node.Get());
        }
    }
    return RECT{visible.left - windowRect.left, visible.top - windowRect.top,
                visible.right - windowRect.left, visible.bottom - windowRect.top};
}

ComPtr<IUIAutomationScrollPattern> UiaSnapshotBuilder::scroll_container_at(POINT screenPoint) const {
    const auto automation = create_automation();
    const auto walker = control_view_walker(automation.Get());
    if (!walker) return nullptr;
    ComPtr<IUIAutomationElement> node;
    HRESULT status = E_FAIL;
    if (guarded_element_from_point(automation.Get(), screenPoint, node.GetAddressOf(), &status) ||
        FAILED(status)) {
        return nullptr;
    }
    for (int depth = 0; node && depth < kMaximumAncestors; ++depth) {
        if (auto pattern = scroll_pattern_of(node.Get())) return pattern;
        node = parent_of(walker.Get(), node.Get());
    }
    return nullptr;
}

class UiaChangeWatcher::Handler final : public IUIAutomationEventHandler,
                                        public IUIAutomationStructureChangedEventHandler,
                                        public IUIAutomationPropertyChangedEventHandler {
public:
    std::optional<std::chrono::steady_clock::time_point> last_change() const {
        const auto ticks = lastChangeTicks_.load(std::memory_order_acquire);
        if (ticks == 0) return std::nullopt;
        return std::chrono::steady_clock::time_point(std::chrono::steady_clock::duration(ticks));
    }

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** result) override {
        if (!result) return E_POINTER;
        if (iid == __uuidof(IUnknown) || iid == __uuidof(IUIAutomationEventHandler)) {
            *result = static_cast<IUIAutomationEventHandler*>(this);
        } else if (iid == __uuidof(IUIAutomationStructureChangedEventHandler)) {
            *result = static_cast<IUIAutomationStructureChangedEventHandler*>(this);
        } else if (iid == __uuidof(IUIAutomationPropertyChangedEventHandler)) {
            *result = static_cast<IUIAutomationPropertyChangedEventHandler*>(this);
        } else {
            *result = nullptr;
            return E_NOINTERFACE;
        }
        AddRef();
        return S_OK;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return ++references_; }
    ULONG STDMETHODCALLTYPE Release() override {
        const ULONG remaining = --references_;
        if (remaining == 0) delete this;
        return remaining;
    }

    HRESULT STDMETHODCALLTYPE HandleAutomationEvent(IUIAutomationElement*, EVENTID) override {
        record();
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE HandleStructureChangedEvent(IUIAutomationElement*, StructureChangeType,
                                                          SAFEARRAY*) override {
        record();
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE HandlePropertyChangedEvent(IUIAutomationElement*, PROPERTYID,
                                                         VARIANT) override {
        record();
        return S_OK;
    }

private:
    void record() {
        lastChangeTicks_.store(std::chrono::steady_clock::now().time_since_epoch().count(),
                               std::memory_order_release);
    }

    std::atomic<ULONG> references_{1};
    std::atomic<std::chrono::steady_clock::rep> lastChangeTicks_{0};
};

UiaChangeWatcher::UiaChangeWatcher(HWND hwnd) : automation_(create_automation()) {
    ComPtr<IUIAutomationElement> root;
    if (!automation_ || FAILED(automation_->ElementFromHandle(hwnd, &root)) || !root) return;
    // Only the arrival time matters, so no element data is fetched across
    // processes for each event; a burst would otherwise delay its delivery.
    ComPtr<IUIAutomationCacheRequest> nothing;
    if (FAILED(automation_->CreateCacheRequest(&nothing)) ||
        FAILED(nothing->put_AutomationElementMode(AutomationElementMode_None))) {
        return;
    }
    auto* handler = new Handler();
    PROPERTYID scrollProperties[] = {UIA_ScrollHorizontalScrollPercentPropertyId,
                                     UIA_ScrollVerticalScrollPercentPropertyId};
    const bool registered =
        SUCCEEDED(automation_->AddAutomationEventHandler(UIA_LayoutInvalidatedEventId, root.Get(),
                                                         TreeScope_Subtree, nothing.Get(), handler)) &&
        SUCCEEDED(automation_->AddStructureChangedEventHandler(root.Get(), TreeScope_Subtree,
                                                               nothing.Get(), handler)) &&
        SUCCEEDED(automation_->AddPropertyChangedEventHandlerNativeArray(
            root.Get(), TreeScope_Subtree, nothing.Get(), handler, scrollProperties,
            static_cast<int>(std::size(scrollProperties))));
    if (!registered) {
        automation_->RemoveAllEventHandlers();
        handler->Release();
        return;
    }
    handler_ = handler;
}

UiaChangeWatcher::~UiaChangeWatcher() {
    if (!handler_) return;
    automation_->RemoveAllEventHandlers();
    handler_->Release();
}

std::optional<std::chrono::steady_clock::time_point> UiaChangeWatcher::last_change() const {
    return handler_ ? handler_->last_change() : std::nullopt;
}

std::optional<ScrollPosition> UiaSnapshotBuilder::scroll_position(
    IUIAutomationScrollPattern* container) const {
    if (!container) return std::nullopt;
    ScrollPosition position;
    HRESULT status = E_FAIL;
    if (guarded_scroll_percents(container, &position.horizontalPercent,
                                &position.verticalPercent, &status) ||
        FAILED(status)) {
        return std::nullopt;
    }
    return position;
}

} // namespace dcu::windows
