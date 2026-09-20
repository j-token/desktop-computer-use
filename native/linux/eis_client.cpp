#include "eis_client.hpp"

#include "dcu/backend.hpp"

#include <algorithm>
#include <array>
#include <chrono>
#include <cerrno>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <dlfcn.h>
#include <fcntl.h>
#include <poll.h>
#include <set>
#include <sstream>
#include <string>
#include <unistd.h>
#include <utility>
#include <vector>

/*
 * ABI provenance and licensing
 * ----------------------------
 * The opaque declarations, enum values, and function signatures below are a
 * minimal ABI shim for libei.  They are taken from Ubuntu 24.04 (noble)
 * libei-dev 1.2.1, /usr/include/libei-1.0/libei.h, whose upstream source is
 * https://gitlab.freedesktop.org/libinput/libei (SPDX-License-Identifier: MIT,
 * Copyright 2020 Red Hat, Inc.).  The project deliberately does not include
 * that header or link libei at build time: all symbols are resolved through
 * dlsym so the same binary can run on Ubuntu 22.04 installations that only
 * provide the runtime library or do not provide libei at all.
 *
 * Permission is granted, free of charge, to any person obtaining a copy of
 * this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions: the above
 * copyright notice and this permission notice shall be included in all copies
 * or substantial portions of the Software. THE SOFTWARE IS PROVIDED "AS IS",
 * WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
 * TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
 * NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
 * LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
 * CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
 * SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
 */

namespace dcu {
namespace {

// These are the public C types from libei.h.  They intentionally remain
// opaque; no private struct layout is relied upon by this implementation.
struct ei;
struct ei_event;
struct ei_seat;
struct ei_device;
struct ei_region;

enum ei_device_capability : int {
    EI_DEVICE_CAP_POINTER = (1 << 0),
    EI_DEVICE_CAP_POINTER_ABSOLUTE = (1 << 1),
    EI_DEVICE_CAP_KEYBOARD = (1 << 2),
    EI_DEVICE_CAP_TOUCH = (1 << 3),
    EI_DEVICE_CAP_SCROLL = (1 << 4),
    EI_DEVICE_CAP_BUTTON = (1 << 5),
};

enum ei_event_type : int {
    EI_EVENT_CONNECT = 1,
    EI_EVENT_DISCONNECT = 2,
    EI_EVENT_SEAT_ADDED = 3,
    EI_EVENT_SEAT_REMOVED = 4,
    EI_EVENT_DEVICE_ADDED = 5,
    EI_EVENT_DEVICE_REMOVED = 6,
    EI_EVENT_DEVICE_PAUSED = 7,
    EI_EVENT_DEVICE_RESUMED = 8,
};

struct Api {
    using EiNewSender = ei* (*)(void*);
    using EiConfigureName = void (*)(ei*, const char*);
    using EiSetupBackendFd = int (*)(ei*, int);
    using EiGetFd = int (*)(ei*);
    using EiDispatch = void (*)(ei*);
    using EiGetEvent = ei_event* (*)(ei*);
    using EiEventUnref = ei_event* (*)(ei_event*);
    using EiEventGetType = ei_event_type (*)(ei_event*);
    using EiEventGetSeat = ei_seat* (*)(ei_event*);
    using EiEventGetDevice = ei_device* (*)(ei_event*);
    using EiSeatRef = ei_seat* (*)(ei_seat*);
    using EiSeatUnref = ei_seat* (*)(ei_seat*);
    using EiSeatHasCapability = bool (*)(ei_seat*, ei_device_capability);
    using EiSeatBindCapabilities = void (*)(ei_seat*, ...);
    using EiDeviceRef = ei_device* (*)(ei_device*);
    using EiDeviceUnref = ei_device* (*)(ei_device*);
    using EiDeviceHasCapability = bool (*)(ei_device*, ei_device_capability);
    using EiDeviceGetRegion = ei_region* (*)(ei_device*, std::size_t);
    using EiRegionGetX = std::uint32_t (*)(ei_region*);
    using EiRegionGetY = std::uint32_t (*)(ei_region*);
    using EiRegionGetWidth = std::uint32_t (*)(ei_region*);
    using EiRegionGetHeight = std::uint32_t (*)(ei_region*);
    using EiRegionGetMappingId = const char* (*)(ei_region*);
    using EiDeviceStartEmulating = void (*)(ei_device*, std::uint32_t);
    using EiDeviceStopEmulating = void (*)(ei_device*);
    using EiDeviceFrame = void (*)(ei_device*, std::uint64_t);
    using EiDevicePointerMotionAbsolute = void (*)(ei_device*, double, double);
    using EiDeviceButtonButton = void (*)(ei_device*, std::uint32_t, bool);
    using EiDeviceScrollDelta = void (*)(ei_device*, double, double);
    using EiDeviceKeyboardKey = void (*)(ei_device*, std::uint32_t, bool);
    using EiNow = std::uint64_t (*)(ei*);
    using EiDeviceClose = void (*)(ei_device*);
    using EiUnref = ei* (*)(ei*);

