#include "uia.hpp"

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

#include <algorithm>
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
        record.bounds = bounds;
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

} // namespace dcu::windows