    void* handle = nullptr;
    EiNewSender new_sender = nullptr;
    EiConfigureName configure_name = nullptr;
    EiSetupBackendFd setup_backend_fd = nullptr;
    EiGetFd get_fd = nullptr;
    EiDispatch dispatch = nullptr;
    EiGetEvent get_event = nullptr;
    EiEventUnref event_unref = nullptr;
    EiEventGetType event_get_type = nullptr;
    EiEventGetSeat event_get_seat = nullptr;
    EiEventGetDevice event_get_device = nullptr;
    EiSeatRef seat_ref = nullptr;
    EiSeatUnref seat_unref = nullptr;
    EiSeatHasCapability seat_has_capability = nullptr;
    EiSeatBindCapabilities seat_bind_capabilities = nullptr;
    EiDeviceRef device_ref = nullptr;
    EiDeviceUnref device_unref = nullptr;
    EiDeviceHasCapability device_has_capability = nullptr;
    EiDeviceGetRegion device_get_region = nullptr;
    EiRegionGetX region_get_x = nullptr;
    EiRegionGetY region_get_y = nullptr;
    EiRegionGetWidth region_get_width = nullptr;
    EiRegionGetHeight region_get_height = nullptr;
    // Added to the public API in libei 1.1.  Ubuntu 22.04 may not export it.
    EiRegionGetMappingId region_get_mapping_id = nullptr;
    EiDeviceStartEmulating device_start_emulating = nullptr;
    EiDeviceStopEmulating device_stop_emulating = nullptr;
    EiDeviceFrame device_frame = nullptr;
    EiDevicePointerMotionAbsolute device_pointer_motion_absolute = nullptr;
    EiDeviceButtonButton device_button_button = nullptr;
    EiDeviceScrollDelta device_scroll_delta = nullptr;
    EiDeviceKeyboardKey device_keyboard_key = nullptr;
    EiNow now = nullptr;
    EiDeviceClose device_close = nullptr;
    EiUnref unref = nullptr;

    Api() = default;
    Api(const Api&) = delete;
    Api& operator=(const Api&) = delete;

    ~Api() {
        unload();
    }

    void unload() noexcept {
        if (handle) dlclose(handle);
        handle = nullptr;
    }

    template <typename T>
    static T symbol(void* library, const char* name) {
        return reinterpret_cast<T>(dlsym(library, name));
    }

    bool load(std::string* error) {
        if (handle) return true;
        // libei.so.1 is the stable SONAME used by Ubuntu 22.04 and 24.04.
        // The second name helps development images with an unversioned linker
        // name without creating a build-time dependency.
        handle = dlopen("libei.so.1", RTLD_NOW | RTLD_LOCAL);
        if (!handle) handle = dlopen("libei.so", RTLD_NOW | RTLD_LOCAL);
        if (!handle) {
            if (error) {
                const char* detail = dlerror();
                *error = detail ? detail : "libei.so.1 could not be loaded";
            }
            return false;
        }

#define DCU_EI_REQUIRED(member, name) \
        member = symbol<decltype(member)>(handle, name); \
        if (!member) { \
            if (error) *error = std::string("libei is missing ") + name; \
            unload(); \
            return false; \
        }
#define DCU_EI_OPTIONAL(member, name) member = symbol<decltype(member)>(handle, name)

        DCU_EI_REQUIRED(new_sender, "ei_new_sender");
        DCU_EI_REQUIRED(configure_name, "ei_configure_name");
        DCU_EI_REQUIRED(setup_backend_fd, "ei_setup_backend_fd");
        DCU_EI_REQUIRED(get_fd, "ei_get_fd");
        DCU_EI_REQUIRED(dispatch, "ei_dispatch");
        DCU_EI_REQUIRED(get_event, "ei_get_event");
        DCU_EI_REQUIRED(event_unref, "ei_event_unref");
        DCU_EI_REQUIRED(event_get_type, "ei_event_get_type");
        DCU_EI_REQUIRED(event_get_seat, "ei_event_get_seat");
        DCU_EI_REQUIRED(event_get_device, "ei_event_get_device");
        DCU_EI_REQUIRED(seat_ref, "ei_seat_ref");
        DCU_EI_REQUIRED(seat_unref, "ei_seat_unref");
        DCU_EI_REQUIRED(seat_has_capability, "ei_seat_has_capability");
        DCU_EI_REQUIRED(seat_bind_capabilities, "ei_seat_bind_capabilities");
        DCU_EI_REQUIRED(device_ref, "ei_device_ref");
        DCU_EI_REQUIRED(device_unref, "ei_device_unref");
        DCU_EI_REQUIRED(device_has_capability, "ei_device_has_capability");
        DCU_EI_REQUIRED(device_get_region, "ei_device_get_region");
        DCU_EI_REQUIRED(region_get_x, "ei_region_get_x");
        DCU_EI_REQUIRED(region_get_y, "ei_region_get_y");
        DCU_EI_REQUIRED(region_get_width, "ei_region_get_width");
        DCU_EI_REQUIRED(region_get_height, "ei_region_get_height");
        DCU_EI_REQUIRED(device_start_emulating, "ei_device_start_emulating");
        DCU_EI_REQUIRED(device_stop_emulating, "ei_device_stop_emulating");
        DCU_EI_REQUIRED(device_frame, "ei_device_frame");
        DCU_EI_REQUIRED(device_pointer_motion_absolute, "ei_device_pointer_motion_absolute");
        DCU_EI_REQUIRED(device_button_button, "ei_device_button_button");
        DCU_EI_REQUIRED(device_scroll_delta, "ei_device_scroll_delta");
        DCU_EI_REQUIRED(device_keyboard_key, "ei_device_keyboard_key");
        DCU_EI_REQUIRED(now, "ei_now");
        DCU_EI_REQUIRED(device_close, "ei_device_close");
        DCU_EI_REQUIRED(unref, "ei_unref");

        DCU_EI_OPTIONAL(region_get_mapping_id, "ei_region_get_mapping_id");

#undef DCU_EI_REQUIRED
#undef DCU_EI_OPTIONAL
        return true;
    }
};

struct DeviceState {
    ei_device* device = nullptr;
    bool has_absolute_pointer = false;
    bool has_button = false;
    bool has_keyboard = false;
    bool has_scroll = false;
    bool resumed = false;
    bool emulating = false;
    std::uint32_t sequence = 0;
    std::vector<EisClient::Region> regions;
    std::set<std::uint32_t> buttons_down;
    std::set<std::uint32_t> keys_down;
};

struct SeatState {
    ei_seat* seat = nullptr;
};

enum class InputCapability {
    Button,
    Keyboard,
    Scroll,
};

constexpr std::chrono::seconds kHandshakeTimeout{5};
constexpr std::chrono::milliseconds kDispatchPollInterval{50};
constexpr std::size_t kMaxRegionsPerDevice = 256;
constexpr std::uint32_t kFirstEvdevButtonCode = 0x110;

std::string errno_text(int value) {
    const int code = value < 0 ? -value : value;
    std::ostringstream out;
    out << std::strerror(code) << " (" << code << ')';
    return out.str();
}

void check(Context& context) { context.check(); }

} // namespace

struct EisClient::Impl {
    Api api;
    ei* ei_context = nullptr;
    bool connected = false;
    bool disconnected = false;
    bool mapping_ids_available = false;
    std::vector<SeatState> seats;
    std::vector<DeviceState> devices;

    ~Impl() { close(); }

    void close() noexcept {
        if (!ei_context) {
            seats.clear();
            devices.clear();
            return;
        }

        release();
        for (auto& state : devices) {
            if (!state.device) continue;
            // DEVICE_REMOVED events are not needed during context teardown.
            if (state.emulating && !disconnected) api.device_stop_emulating(state.device);
            if (!disconnected) api.device_close(state.device);
            api.device_unref(state.device);
            state.device = nullptr;
        }
        devices.clear();
        for (auto& state : seats) {
            if (state.seat) api.seat_unref(state.seat);
            state.seat = nullptr;
        }
        seats.clear();
        api.unref(ei_context);
        ei_context = nullptr;
        connected = false;
        disconnected = false;
        mapping_ids_available = false;
    }

    void release() noexcept {
        if (!ei_context || disconnected) {
            for (auto& state : devices) {
                state.buttons_down.clear();
                state.keys_down.clear();
            }
            return;
        }
        for (auto& state : devices) {
            if (!state.device) continue;
            bool changed = false;
            if (state.resumed && state.emulating && state.has_button) {
                for (const std::uint32_t code : state.buttons_down) {
                    api.device_button_button(state.device, code, false);
                    changed = true;
                }
            }
            if (state.resumed && state.emulating && state.has_keyboard) {
                for (const std::uint32_t code : state.keys_down) {
                    api.device_keyboard_key(state.device, code, false);
                    changed = true;
                }
            }
            if (changed && state.resumed && state.emulating) {
                api.device_frame(state.device, api.now(ei_context));
            }
            state.buttons_down.clear();
            state.keys_down.clear();
        }
        flush_writes();
    }

    DeviceState* find_device(ei_device* device) {
        for (auto& state : devices) {
            if (state.device == device) return &state;
        }
        return nullptr;
    }

    static bool supports(const DeviceState& state, InputCapability capability) {
        switch (capability) {
        case InputCapability::Button:
            return state.has_button;
        case InputCapability::Keyboard:
            return state.has_keyboard;
        case InputCapability::Scroll:
            return state.has_scroll;
        }
        return false;
    }

    DeviceState* find_ready(InputCapability capability) {
        for (auto& state : devices) {
            if (state.device && state.resumed && state.emulating && supports(state, capability)) {
                return &state;
            }
        }
        return nullptr;
    }

    bool has_ready_absolute() const {
        for (const auto& state : devices) {
            if (state.device && state.resumed && state.emulating && state.has_absolute_pointer &&
                !state.regions.empty()) {
                return true;
            }
        }
        return false;
    }

    void bind_seat(ei_seat* seat) {
        // Bind each capability only when the EIS seat advertises it.  The
        // public API permits binding an unavailable capability, but checking
        // first makes the negotiation explicit and avoids requesting touch or
        // other device classes that this transport does not consume.
        constexpr std::array<ei_device_capability, 5> wanted = {
            EI_DEVICE_CAP_POINTER,
            EI_DEVICE_CAP_POINTER_ABSOLUTE,
            EI_DEVICE_CAP_BUTTON,
            EI_DEVICE_CAP_KEYBOARD,
            EI_DEVICE_CAP_SCROLL,
        };
        for (const auto capability : wanted) {
            if (api.seat_has_capability(seat, capability)) {
                api.seat_bind_capabilities(seat, capability,
                                           static_cast<ei_device_capability>(0));
            }
        }
        seats.push_back(SeatState{api.seat_ref(seat)});
    }

    void add_device(ei_device* device) {
        if (!device || find_device(device)) return;
        DeviceState state;
        state.device = api.device_ref(device);
        state.has_absolute_pointer = api.device_has_capability(device, EI_DEVICE_CAP_POINTER_ABSOLUTE);
        state.has_button = api.device_has_capability(device, EI_DEVICE_CAP_BUTTON);
        state.has_keyboard = api.device_has_capability(device, EI_DEVICE_CAP_KEYBOARD);
        state.has_scroll = api.device_has_capability(device, EI_DEVICE_CAP_SCROLL);
        if (state.has_absolute_pointer) {
            // Region indices are stable for a device lifetime.  A malformed
            // server must not make a client loop forever, hence the bound.
            for (std::size_t i = 0; i < kMaxRegionsPerDevice; ++i) {
                ei_region* region = api.device_get_region(device, i);
                if (!region) break;
                EisClient::Region value;
                value.x = static_cast<double>(api.region_get_x(region));
                value.y = static_cast<double>(api.region_get_y(region));
                value.width = static_cast<double>(api.region_get_width(region));
                value.height = static_cast<double>(api.region_get_height(region));
                if (api.region_get_mapping_id) {
                    const char* id = api.region_get_mapping_id(region);
                    if (id) value.mapping_id = id;
                }
                state.regions.push_back(std::move(value));
            }
            if (api.region_get_mapping_id) mapping_ids_available = true;
        }
        devices.push_back(std::move(state));
    }

    void remove_device(ei_device* device) noexcept {
        for (auto it = devices.begin(); it != devices.end(); ++it) {
            if (it->device != device) continue;
            if (it->device) api.device_unref(it->device);
            devices.erase(it);
            return;
        }
    }

    void remove_seat(ei_seat* seat) noexcept {
        for (auto it = seats.begin(); it != seats.end(); ++it) {
            if (it->seat != seat) continue;
            if (it->seat) api.seat_unref(it->seat);
            seats.erase(it);
            return;
        }
    }

    void on_event(ei_event* event) {
        const ei_event_type type = api.event_get_type(event);
        switch (type) {
        case EI_EVENT_CONNECT:
            connected = true;
            disconnected = false;
            break;
        case EI_EVENT_DISCONNECT:
            connected = false;
            disconnected = true;
            break;
        case EI_EVENT_SEAT_ADDED: {
            ei_seat* seat = api.event_get_seat(event);
            if (seat) bind_seat(seat);
            break;
        }
        case EI_EVENT_SEAT_REMOVED:
            remove_seat(api.event_get_seat(event));
            break;
        case EI_EVENT_DEVICE_ADDED:
            add_device(api.event_get_device(event));
            break;
        case EI_EVENT_DEVICE_REMOVED:
            remove_device(api.event_get_device(event));
            break;
        case EI_EVENT_DEVICE_PAUSED: {
            DeviceState* state = find_device(api.event_get_device(event));
            if (state) {
                state->resumed = false;
                state->emulating = false;
            }
            break;
        }
        case EI_EVENT_DEVICE_RESUMED: {
            DeviceState* state = find_device(api.event_get_device(event));
            if (!state || !state->device) break;
            if (state->emulating) break;
            state->resumed = true;
            state->sequence = state->sequence == UINT32_MAX ? 1 : state->sequence + 1;
            api.device_start_emulating(state->device, state->sequence);
            state->emulating = true;
            break;
        }
        default:
            // A sender should not receive input events, but ignoring an
            // extension event keeps the transport forward-compatible.
            break;
        }
    }

    bool dispatch_events() {
        api.dispatch(ei_context);
        bool event_received = false;
        for (;;) {
            ei_event* event = api.get_event(ei_context);
            if (!event) break;
            event_received = true;
            on_event(event);
            api.event_unref(event);
        }
        return event_received;
    }

    void flush_writes() noexcept {
        if (ei_context && !disconnected) api.dispatch(ei_context);
    }

    bool pump_once(Context& context, int timeout_ms) {
        check(context);
        const int fd = api.get_fd(ei_context);
        if (fd < 0) throw Error("eis_disconnected", "libei did not expose a transport fd");
        pollfd pfd{fd, POLLIN | POLLERR | POLLHUP, 0};
        int result;
        do {
            result = ::poll(&pfd, 1, timeout_ms);
        } while (result < 0 && errno == EINTR);
        if (result < 0) throw Error("eis_transport", std::string("poll failed: ") + errno_text(errno));
        const bool transport_closed = result > 0 &&
                                      (pfd.revents & (POLLERR | POLLHUP | POLLNVAL)) != 0;
        // libei still queues EI_EVENT_DISCONNECT where possible.  Dispatch
        // once before surfacing a poll error to the caller.  Dispatching on a
        // timeout also drains events queued by ei_setup_backend_fd() itself.
        const bool event_received = dispatch_events();
        flush_writes();
        if (transport_closed && !disconnected) {
            connected = false;
            disconnected = true;
        }
        return result != 0 || event_received;
    }

    void require_connected(Context& context) {
        if (!ei_context || disconnected) {
            throw Error("eis_not_ready", "EIS is not connected");
        }

        // EIS can pause or remove a device asynchronously.  Drain readable
        // notifications before every action so the capability and emulation
        // state below reflects the server rather than the previous request.
        pump_once(context, 0);
        if (disconnected || !connected) {
            throw Error("eis_disconnected", "The EIS server disconnected the client");
        }
    }

    void wait_until_ready(Context& context) {
        const auto deadline = std::chrono::steady_clock::now() + kHandshakeTimeout;
        while ((!connected || !has_ready_absolute()) && !disconnected) {
            check(context);
            const auto now = std::chrono::steady_clock::now();
            if (now >= deadline) {
                throw Error("timeout", "Timed out waiting for a resumed libei absolute device");
            }
            const auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(deadline - now);
            const int timeout = static_cast<int>(std::clamp<long long>(
                remaining.count(), 1, kDispatchPollInterval.count()));
            pump_once(context, timeout);
        }
        if (disconnected) throw Error("permission_required", "The EIS server disconnected the client");
        if (!connected) throw Error("eis_disconnected", "The EIS client did not connect");
    }

    DeviceState& require_capability(InputCapability capability, const char* name) {
        DeviceState* state = find_ready(capability);
        if (!state) throw Error("unsupported", std::string("EIS has no resumed ") + name + " device");
        return *state;
    }

    std::uint64_t now() const noexcept { return api.now(ei_context); }
};

EisClient::EisClient() : impl_(std::make_unique<Impl>()) {}
EisClient::~EisClient() = default;
EisClient::EisClient(EisClient&&) noexcept = default;
EisClient& EisClient::operator=(EisClient&&) noexcept = default;

void EisClient::connect(int fd, Context& context) {
    if (!impl_) impl_ = std::make_unique<Impl>();
    if (impl_->ei_context) throw Error("already_connected", "EisClient is already connected");
    check(context);
    if (fd < 0) throw Error("invalid_argument", "EIS fd must be non-negative");
    std::string load_error;
    if (!impl_->api.load(&load_error)) {
        throw Error("unsupported", "libei is unavailable: " + load_error);
    }
    int owned_fd = ::fcntl(fd, F_DUPFD_CLOEXEC, 0);
    if (owned_fd < 0 && errno == EINVAL) owned_fd = ::dup(fd);
    if (owned_fd < 0) {
        throw Error("eis_transport", std::string("Could not duplicate EIS fd: ") + errno_text(errno));
    }
    const int flags = ::fcntl(owned_fd, F_GETFL, 0);
    if (flags < 0 || ::fcntl(owned_fd, F_SETFL, flags | O_NONBLOCK) < 0) {
        const int saved_errno = errno;
        ::close(owned_fd);
        throw Error("eis_transport", std::string("Could not make EIS fd non-blocking: ") +
                                   errno_text(saved_errno));
    }
    impl_->ei_context = impl_->api.new_sender(nullptr);
    if (!impl_->ei_context) {
        ::close(owned_fd);
        throw Error("eis_transport", "ei_new_sender failed");
    }
    impl_->api.configure_name(impl_->ei_context, "desktop-computer-use");
    const int result = impl_->api.setup_backend_fd(impl_->ei_context, owned_fd);
    if (result < 0) {
        // setup_backend_fd owns/cleans the descriptor according to libei's
        // public contract, and ei_unref tears down the partially initialized
        // context.
        impl_->api.unref(impl_->ei_context);
        impl_->ei_context = nullptr;
        throw Error("eis_transport", "ei_setup_backend_fd failed: " + errno_text(result));
    }
    try {
        impl_->wait_until_ready(context);
    } catch (...) {
        impl_->close();
        throw;
    }
}

bool EisClient::connected() const noexcept { return impl_ && impl_->connected && !impl_->disconnected; }
bool EisClient::ready() const noexcept { return connected() && impl_->has_ready_absolute(); }
bool EisClient::mapping_ids_available() const noexcept {
    return impl_ && impl_->mapping_ids_available;
}

std::vector<EisClient::Region> EisClient::regions() const {
    std::vector<Region> result;
    if (!impl_) return result;
    for (const auto& state : impl_->devices) {
        if (state.device && state.resumed && state.emulating && state.has_absolute_pointer) {
            result.insert(result.end(), state.regions.begin(), state.regions.end());
        }
    }
    return result;
}

void EisClient::absolute(double x, double y, std::string_view mapping_id, Context& context) {
    check(context);
    if (!std::isfinite(x) || !std::isfinite(y)) {
        throw Error("invalid_argument", "Absolute EIS coordinates must be finite");
    }
    if (!impl_) throw Error("eis_not_ready", "EIS is not connected");
    impl_->require_connected(context);
    if (!impl_->has_ready_absolute()) {
        throw Error("eis_not_ready", "EIS has no resumed absolute device");
    }
    DeviceState* selected = nullptr;
    Region* selected_region = nullptr;
    for (auto& state : impl_->devices) {
        if (!state.device || !state.resumed || !state.emulating || !state.has_absolute_pointer) continue;
        for (auto& region : state.regions) {
            if ((!mapping_id.empty() && region.mapping_id == mapping_id) ||
                (mapping_id.empty() && state.regions.size() == 1)) {
                selected = &state;
                selected_region = &region;
                break;
            }
        }
        if (selected) break;
    }
    if (!selected) {
        if (!mapping_id.empty() && !impl_->mapping_ids_available) {
            throw Error("unsupported", "This libei version does not expose region mapping IDs");
        }
        throw Error("invalid_argument", "mappingId does not identify a resumed EIS region");
    }
    if (x < 0.0 || y < 0.0 || x >= selected_region->width || y >= selected_region->height) {
        throw Error("invalid_argument", "Absolute EIS coordinates are outside the selected region");
    }
    impl_->api.device_pointer_motion_absolute(selected->device,
                                               selected_region->x + x,
                                               selected_region->y + y);
    impl_->api.device_frame(selected->device, impl_->now());
    impl_->flush_writes();
}

void EisClient::absolute_global(double x, double y, Context& context) {
    check(context);
    if (!std::isfinite(x) || !std::isfinite(y)) {
        throw Error("invalid_argument", "Absolute EIS coordinates must be finite");
    }
    if (!impl_) throw Error("eis_not_ready", "EIS is not connected");
    impl_->require_connected(context);
    if (!impl_->has_ready_absolute()) {
        throw Error("eis_not_ready", "EIS has no resumed absolute device");
    }

    DeviceState* selected = nullptr;
    for (auto& state : impl_->devices) {
        if (!state.device || !state.resumed || !state.emulating || !state.has_absolute_pointer) {
            continue;
        }
        for (const auto& region : state.regions) {
            const bool inside_region = x >= region.x && x < region.x + region.width &&
                                       y >= region.y && y < region.y + region.height;
            if (inside_region) {
                selected = &state;
                break;
            }
        }
        if (selected) break;
    }
    if (!selected) {
        throw Error("invalid_argument", "Absolute EIS coordinates are outside all regions");
    }

    impl_->api.device_pointer_motion_absolute(selected->device, x, y);
    impl_->api.device_frame(selected->device, impl_->now());
    impl_->flush_writes();
}

void EisClient::button(std::uint32_t evdev_code, bool pressed, Context& context) {
    check(context);
    if (evdev_code < kFirstEvdevButtonCode) {
        throw Error("invalid_argument", "button code must be an evdev BTN_* code");
    }
    if (!impl_) {
        throw Error("eis_not_ready", "EIS is not connected");
    }
    impl_->require_connected(context);
    DeviceState& state = impl_->require_capability(InputCapability::Button, "button");
    impl_->api.device_button_button(state.device, evdev_code, pressed);
    impl_->api.device_frame(state.device, impl_->now());
    impl_->flush_writes();
    if (pressed) state.buttons_down.insert(evdev_code);
    else state.buttons_down.erase(evdev_code);
}

void EisClient::key(std::uint32_t evdev_keycode, bool pressed, Context& context) {
    check(context);
    if (!impl_) {
        throw Error("eis_not_ready", "EIS is not connected");
    }
    impl_->require_connected(context);
    DeviceState& state = impl_->require_capability(InputCapability::Keyboard, "keyboard");
    impl_->api.device_keyboard_key(state.device, evdev_keycode, pressed);
    impl_->api.device_frame(state.device, impl_->now());
    impl_->flush_writes();
    if (pressed) state.keys_down.insert(evdev_keycode);
    else state.keys_down.erase(evdev_keycode);
}

void EisClient::scroll(double dx, double dy, Context& context) {
    check(context);
    if (!std::isfinite(dx) || !std::isfinite(dy)) {
        throw Error("invalid_argument", "EIS scroll deltas must be finite");
    }
    if (!impl_) {
        throw Error("eis_not_ready", "EIS is not connected");
    }
    impl_->require_connected(context);
    DeviceState& state = impl_->require_capability(InputCapability::Scroll, "scroll");
    impl_->api.device_scroll_delta(state.device, dx, dy);
    impl_->api.device_frame(state.device, impl_->now());
    impl_->flush_writes();
}

void EisClient::release() noexcept {
    if (impl_) impl_->release();
}

void EisClient::close() noexcept {
    if (impl_) impl_->close();
}

} // namespace dcu
