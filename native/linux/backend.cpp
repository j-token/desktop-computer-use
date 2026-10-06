#include "dcu/backend.hpp"
#include "dcu/window_relations.hpp"

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <functional>
#include <iomanip>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <random>
#include <set>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <unordered_map>
#include <utility>
#include <vector>

#include <fcntl.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include <dlfcn.h>

#if DCU_HAVE_GIO
#include <gio/gio.h>
#include <glib.h>
#endif

#if DCU_HAVE_X11
#include <X11/Xatom.h>
#include <X11/keysym.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#endif

#if DCU_HAVE_XTEST
#include <X11/extensions/XTest.h>
#endif

#if DCU_HAVE_XRANDR
#include <X11/extensions/Xrandr.h>
#endif

#if DCU_HAVE_JPEG
extern "C" {
#include <jpeglib.h>
}
#endif

#if DCU_HAVE_PNG
extern "C" {
#include <png.h>
}
#endif

#if DCU_HAVE_PIPEWIRE
extern "C" {
#include <pipewire/pipewire.h>
#include <pipewire/version.h>
#include <spa/param/video/format-utils.h>
#include <spa/param/video/raw.h>
#include <spa/pod/builder.h>
#include <spa/buffer/meta.h>
}
#endif

#if DCU_HAVE_ATSPI
#include <atspi/atspi.h>
#endif

#if DCU_HAVE_GIO
#include "bus.hpp"
#endif
#include "eis_client.hpp"

namespace dcu {
using Json = dcu::Json;
using Clock = std::chrono::steady_clock;

struct WindowInfo {
    std::string id;
    std::string app;
    std::string title;
    int pid = 0;
    int x = 0;
    int y = 0;
    int width = 0;
    int height = 0;
    bool active = false;
    // Transient-for parent of a dialog, when that parent is listed.
    std::string owner_id;
    // A modal dialog that blocks input to owner_id.
    bool modal = false;
};

struct RgbaImage {
    int width = 0;
    int height = 0;
    std::vector<std::uint8_t> rgba;
    std::chrono::system_clock::time_point captured_at{};
    std::uint64_t frame_sequence = 0;
    std::uint64_t source_frame_sequence = 0;
    std::int64_t frame_timestamp_ns = 0;
    std::uint64_t frame_generation = 0;
    bool fresh_frame = false;
    bool cached_frame = false;
};

// One encoded screenshot. scale_x/scale_y map window-local coordinates to
// this image's pixels: pixel = window * scale.
struct ObservationImage {
    std::string path;
    int width = 0;
    int height = 0;
    double scale_x = 1.0;
    double scale_y = 1.0;
};

struct ObservationRecord {
    std::string id;
    WindowInfo window;
    Clock::time_point created;
    std::uint64_t coordinate_revision = 0;
    bool has_screenshot = false;
    std::string mime;
    ObservationImage reduced;
    ObservationImage full;
};

// Screenshot pixel space selected for coordinate input.
struct CoordinateTransform {
    std::string space;
    ObservationImage image;
};

#if DCU_HAVE_GIO
struct PortalStreamInfo {
    bool valid = false;
    std::uint32_t node_id = UINT32_MAX;
    std::uint64_t pipewire_serial = 0;
    std::string mapping_id;
    int position_x = 0;
    int position_y = 0;
    int compositor_width = 0;
    int compositor_height = 0;
    int logical_width = 0;
    int logical_height = 0;
};
#endif

class HeldButton {
public:
    HeldButton() = default;
    HeldButton(std::function<void()> release, bool held) : release_(std::move(release)), held_(held) {}
    HeldButton(const HeldButton&) = delete;
    HeldButton& operator=(const HeldButton&) = delete;
    HeldButton(HeldButton&& other) noexcept
        : release_(std::move(other.release_)), held_(std::exchange(other.held_, false)) {}
    ~HeldButton() { reset(); }
    void reset() noexcept {
        if (held_ && release_) {
            try { release_(); } catch (...) { }
        }
        held_ = false;
    }
    void dismiss() noexcept { held_ = false; }
private:
    std::function<void()> release_;
    bool held_ = false;
};

constexpr int kDefaultDragDurationMs = 240;
constexpr int kDefaultDragSteps = 12;
constexpr int kDefaultHoldBeforeMs = 50;
constexpr int kDefaultHoldAfterMs = 50;
// How long the GNOME extension releases Escape before an injected Escape press.
constexpr std::uint32_t kStopKeySuspendMs = 300;
constexpr int kIdleTimeoutSeconds = 120;
constexpr std::size_t kMaxTreeNodes = 1200;
constexpr std::size_t kMaxTreeDepth = 64;
constexpr std::size_t kMaxText = 500;

namespace {
std::string env(const char* name);
bool env_present(const char* name);
std::string lower(std::string value);
std::string uuid();
std::string utc_now();
std::string utc_now(std::chrono::system_clock::time_point value);
std::string runtime_directory();
bool command_exists(const std::string& command);
bool libei_runtime_available();
void check_context(Context& context);
int integer(const Json& object, const char* key, int fallback);
double number(const Json& object, const char* key, double fallback);
std::string string_value(const Json& object, const char* key, std::string fallback = {});
Json action_result();
Json window_json(const WindowInfo& window);
Json valid_overlay_regions(const Json& candidate);
void wait_checked(Context& context, int milliseconds);
#if DCU_HAVE_ATSPI
std::optional<std::array<int, 4>> atspi_rect(AtspiAccessible* node);
AtspiAccessible* atspi_element_for_index(const WindowInfo& window, const Json& params, Context& context);
#endif
}
#if DCU_HAVE_GIO
GVariant* empty_options();
std::string variant_string(GVariant* dictionary, const char* key);
PortalStreamInfo first_stream_info(GVariant* values);
#endif

class LinuxBackend final : public Backend {
public:
    LinuxBackend();
    ~LinuxBackend() override;

    Json execute(const std::string& method, const Json& params, Context& context) override;
    void interrupt() noexcept override;
    void set_toggle(const std::string& key, bool down, Context& context) override;
    void release_toggles() noexcept override;

private:
    Json doctor() const;
    Json capabilities() const;
    Json session_start(const Json& params, Context& context);
    Json session_stop(const Json& params);
    Json list_windows(const Json& params);
    Json list_apps(const Json& params);
    Json get_app_state(const Json& params, Context& context);
    Json click(const Json& params, Context& context);
    Json drag(const Json& params, Context& context);
    Json scroll(const Json& params, Context& context);
    Json type_text(const Json& params, Context& context);
    Json press_key(const Json& params, Context& context);
    Json hotkey(const Json& params, Context& context);
    Json set_value(const Json& params, Context& context);
    Json paste_text(const Json& params, Context& context);

    void ensure_active(const Json& params) const;
    std::vector<WindowInfo> enumerate_windows() const;
    std::vector<WindowInfo> extension_windows() const;
    WindowInfo select_window(const Json& params) const;
    std::optional<WindowInfo> blocking_modal(const WindowInfo& target) const;
    void reject_blocked_target(const Json& params) const;
    WindowInfo current_window(const WindowInfo& expected) const;
    void validate_observation(const Json& params, const WindowInfo& window);
    void store_observation(ObservationRecord record);
    void clear_observations() noexcept;
    CoordinateTransform coordinate_transform(const Json& params, const WindowInfo& window) const;
    Json full_screenshot(const Json& params);
    Json make_observation(const Json& params, Context& context);
    RgbaImage capture_window(const WindowInfo& window, Context& context);
    void save_image(const RgbaImage& image, const std::string& path, const std::string& format,
                    int quality, std::string* mime);
    Json accessibility_snapshot(const WindowInfo& window, Context& context);

    void activate(const WindowInfo& window);
    void move_pointer(int x, int y, Context& context, const WindowInfo* target = nullptr);
    void button_event(const std::string& button, bool pressed, Context& context);
    void scroll_event(int amount, const std::string& direction, Context& context);
    void key_event(const std::string& key, bool pressed, Context& context);
    void suspend_stop_key_for_escape();
    void hotkey_event(const std::vector<std::string>& keys, Context& context);
    void type_text_impl(const std::string& text, Context& context);
    void release_inputs() noexcept;
    void release_toggled_keys() noexcept;
    void require_not_toggled(const std::string& key) const;
    void mark_input_complete() noexcept;
    std::string coordinate_signature() const;
    void refresh_coordinate_state();
#if DCU_HAVE_GIO
    void close_portal_sessions() noexcept;
    void start_stop_monitor();
    void stop_stop_monitor() noexcept;
#endif
    bool portal_ready_for_input(Context& context);
    bool portal_notify_motion_absolute(double x, double y, const WindowInfo* target, Context& context);
    bool portal_notify_button(const std::string& button, bool pressed, Context& context);
    bool portal_notify_axis(double dx, double dy, Context& context);
    bool portal_notify_key(std::uint32_t key, bool pressed, Context& context);

    static std::string button_name(const Json& params);
    static std::vector<std::string> key_list(const Json& params);

    mutable std::mutex mutex_;
    std::atomic_bool interrupted_{false};
    std::atomic_bool input_release_pending_{false};
    std::atomic_bool session_active_{false};
    std::atomic_bool heartbeat_stop_{false};
    std::string session_id_;
    Clock::time_point last_activity_{};
    // Canonical names (shift, ctrl, alt, super, space) of keys held across
    // requests. Only touched by serialized calls and the destructor.
    std::set<std::string> toggled_;
    std::thread heartbeat_thread_;
    std::unordered_map<std::string, ObservationRecord> observations_;
    std::string image_directory_;
    std::string session_type_;
    bool wayland_ = false;
    bool x11_ = false;
    std::uint64_t coordinate_revision_ = 0;
    std::string coordinate_signature_;

#if DCU_HAVE_GIO
    std::unique_ptr<SessionBus> shell_bus_;
    std::unique_ptr<PortalBus> portal_bus_;
    std::atomic_bool stop_monitor_stop_{true};
    std::thread stop_monitor_thread_;
    std::string remote_desktop_session_;
    bool remote_desktop_started_ = false;
    bool remote_stream_valid_ = false;
    bool eis_session_selected_ = false;
    std::uint32_t remote_stream_id_ = UINT32_MAX;
    std::uint64_t remote_stream_serial_ = 0;
    std::string remote_mapping_id_;
    int remote_stream_x_ = 0;
    int remote_stream_y_ = 0;
    int remote_stream_width_ = 0;
    int remote_stream_height_ = 0;
    int remote_logical_width_ = 0;
    int remote_logical_height_ = 0;
#endif

    std::unique_ptr<EisClient> eis_;
    std::atomic<Context*> active_context_{nullptr};

#if DCU_HAVE_X11
    Display* display_ = nullptr;
    Window root_window_ = 0;
#endif

#if DCU_HAVE_PIPEWIRE && DCU_HAVE_GIO
    struct PipeWireCapture;
    std::unique_ptr<PipeWireCapture> pipewire_capture_;
#endif
};

#if DCU_HAVE_X11
std::string x11_property_string(Display* display, Window window, Atom atom) {
    if (!display || atom == None) return {};
    Atom actual_type = None;
    int actual_format = 0;
    unsigned long item_count = 0;
    unsigned long bytes_after = 0;
    unsigned char* data = nullptr;
    const int status = XGetWindowProperty(display, window, atom, 0, 4096, False,
                                          AnyPropertyType, &actual_type, &actual_format,
                                          &item_count, &bytes_after, &data);
    if (status != Success || !data) return {};
    std::string value(reinterpret_cast<char*>(data),
                      actual_format == 8 ? static_cast<std::size_t>(item_count) : 0);
    XFree(data);
    return value;
}

long x11_property_cardinal(Display* display, Window window, Atom atom) {
    if (!display || atom == None) return 0;
    Atom actual_type = None;
    int actual_format = 0;
    unsigned long item_count = 0;
    unsigned long bytes_after = 0;
    unsigned char* data = nullptr;
    const int status = XGetWindowProperty(display, window, atom, 0, 1, False,
                                          AnyPropertyType, &actual_type, &actual_format,
                                          &item_count, &bytes_after, &data);
    if (status != Success || !data || item_count == 0) {
        if (data) XFree(data);
        return 0;
    }
    long value = 0;
    if (actual_format == 32) value = static_cast<long>(reinterpret_cast<unsigned long*>(data)[0]);
    XFree(data);
    return value;
}

bool x11_has_state(Display* display, Window window, const char* state_name) {
    if (!display) return false;
    const Atom state_atom = XInternAtom(display, "_NET_WM_STATE", False);
    const Atom wanted = XInternAtom(display, state_name, False);
    if (state_atom == None || wanted == None) return false;
    Atom actual_type = None;
    int actual_format = 0;
    unsigned long item_count = 0;
    unsigned long bytes_after = 0;
    unsigned char* data = nullptr;
    const int status = XGetWindowProperty(display, window, state_atom, 0, 64, False, XA_ATOM,
                                          &actual_type, &actual_format, &item_count,
                                          &bytes_after, &data);
    if (status != Success || !data) return false;
    bool found = false;
    if (actual_format == 32) {
        const auto* atoms = reinterpret_cast<Atom*>(data);
        for (unsigned long index = 0; index < item_count && !found; ++index) found = atoms[index] == wanted;
    }
    XFree(data);
    return found;
}

bool x11_is_viewable(Display* display, Window window) {
    XWindowAttributes attrs{};
    return display && XGetWindowAttributes(display, window, &attrs) != 0 &&
           attrs.map_state == IsViewable && attrs.width > 0 && attrs.height > 0;
}

WindowInfo x11_window_info(Display* display, Window root, Window window, Window active) {
    WindowInfo result;
    result.id = "x11:" + std::to_string(static_cast<unsigned long>(window));
    result.pid = static_cast<int>(x11_property_cardinal(display, window,
                                                          XInternAtom(display, "_NET_WM_PID", False)));
    result.title = x11_property_string(display, window,
                                       XInternAtom(display, "_NET_WM_NAME", False));
    if (result.title.empty()) {
        char* name = nullptr;
        if (XFetchName(display, window, &name) && name) {
            result.title = name;
            XFree(name);
        }
    }
    XClassHint hint{};
    if (XGetClassHint(display, window, &hint)) {
        if (hint.res_class) result.app = hint.res_class;
        if (result.app.empty() && hint.res_name) result.app = hint.res_name;
        if (hint.res_name) XFree(hint.res_name);
        if (hint.res_class) XFree(hint.res_class);
    }
    if (result.app.empty()) result.app = "unknown";
    result.active = window == active;
    Window transient_for = 0;
    if (XGetTransientForHint(display, window, &transient_for) && transient_for &&
        transient_for != root && transient_for != window) {
        result.owner_id = "x11:" + std::to_string(static_cast<unsigned long>(transient_for));
        result.modal = x11_has_state(display, window, "_NET_WM_STATE_MODAL");
    }

    XWindowAttributes attrs{};
    if (XGetWindowAttributes(display, window, &attrs)) {
        result.width = attrs.width;
        result.height = attrs.height;
        int absolute_x = 0;
        int absolute_y = 0;
        Window child = 0;
        XTranslateCoordinates(display, window, root, 0, 0, &absolute_x, &absolute_y, &child);
        result.x = absolute_x;
        result.y = absolute_y;
    }
    return result;
}
#endif

LinuxBackend::LinuxBackend()
    : image_directory_(runtime_directory()), session_type_(env("XDG_SESSION_TYPE")) {
    wayland_ = lower(session_type_) == "wayland" || env_present("WAYLAND_DISPLAY");
    x11_ = !env("DISPLAY").empty();
#if DCU_HAVE_X11
    if (x11_) {
        display_ = XOpenDisplay(nullptr);
        if (display_) root_window_ = DefaultRootWindow(display_);
        else x11_ = false;
    }
#endif
#if DCU_HAVE_GIO
    shell_bus_ = std::make_unique<SessionBus>();
    portal_bus_ = std::make_unique<PortalBus>();
#endif
}

#if DCU_HAVE_GIO
void LinuxBackend::start_stop_monitor() {
    stop_stop_monitor();
    stop_monitor_stop_.store(false, std::memory_order_release);
    stop_monitor_thread_ = std::thread([this] {
        try {
            if (!shell_bus_ || !shell_bus_->available()) return;
            ShellStopSubscription subscription(shell_bus_->connection());
            while (!stop_monitor_stop_.load(std::memory_order_acquire)) {
                if (subscription.poll_stopped()) {
                    session_active_.store(false, std::memory_order_release);
                    heartbeat_stop_.store(true, std::memory_order_release);
                    if (auto* context = active_context_.load(std::memory_order_acquire)) {
                        context->cancelled.store(true, std::memory_order_release);
                    }
                    stop_monitor_stop_.store(true, std::memory_order_release);
                    return;
                }
                std::this_thread::sleep_for(std::chrono::milliseconds(10));
            }
        } catch (...) {
            // A disappearing session bus is equivalent to a stopped
            // indicator.  The serialized worker performs the actual release
            // and portal cleanup after it observes this cancellation.
            session_active_.store(false, std::memory_order_release);
            heartbeat_stop_.store(true, std::memory_order_release);
            if (auto* context = active_context_.load(std::memory_order_acquire)) {
                context->cancelled.store(true, std::memory_order_release);
            }
            stop_monitor_stop_.store(true, std::memory_order_release);
        }
    });
}

void LinuxBackend::stop_stop_monitor() noexcept {
    stop_monitor_stop_.store(true, std::memory_order_release);
    if (stop_monitor_thread_.joinable()) stop_monitor_thread_.join();
}
#endif

LinuxBackend::~LinuxBackend() {
    heartbeat_stop_.store(true);
#if DCU_HAVE_GIO
    stop_monitor_stop_.store(true);
#endif
    if (heartbeat_thread_.joinable()) heartbeat_thread_.join();
#if DCU_HAVE_GIO
    if (stop_monitor_thread_.joinable()) stop_monitor_thread_.join();
#endif
    release_inputs();
#if DCU_HAVE_PIPEWIRE && DCU_HAVE_GIO
    pipewire_capture_.reset();
#endif
#if DCU_HAVE_GIO
    close_portal_sessions();
#endif
#if DCU_HAVE_X11
    if (display_) XCloseDisplay(display_);
#endif
}

void LinuxBackend::interrupt() noexcept {
    interrupted_.store(true, std::memory_order_release);
    heartbeat_stop_.store(true, std::memory_order_release);
    if (auto* context = active_context_.load(std::memory_order_acquire)) {
        context->cancelled.store(true, std::memory_order_release);
    }
}

void LinuxBackend::ensure_active(const Json& params) const {
    if (!session_active_.load(std::memory_order_acquire)) {
        throw Error("session_required", "Start a computer-use session before input actions");
    }
    const std::string requested = string_value(params, "sessionId");
    if (!requested.empty() && requested != session_id_) {
        throw Error("session_owner", "The input session does not belong to this client");
    }
    if (Clock::now() - last_activity_ > std::chrono::seconds(kIdleTimeoutSeconds)) {
        throw Error("session_expired", "The computer-use session expired after 120 seconds idle");
    }
}

Json LinuxBackend::doctor() const {
    Json result = Json::object();
    result["platform"] = "linux";
    result["sessionType"] = session_type_.empty() ? "unknown" : session_type_;
    result["graphicalSession"] = wayland_ || x11_;
    result["xdgRuntimeDir"] = env("XDG_RUNTIME_DIR");
    result["dbusSessionBus"] = env_present("DBUS_SESSION_BUS_ADDRESS");
    result["display"] = env("DISPLAY");
    result["waylandDisplay"] = env("WAYLAND_DISPLAY");
    result["dependencies"] = {
        {"gio", DCU_HAVE_GIO != 0},
        {"x11", DCU_HAVE_X11 != 0},
        {"xtest", DCU_HAVE_XTEST != 0},
        {"xrandr", DCU_HAVE_XRANDR != 0},
        {"jpeg", DCU_HAVE_JPEG != 0},
        {"png", DCU_HAVE_PNG != 0},
        {"pipewire", DCU_HAVE_PIPEWIRE != 0},
        {"atspi", DCU_HAVE_ATSPI != 0},
        {"wlCopy", command_exists("wl-copy")},
        {"xclip", command_exists("xclip")},
        {"xdotool", command_exists("xdotool")},
    };
#if DCU_HAVE_GIO
    result["gnomeExtension"] = shell_bus_ && shell_bus_->available();
    if (shell_bus_ && !shell_bus_->available()) result["gnomeExtensionError"] = shell_bus_->error();
    result["portal"] = portal_bus_ && portal_bus_->available();
    if (portal_bus_ && !portal_bus_->available()) result["portalError"] = portal_bus_->error();
#else
    result["gnomeExtension"] = false;
    result["portal"] = false;
#endif
    bool indicator_available = false;
#if DCU_HAVE_GIO
    indicator_available = shell_bus_ && shell_bus_->available();
#endif
    result["ready"] = indicator_available &&
                       ((x11_ && DCU_HAVE_XTEST && DCU_HAVE_JPEG) ||
                        (wayland_ && DCU_HAVE_GIO && DCU_HAVE_PIPEWIRE));
    result["stopKey"] = "Escape x2";
    result["indicatorRequired"] = true;
    result["notes"] = Json::array();
    if (!wayland_ && !x11_) result["notes"].push_back("Run inside the logged-in GNOME graphical session; SSH alone is not a desktop session.");
    if (wayland_ && !(DCU_HAVE_GIO && DCU_HAVE_PIPEWIRE)) {
        result["notes"].push_back("Wayland requires the XDG ScreenCast/RemoteDesktop portals and PipeWire development/runtime support.");
    }
    if (x11_ && !DCU_HAVE_XTEST) result["notes"].push_back("X11 input is unavailable until the XTest library is installed.");
    if (DCU_HAVE_GIO && (!shell_bus_ || !shell_bus_->available())) {
        result["notes"].push_back("Install and enable the GNOME desktop-computer-use extension before session.start.");
    }
    return result;
}

Json LinuxBackend::capabilities() const {
    Json result = Json::object();
    result["platform"] = "linux";
    result["sessionType"] = session_type_.empty() ? "unknown" : session_type_;
    bool indicator_available = false;
#if DCU_HAVE_GIO
    indicator_available = shell_bus_ && shell_bus_->available();
#endif
    const bool wayland_input = wayland_ && DCU_HAVE_GIO;
    result["input"] = {
        {"click", (x11_ && DCU_HAVE_XTEST) || wayland_input},
        {"drag", (x11_ && DCU_HAVE_XTEST) || wayland_input},
        {"scroll", (x11_ && DCU_HAVE_XTEST) || wayland_input},
        {"keyboard", (x11_ && DCU_HAVE_XTEST) || wayland_input},
        {"modifierClick", (x11_ && DCU_HAVE_XTEST) || wayland_input},
        {"setValue", DCU_HAVE_ATSPI != 0},
        {"pasteText", command_exists("wl-copy") || command_exists("xclip") || command_exists("xsel")},
    };
    result["observation"] = {
        {"screenshot", (x11_ && DCU_HAVE_JPEG) || (wayland_ && DCU_HAVE_GIO && DCU_HAVE_PIPEWIRE)},
        {"accessibility", DCU_HAVE_ATSPI != 0},
        {"jpeg", DCU_HAVE_JPEG != 0},
        {"png", DCU_HAVE_PNG != 0},
        {"maxEdge", 0},
        {"capturePersistent", true},
    };
    result["dragDefaults"] = {
        {"durationMs", kDefaultDragDurationMs},
        {"steps", kDefaultDragSteps},
        {"holdBeforeMs", kDefaultHoldBeforeMs},
        {"holdAfterMs", kDefaultHoldAfterMs},
    };
    result["wayland"] = {
        {"portalScreenCast", wayland_ && DCU_HAVE_GIO && DCU_HAVE_PIPEWIRE},
        {"portalRemoteDesktop", wayland_ && DCU_HAVE_GIO},
        {"libei", wayland_ && libei_runtime_available()},
        {"legacyNotifyFallback", wayland_ && DCU_HAVE_GIO},
    };
    result["indicatorRequired"] = true;
    result["stopKey"] = "Escape x2";
    result["hotkeyStop"] = "Escape x2";
    result["supported"] = indicator_available &&
                           ((x11_ && DCU_HAVE_XTEST && DCU_HAVE_JPEG) ||
                            (wayland_ && DCU_HAVE_GIO && DCU_HAVE_PIPEWIRE));
    return result;
}

// A transient-for parent can be a group leader, an unmapped window, or a
// filtered shell window; only a listed owner is a targetable id.
void drop_unlisted_owners(std::vector<WindowInfo>& windows) {
    for (auto& window : windows) {
        if (window.owner_id.empty()) continue;
        const bool listed = std::any_of(windows.begin(), windows.end(), [&](const WindowInfo& other) {
            return other.id == window.owner_id;
        });
        if (!listed) {
            window.owner_id.clear();
            window.modal = false;
        }
    }
}

std::vector<WindowInfo> LinuxBackend::extension_windows() const {
#if DCU_HAVE_GIO
    if (!shell_bus_ || !shell_bus_->available()) return {};
    try {
        const Json payload = shell_bus_->list_windows();
        if (!payload.is_array()) return {};
        std::vector<WindowInfo> windows;
        for (const auto& item : payload) {
            WindowInfo window;
            window.id = item.value("id", "");
            window.title = item.value("title", "");
            window.app = item.value("app", "");
            window.pid = item.value("pid", 0);
            window.x = item.value("x", 0);
            window.y = item.value("y", 0);
            window.width = item.value("width", 0);
            window.height = item.value("height", 0);
            window.active = item.value("active", false);
            window.owner_id = item.value("ownerWindowId", "");
            window.modal = !window.owner_id.empty() && item.value("modal", false);
            if (!window.id.empty() && window.width > 0 && window.height > 0) windows.push_back(std::move(window));
        }
        drop_unlisted_owners(windows);
        return windows;
    } catch (...) {
        return {};
    }
#else
    return {};
#endif
}

std::vector<WindowInfo> LinuxBackend::enumerate_windows() const {
    auto from_extension = extension_windows();
    if (!from_extension.empty()) return from_extension;
#if DCU_HAVE_X11
    if (!display_) return {};
    const Atom client_list = XInternAtom(display_, "_NET_CLIENT_LIST", False);
    const Atom active_atom = XInternAtom(display_, "_NET_ACTIVE_WINDOW", False);
    Window active = static_cast<Window>(x11_property_cardinal(display_, root_window_, active_atom));
    Atom actual_type = None;
    int actual_format = 0;
    unsigned long count = 0;
    unsigned long remaining = 0;
    unsigned char* data = nullptr;
    std::vector<Window> ids;
    if (client_list != None && XGetWindowProperty(display_, root_window_, client_list, 0, 4096, False,
                                                   XA_WINDOW, &actual_type, &actual_format, &count,
                                                   &remaining, &data) == Success && data) {
        const auto* windows = reinterpret_cast<Window*>(data);
        ids.assign(windows, windows + count);
        XFree(data);
    }
    if (ids.empty()) {
        Window root = 0, parent = 0;
        Window* children = nullptr;
        unsigned int count_children = 0;
        if (XQueryTree(display_, root_window_, &root, &parent, &children, &count_children)) {
            ids.assign(children, children + count_children);
            if (children) XFree(children);
        }
    }
    std::vector<WindowInfo> result;
    for (Window window : ids) {
        if (x11_is_viewable(display_, window)) result.push_back(x11_window_info(display_, root_window_, window, active));
    }
    drop_unlisted_owners(result);
    return result;
#else
    return {};
#endif
}

Json LinuxBackend::list_windows(const Json&) {
    Json result = Json::array();
    for (const auto& window : enumerate_windows()) result.push_back(window_json(window));
    return Json{{"windows", result}};
}

Json LinuxBackend::list_apps(const Json&) {
    std::map<std::string, Json> apps;
    for (const auto& window : enumerate_windows()) {
        Json& app = apps[window.app];
        if (app.is_null()) app = Json{{"app", window.app}, {"pid", window.pid}, {"windows", Json::array()}};
        app["windows"].push_back(window.id);
        if (!window.title.empty()) app["title"] = window.title;
    }
    Json result = Json::array();
    for (auto& [_, app] : apps) result.push_back(std::move(app));
    return Json{{"apps", result}};
}

WindowInfo LinuxBackend::select_window(const Json& params) const {
    const std::string requested_id = string_value(params, "windowId");
    const std::string requested_app = string_value(params, "app");
    const auto windows = enumerate_windows();
    if (!requested_id.empty()) {
        for (const auto& window : windows) if (window.id == requested_id) return window;
        throw Error("window_not_found", "windowId does not identify a visible window");
    }
    if (!requested_app.empty()) {
        const std::string needle = lower(requested_app);
        // An app selects its main window before its dialogs; an observation
        // of the main window reports a blocking modal.
        for (const bool owned : {false, true}) {
            for (const auto& window : windows) {
                if (window.owner_id.empty() == owned) continue;
                if (lower(window.app) == needle || lower(window.title) == needle ||
                    (window.pid > 0 && needle == "pid:" + std::to_string(window.pid))) return window;
            }
        }
        throw Error("window_not_found", "app does not identify a visible window");
    }
    for (const auto& window : windows) if (window.active) return window;
    if (!windows.empty()) return windows.front();
    throw Error("window_not_found", "No visible desktop windows were found");
}

std::optional<WindowInfo> LinuxBackend::blocking_modal(const WindowInfo& target) const {
    const auto windows = enumerate_windows();
    std::vector<WindowRelation> relations;
    relations.reserve(windows.size());
    for (const auto& window : windows) relations.push_back({window.id, window.owner_id, window.modal});
    const auto index = find_blocking_modal(relations, target.id);
    if (!index) return std::nullopt;
    return windows[*index];
}

void LinuxBackend::reject_blocked_target(const Json& params) const {
    const WindowInfo target = select_window(params);
    if (const auto modal = blocking_modal(target)) {
        throw Error("modal_active",
                    "Window " + target.id + " is blocked by modal dialog " + modal->id + " \"" +
                        modal->title + "\"; observe and act on --window-id " + modal->id + " instead");
    }
}

WindowInfo LinuxBackend::current_window(const WindowInfo& expected) const {
    for (const auto& window : enumerate_windows()) if (window.id == expected.id) return window;
    throw Error("window_not_found", "The target window no longer exists");
}

void LinuxBackend::validate_observation(const Json& params, const WindowInfo& window) {
    refresh_coordinate_state();
    const std::string id = string_value(params, "observationId");
    if (id.empty()) return;
    const auto found = observations_.find(id);
    if (found == observations_.end()) throw Error("stale_observation", "Unknown observationId; observe again");
    const auto& old = found->second.window;
    if (old.id != window.id || old.x != window.x || old.y != window.y ||
        old.width != window.width || old.height != window.height) {
        throw Error("stale_observation", "Target window geometry changed; observe again");
    }
    if (found->second.coordinate_revision != coordinate_revision_) {
        throw Error("stale_observation", "Display geometry or capture mapping changed; observe again");
    }
}

namespace {
void remove_observation_files(const ObservationRecord& record) noexcept {
    for (const auto* path : {&record.reduced.path, &record.full.path}) {
        if (!path->empty()) ::unlink(path->c_str());
    }
}

Json action_transform(const ObservationImage& image) {
    return Json{{"scaleX", image.scale_x}, {"scaleY", image.scale_y}, {"offsetX", 0}, {"offsetY", 0}};
}
} // namespace

void LinuxBackend::store_observation(ObservationRecord record) {
    const std::string id = record.id;
    observations_[id] = std::move(record);
    // Evict the oldest observations first; their screenshots go with them.
    while (observations_.size() > 32) {
        const auto oldest = std::min_element(
            observations_.begin(), observations_.end(),
            [](const auto& a, const auto& b) { return a.second.created < b.second.created; });
        remove_observation_files(oldest->second);
        observations_.erase(oldest);
    }
}

void LinuxBackend::clear_observations() noexcept {
    for (const auto& [_, record] : observations_) remove_observation_files(record);
    observations_.clear();
}

CoordinateTransform LinuxBackend::coordinate_transform(const Json& params, const WindowInfo& window) const {
    const bool full = string_value(params, "coords", "reduced") == "full";
    const ObservationRecord* source = nullptr;
    const std::string id = string_value(params, "observationId");
    if (!id.empty()) {
        // validate_observation has already matched this id to the window.
        const auto found = observations_.find(id);
        if (found == observations_.end()) throw Error("stale_observation", "Unknown observationId; observe again");
        source = &found->second;
    } else {
        // Without an observationId, pixel coordinates refer to the most recent
        // screenshot of this window.
        for (const auto& [_, candidate] : observations_) {
            if (candidate.has_screenshot && candidate.window.id == window.id &&
                (!source || candidate.created > source->created)) {
                source = &candidate;
            }
        }
        if (source && (source->window.width != window.width || source->window.height != window.height ||
                       source->coordinate_revision != coordinate_revision_)) {
            throw Error("stale_observation", "Window geometry changed since the last screenshot; observe again");
        }
    }
    if (!source || !source->has_screenshot) {
        throw Error("observation_required", "Observe the window with a screenshot before clicking by coordinates");
    }
    return CoordinateTransform{full ? "full" : "reduced", full ? source->full : source->reduced};
}

Json LinuxBackend::full_screenshot(const Json& params) {
    ensure_active(params);
    refresh_coordinate_state();
    const std::string id = string_value(params, "observationId");
    if (id.empty()) throw Error("observation_required", "get-full-screenshot requires observationId");
    const auto found = observations_.find(id);
    if (found == observations_.end()) throw Error("stale_observation", "Unknown or expired observationId; observe again");
    const auto& record = found->second;
    const std::string window_id = string_value(params, "windowId");
    if (!window_id.empty() && window_id != record.window.id) {
        throw Error("stale_observation", "Observation belongs to a different window; observe again");
    }
    if (!record.has_screenshot || ::access(record.full.path.c_str(), R_OK) != 0) {
        throw Error("stale_observation", "Observation has no full screenshot; observe again with a screenshot");
    }
    return Json{{"observationId", id},
                {"screenshot", {{"path", record.full.path},
                                {"mimeType", record.mime},
                                {"width", record.full.width},
                                {"height", record.full.height},
                                {"variant", "full"},
                                {"actionTransform", action_transform(record.full)}}},
                {"notice", "To click a point read from this image, pass --coords full."}};
}

Json LinuxBackend::session_start(const Json& params, Context& context) {
    if (session_active_.load(std::memory_order_acquire)) {
        return Json{{"sessionId", session_id_}, {"ready", true}, {"reused", true},
                    {"indicator", {{"ready", true}, {"sessionId", session_id_}}}};
    }
    check_context(context);

    // Escape or a disconnected session bus can end a previous session while
    // the common dispatcher is between requests.  Join both monitor threads
    // before changing the session pointer so a late callback cannot cancel a
    // newly started session.
    heartbeat_stop_.store(true, std::memory_order_release);
#if DCU_HAVE_GIO
    stop_stop_monitor();
#endif
    if (heartbeat_thread_.joinable()) heartbeat_thread_.join();

    // A stop signal can make the session inactive before the dispatcher gets
    // a chance to run session.stop.  Tear down that old authorization before
    // starting a new one; otherwise the next session could reuse an EIS fd or
    // ScreenCast stream that belongs to the previous indicator session.
    release_inputs();
#if DCU_HAVE_PIPEWIRE && DCU_HAVE_GIO
    pipewire_capture_.reset();
#endif
#if DCU_HAVE_GIO
    close_portal_sessions();
#endif

    const std::string requested_id = string_value(params, "sessionId", uuid());
    if (requested_id.empty()) throw Error("invalid_argument", "sessionId must not be empty");

#if DCU_HAVE_GIO
    if (!shell_bus_ || !shell_bus_->available()) {
        throw Error("setup_required", "Enable the GNOME desktop-computer-use extension in the logged-in session");
    }
    const Json indicator = shell_bus_->call_string("Start", requested_id, 5000);
    if (!indicator.value("ready", false)) {
        throw Error("setup_required", "GNOME extension did not report a ready indicator");
    }
#else
    throw Error("setup_required", "This build has no D-Bus support for the GNOME indicator extension");
#endif

    session_id_ = requested_id;
    active_context_.store(&context, std::memory_order_release);
    session_active_.store(true, std::memory_order_release);
    interrupted_.store(false, std::memory_order_release);
    last_activity_ = Clock::now();
    heartbeat_stop_.store(false, std::memory_order_release);
    heartbeat_thread_ = std::thread([this] {
        while (!heartbeat_stop_.load(std::memory_order_acquire)) {
            std::this_thread::sleep_for(std::chrono::milliseconds(1500));
            if (heartbeat_stop_.load(std::memory_order_acquire)) break;
#if DCU_HAVE_GIO
            if (session_active_.load(std::memory_order_acquire) && shell_bus_) {
                try {
                    if (!shell_bus_->call_bool("Heartbeat", 1500)) {
                        session_active_.store(false, std::memory_order_release);
                        if (auto* context = active_context_.load(std::memory_order_acquire)) {
                            context->cancelled.store(true, std::memory_order_release);
                        }
                        heartbeat_stop_.store(true, std::memory_order_release);
#if DCU_HAVE_GIO
                        stop_monitor_stop_.store(true, std::memory_order_release);
#endif
                    }
                } catch (...) {
                    session_active_.store(false, std::memory_order_release);
                    if (auto* context = active_context_.load(std::memory_order_acquire)) {
                        context->cancelled.store(true, std::memory_order_release);
                    }
                    heartbeat_stop_.store(true, std::memory_order_release);
#if DCU_HAVE_GIO
                    stop_monitor_stop_.store(true, std::memory_order_release);
#endif
                }
            }
#endif
        }
    });
#if DCU_HAVE_GIO
    start_stop_monitor();
#endif
    return Json{{"sessionId", session_id_},
                {"ready", true},
                {"indicator", indicator},
                {"platform", wayland_ ? "wayland" : (x11_ ? "x11" : "unknown")}};
}

Json LinuxBackend::session_stop(const Json& params) {
    const std::string requested = string_value(params, "sessionId");
    if (!requested.empty() && requested != session_id_) throw Error("session_owner", "Unknown sessionId");
    heartbeat_stop_.store(true, std::memory_order_release);
#if DCU_HAVE_GIO
    stop_stop_monitor();
#endif
    if (heartbeat_thread_.joinable()) heartbeat_thread_.join();
    release_inputs();
#if DCU_HAVE_PIPEWIRE && DCU_HAVE_GIO
    pipewire_capture_.reset();
#endif
#if DCU_HAVE_GIO
    close_portal_sessions();
    if (shell_bus_ && shell_bus_->available()) {
        try { shell_bus_->call_bool("Stop", 3000); } catch (...) { }
    }
#endif
    const std::string old_session = std::exchange(session_id_, "");
    session_active_.store(false, std::memory_order_release);
    active_context_.store(nullptr, std::memory_order_release);
    interrupted_.store(false, std::memory_order_release);
    clear_observations();
    return Json{{"stopped", true}, {"sessionId", old_session}};
}

#if DCU_HAVE_GIO
GVariant* empty_options() {
    GVariantBuilder builder;
    g_variant_builder_init(&builder, G_VARIANT_TYPE_VARDICT);
    return g_variant_builder_end(&builder);
}

std::string variant_string(GVariant* dictionary, const char* key) {
    if (!dictionary || !key) return {};
    GVariantRef value(g_variant_lookup_value(dictionary, key, nullptr));
    if (!value.get()) return {};
    const GVariantType* type = g_variant_get_type(value.get());
    if (!g_variant_type_equal(type, G_VARIANT_TYPE_STRING) &&
        !g_variant_type_equal(type, G_VARIANT_TYPE_OBJECT_PATH)) return {};
    const gchar* text = g_variant_get_string(value.get(), nullptr);
    return text ? std::string(text) : std::string{};
}

PortalStreamInfo first_stream_info(GVariant* values) {
    PortalStreamInfo result;
    if (!values) return result;
    GVariantRef streams(g_variant_lookup_value(values, "streams", G_VARIANT_TYPE("a(ua{sv})")));
    if (!streams.get()) return result;
    GVariantIter iterator;
    g_variant_iter_init(&iterator, streams.get());
    guint32 node = UINT32_MAX;
    GVariant* properties = nullptr;
    if (!g_variant_iter_next(&iterator, "(u@a{sv})", &node, &properties) || !properties) return result;
    // A response can contain an empty or sentinel stream tuple when the
    // compositor denied capture.  Such a tuple must never make the backend
    // advertise a started stream.
    result.valid = node != UINT32_MAX;
    if (!result.valid) {
        g_variant_unref(properties);
        return result;
    }
    result.node_id = node;
    GVariantRef serial(g_variant_lookup_value(properties, "pipewire-serial", G_VARIANT_TYPE_UINT64));
    if (serial.get()) result.pipewire_serial = g_variant_get_uint64(serial.get());
    result.mapping_id = variant_string(properties, "mapping_id");
    GVariantRef position(g_variant_lookup_value(properties, "position", G_VARIANT_TYPE("(ii)")));
    if (position.get()) g_variant_get(position.get(), "(ii)", &result.position_x, &result.position_y);
    GVariantRef size(g_variant_lookup_value(properties, "size", G_VARIANT_TYPE("(ii)")));
    if (size.get()) g_variant_get(size.get(), "(ii)", &result.compositor_width, &result.compositor_height);
    GVariantRef logical_size(g_variant_lookup_value(properties, "logical_size", G_VARIANT_TYPE("(ii)")));
    if (logical_size.get()) g_variant_get(logical_size.get(), "(ii)", &result.logical_width, &result.logical_height);
    g_variant_unref(properties);
    return result;
}

void LinuxBackend::close_portal_sessions() noexcept {
    // Reset every piece of session geometry even when D-Bus has already
    // disappeared.  Keeping stale EIS or stream state would make a later
    // session send coordinates through an authorization that no longer owns
    // the desktop.
    if (eis_) {
        eis_->close();
        eis_.reset();
    }
    if (portal_bus_ && portal_bus_->connection() && !remote_desktop_session_.empty()) {
        portal_bus_->close_session(remote_desktop_session_);
    }
    remote_desktop_session_.clear();
    remote_desktop_started_ = false;
    remote_stream_valid_ = false;
    eis_session_selected_ = false;
    remote_stream_id_ = UINT32_MAX;
    remote_stream_serial_ = 0;
    remote_mapping_id_.clear();
    remote_stream_x_ = remote_stream_y_ = 0;
    remote_stream_width_ = remote_stream_height_ = 0;
    remote_logical_width_ = remote_logical_height_ = 0;
}
#endif

bool LinuxBackend::portal_ready_for_input(Context& context) {
#if !DCU_HAVE_GIO
    (void)context;
    if (wayland_) throw Error("unsupported", "Wayland input requires GIO portal support");
    return false;
#else
    check_context(context);
    if (!wayland_) return true;
    if (remote_desktop_started_ && remote_stream_valid_ && remote_stream_id_ != UINT32_MAX) {
        if (eis_session_selected_ && (!eis_ || !eis_->connected())) {
            throw Error("eis_disconnected", "The authorized EIS input session disconnected");
        }
        return true;
    }
    if (!remote_desktop_session_.empty() || eis_) close_portal_sessions();
    if (!portal_bus_ || !portal_bus_->available()) {
        throw Error("setup_required", "Wayland RemoteDesktop portal is unavailable");
    }

    // Keep all response-derived values local until the complete portal and
    // input transport setup succeeds.  This makes a denied source, cancelled
    // permission request, or failed EIS handshake leave no usable-looking
    // partial session behind.
    std::string new_session;
    PortalStreamInfo new_stream;
    std::unique_ptr<EisClient> new_eis;
    bool new_eis_selected = false;
    try {
        const std::string token = "dcu_" + uuid();
        GVariantBuilder create_options;
        g_variant_builder_init(&create_options, G_VARIANT_TYPE_VARDICT);
        g_variant_builder_add(&create_options, "{sv}", "handle_token", g_variant_new_string(token.c_str()));
        portal_bus_->request_dict("org.freedesktop.portal.RemoteDesktop", "CreateSession",
                                  g_variant_new("(a{sv})", &create_options), &context,
                                  [&new_session](GVariant* values) {
                                      new_session = variant_string(values, "session_handle");
                                  });
        if (new_session.empty()) throw Error("protocol_error", "RemoteDesktop returned no session handle");

        GVariantBuilder select_options;
        g_variant_builder_init(&select_options, G_VARIANT_TYPE_VARDICT);
        g_variant_builder_add(&select_options, "{sv}", "types", g_variant_new_uint32(3));
        portal_bus_->request_dict("org.freedesktop.portal.RemoteDesktop", "SelectDevices",
                                  g_variant_new("(oa{sv})", new_session.c_str(), &select_options),
                                  &context, [](GVariant*) {});

        // The ScreenCast source selection is tied to the same portal session,
        // so the returned stream and RemoteDesktop input transport describe
        // one coordinate space.
        GVariantBuilder sources;
        g_variant_builder_init(&sources, G_VARIANT_TYPE_VARDICT);
        g_variant_builder_add(&sources, "{sv}", "types", g_variant_new_uint32(3));
        g_variant_builder_add(&sources, "{sv}", "multiple", g_variant_new_boolean(FALSE));
        g_variant_builder_add(&sources, "{sv}", "cursor_mode", g_variant_new_uint32(2));
        portal_bus_->request_dict("org.freedesktop.portal.ScreenCast", "SelectSources",
                                  g_variant_new("(oa{sv})", new_session.c_str(), &sources),
                                  &context, [](GVariant*) {});

        portal_bus_->request_dict("org.freedesktop.portal.RemoteDesktop", "Start",
                                  g_variant_new("(os@a{sv})", new_session.c_str(), "",
                                                empty_options()), &context,
                                  [&new_stream](GVariant* values) {
                                      new_stream = first_stream_info(values);
                                  });
        if (!new_stream.valid || new_stream.node_id == UINT32_MAX) {
            throw Error("capture_unavailable", "RemoteDesktop returned no valid ScreenCast stream");
        }

        // EIS is preferred when both the portal and the runtime expose it.
        // A failed ConnectToEIS request means no EIS fd was authorized, so
        // the legacy Notify* transport remains valid.  Once an fd is returned,
        // however, a handshake failure is fatal and cannot fall back safely.
        bool portal_supports_eis = false;
        try {
            portal_supports_eis = portal_bus_->interface_version(
                "org.freedesktop.portal.RemoteDesktop", &context) >= 2;
        } catch (const Error& error) {
            if (error.code == "cancelled") throw;
            portal_supports_eis = false;
        }
        if (libei_runtime_available() && portal_supports_eis) {
            int eis_fd = -1;
            try {
                eis_fd = portal_bus_->connect_to_eis(new_session, &context);
            } catch (const Error& error) {
                if (error.code == "cancelled") throw;
                // No descriptor was returned, so this is an older or
                // unavailable EIS portal and Notify* is the supported path.
            }
            if (eis_fd >= 0) {
                new_eis = std::make_unique<EisClient>();
                try {
                    new_eis->connect(eis_fd, context);
                } catch (...) {
                    ::close(eis_fd);
                    new_eis->close();
                    throw;
                }
                ::close(eis_fd);
                new_eis_selected = true;
            }
        }

        remote_desktop_session_ = std::move(new_session);
        remote_desktop_started_ = true;
        remote_stream_valid_ = new_stream.valid;
        remote_stream_id_ = new_stream.node_id;
        remote_stream_serial_ = new_stream.pipewire_serial;
        remote_mapping_id_ = std::move(new_stream.mapping_id);
        remote_stream_x_ = new_stream.position_x;
        remote_stream_y_ = new_stream.position_y;
        remote_stream_width_ = new_stream.compositor_width;
        remote_stream_height_ = new_stream.compositor_height;
        remote_logical_width_ = new_stream.logical_width;
        remote_logical_height_ = new_stream.logical_height;
        eis_ = std::move(new_eis);
        eis_session_selected_ = new_eis_selected;
        return true;
    } catch (...) {
        if (new_eis) new_eis->close();
        if (!new_session.empty()) portal_bus_->close_session(new_session);
        remote_desktop_session_.clear();
        remote_desktop_started_ = false;
        remote_stream_valid_ = false;
        remote_stream_id_ = UINT32_MAX;
        remote_stream_serial_ = 0;
        remote_mapping_id_.clear();
        remote_stream_x_ = remote_stream_y_ = 0;
        remote_stream_width_ = remote_stream_height_ = 0;
        remote_logical_width_ = remote_logical_height_ = 0;
        eis_session_selected_ = false;
        if (eis_) {
            eis_->close();
            eis_.reset();
        }
        throw;
    }
#endif
}

bool LinuxBackend::portal_notify_motion_absolute(double x, double y, const WindowInfo* target, Context& context) {
#if !DCU_HAVE_GIO
    (void)x; (void)y; (void)target; (void)context;
    return false;
#else
    check_context(context);
    if (!portal_ready_for_input(context)) return false;
    if (eis_session_selected_) {
        throw Error("eis_disconnected", "The authorized EIS session must carry absolute pointer input");
    }
    if (!remote_stream_valid_ || remote_stream_id_ == UINT32_MAX) {
        throw Error("capture_unavailable", "Wayland absolute input has no ScreenCast stream");
    }
    double stream_x = x - remote_stream_x_;
    double stream_y = y - remote_stream_y_;
    if (remote_stream_width_ <= 0 || remote_stream_height_ <= 0) {
        if (target) {
            stream_x = x - target->x;
            stream_y = y - target->y;
        }
    }
    const double logical_width = remote_logical_width_ > 0
        ? remote_logical_width_
        : (remote_stream_width_ > 0 ? remote_stream_width_ : (target ? target->width : 0));
    const double logical_height = remote_logical_height_ > 0
        ? remote_logical_height_
        : (remote_stream_height_ > 0 ? remote_stream_height_ : (target ? target->height : 0));
    if (target && remote_stream_width_ <= 0 && remote_stream_height_ <= 0) {
        stream_x = x - target->x;
        stream_y = y - target->y;
    } else if (remote_stream_width_ > 0 && remote_stream_height_ > 0 &&
               remote_logical_width_ > 0 && remote_logical_height_ > 0) {
        stream_x *= logical_width / remote_stream_width_;
        stream_y *= logical_height / remote_stream_height_;
    }
    const double normalized_x = logical_width > 0.0
        ? std::clamp(stream_x / logical_width, 0.0, std::nextafter(1.0, 0.0)) : 0.0;
    const double normalized_y = logical_height > 0.0
        ? std::clamp(stream_y / logical_height, 0.0, std::nextafter(1.0, 0.0)) : 0.0;
    GErrorRef error;
    GVariantRef reply(g_dbus_connection_call_sync(
        portal_bus_->connection(), "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.RemoteDesktop", "NotifyPointerMotionAbsolute",
         g_variant_new("(o@a{sv}udd)", remote_desktop_session_.c_str(), empty_options(),
                       remote_stream_id_, normalized_x, normalized_y),
        G_VARIANT_TYPE("()"), G_DBUS_CALL_FLAGS_NONE, 2000, nullptr, error.out()));
    if (!reply.get()) throw Error("input_unavailable", error.message());
    return true;
#endif
}

bool LinuxBackend::portal_notify_button(const std::string& button, bool pressed, Context& context) {
#if !DCU_HAVE_GIO
    (void)button; (void)pressed; (void)context;
    return false;
#else
    check_context(context);
    if (!portal_ready_for_input(context)) return false;
    if (eis_session_selected_) {
        throw Error("eis_disconnected", "The authorized EIS session must carry button input");
    }
    // RemoteDesktop uses Linux evdev button codes, not X11 button numbers.
    const std::uint32_t code = button == "left" ? 272U : (button == "middle" ? 274U : 273U);
    const std::uint32_t state = pressed ? 1U : 0U;
    GErrorRef error;
    GVariantRef reply(g_dbus_connection_call_sync(
        portal_bus_->connection(), "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.RemoteDesktop", "NotifyPointerButton",
        g_variant_new("(o@a{sv}iu)", remote_desktop_session_.c_str(), empty_options(), code, state),
        G_VARIANT_TYPE("()"), G_DBUS_CALL_FLAGS_NONE, 2000, nullptr, error.out()));
    if (!reply.get()) throw Error("input_unavailable", error.message());
    return true;
#endif
}

bool LinuxBackend::portal_notify_axis(double dx, double dy, Context& context) {
#if !DCU_HAVE_GIO
    (void)dx; (void)dy; (void)context;
    return false;
#else
    check_context(context);
    if (!portal_ready_for_input(context)) return false;
    if (eis_session_selected_) {
        throw Error("eis_disconnected", "The authorized EIS session must carry scroll input");
    }
    GErrorRef error;
    GVariantRef reply(g_dbus_connection_call_sync(
        portal_bus_->connection(), "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.RemoteDesktop", "NotifyPointerAxis",
        g_variant_new("(o@a{sv}dd)", remote_desktop_session_.c_str(), empty_options(), dx, dy),
        G_VARIANT_TYPE("()"), G_DBUS_CALL_FLAGS_NONE, 2000, nullptr, error.out()));
    if (!reply.get()) throw Error("input_unavailable", error.message());
    return true;
#endif
}

bool LinuxBackend::portal_notify_key(std::uint32_t key, bool pressed, Context& context) {
#if !DCU_HAVE_GIO
    (void)key; (void)pressed; (void)context;
    return false;
#else
    check_context(context);
    if (!portal_ready_for_input(context)) return false;
    if (eis_session_selected_) {
        throw Error("eis_disconnected", "The authorized EIS session must carry keyboard input");
    }
    GErrorRef error;
    GVariantRef reply(g_dbus_connection_call_sync(
        portal_bus_->connection(), "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.RemoteDesktop", "NotifyKeyboardKeysym",
        g_variant_new("(o@a{sv}iu)", remote_desktop_session_.c_str(), empty_options(), key, pressed ? 1U : 0U),
        G_VARIANT_TYPE("()"), G_DBUS_CALL_FLAGS_NONE, 2000, nullptr, error.out()));
    if (!reply.get()) throw Error("input_unavailable", error.message());
    return true;
#endif
}

void LinuxBackend::activate(const WindowInfo& window) {
#if DCU_HAVE_GIO
    if (shell_bus_ && shell_bus_->available()) {
        shell_bus_->activate(window.id);
        return;
    }
#endif
#if DCU_HAVE_X11
    if (display_ && window.id.rfind("x11:", 0) == 0) {
        const Window handle = static_cast<Window>(std::stoul(window.id.substr(4)));
        XRaiseWindow(display_, handle);
        XSetInputFocus(display_, handle, RevertToParent, CurrentTime);
        XFlush(display_);
        return;
    }
#endif
    throw Error("input_unavailable", "The target window could not be activated");
}

void LinuxBackend::move_pointer(int x, int y, Context& context, const WindowInfo* target) {
    check_context(context);
    if (wayland_) {
        portal_ready_for_input(context);
        if (eis_session_selected_) {
            if (!eis_ || !eis_->connected()) {
                throw Error("eis_disconnected", "The authorized EIS input session disconnected");
            }
            eis_->absolute_global(static_cast<double>(x), static_cast<double>(y), context);
        } else if (eis_ && eis_->connected()) {
            throw Error("protocol_error", "Connected EIS transport is missing its session selection state");
        } else {
            portal_notify_motion_absolute(static_cast<double>(x), static_cast<double>(y), target, context);
        }
#if DCU_HAVE_GIO
        if (shell_bus_ && shell_bus_->available()) shell_bus_->pointer(x, y);
#endif
        mark_input_complete();
        return;
    }
#if DCU_HAVE_X11 && DCU_HAVE_XTEST
    if (!display_) throw Error("input_unavailable", "X11 display is unavailable");
    XTestFakeMotionEvent(display_, -1, x, y, CurrentTime);
    XFlush(display_);
    mark_input_complete();
    return;
#else
    (void)x; (void)y;
    throw Error("unsupported", "X11 input requires the XTest extension");
#endif
}

void LinuxBackend::button_event(const std::string& button, bool pressed, Context& context) {
    if (pressed) check_context(context);
    if (wayland_) {
        portal_ready_for_input(context);
        if (eis_session_selected_) {
            if (!eis_ || !eis_->connected()) {
                throw Error("eis_disconnected", "The authorized EIS input session disconnected");
            }
            const std::uint32_t code = button == "left" ? 272U : (button == "middle" ? 274U : 273U);
            eis_->button(code, pressed, context);
        } else if (eis_ && eis_->connected()) {
            throw Error("protocol_error", "Connected EIS transport is missing its session selection state");
        } else {
            portal_notify_button(button, pressed, context);
        }
        mark_input_complete();
        return;
    }
#if DCU_HAVE_X11 && DCU_HAVE_XTEST
    if (!display_) throw Error("input_unavailable", "X11 display is unavailable");
    const unsigned int code = button == "left" ? 1U : (button == "middle" ? 2U : 3U);
    XTestFakeButtonEvent(display_, code, pressed ? True : False, CurrentTime);
    XFlush(display_);
    mark_input_complete();
#else
    (void)button; (void)pressed;
    throw Error("unsupported", "X11 input requires the XTest extension");
#endif
}

void LinuxBackend::scroll_event(int amount, const std::string& direction, Context& context) {
    check_context(context);
    const int sign = direction == "up" || direction == "left" ? -1 : 1;
    const bool horizontal = direction == "left" || direction == "right";
    const int count = std::max(1, std::abs(amount));
    for (int i = 0; i < count; ++i) {
        check_context(context);
        if (wayland_) {
            portal_ready_for_input(context);
            if (eis_session_selected_) {
                if (!eis_ || !eis_->connected()) {
                    throw Error("eis_disconnected", "The authorized EIS input session disconnected");
                }
                eis_->scroll(horizontal ? sign : 0.0, horizontal ? 0.0 : sign, context);
            } else if (eis_ && eis_->connected()) {
                throw Error("protocol_error", "Connected EIS transport is missing its session selection state");
            } else {
                portal_notify_axis(horizontal ? sign : 0.0, horizontal ? 0.0 : sign, context);
            }
        } else {
#if DCU_HAVE_X11 && DCU_HAVE_XTEST
            const unsigned int code = horizontal ? (sign < 0 ? 6U : 7U) : (sign < 0 ? 4U : 5U);
            XTestFakeButtonEvent(display_, code, True, CurrentTime);
            XTestFakeButtonEvent(display_, code, False, CurrentTime);
            XFlush(display_);
#else
            throw Error("unsupported", "X11 input requires the XTest extension");
#endif
        }
        mark_input_complete();
        std::this_thread::sleep_for(std::chrono::milliseconds(30));
    }
}

std::string LinuxBackend::button_name(const Json& params) {
    const std::string button = lower(string_value(params, "button", "left"));
    if (button != "left" && button != "right" && button != "middle") {
        throw Error("invalid_argument", "button must be left, right, or middle");
    }
    return button;
}

std::vector<std::string> LinuxBackend::key_list(const Json& params) {
    if (params.contains("keys")) {
        if (!params.at("keys").is_array()) throw Error("invalid_argument", "keys must be an array");
        std::vector<std::string> keys;
        for (const auto& key : params.at("keys")) {
            if (!key.is_string()) throw Error("invalid_argument", "keys must contain strings");
            keys.push_back(key.get<std::string>());
        }
        return keys;
    }
    const std::string hotkey = string_value(params, "key");
    if (hotkey.empty()) throw Error("invalid_argument", "key or keys is required");
    std::vector<std::string> keys;
    std::size_t start = 0;
    while (start <= hotkey.size()) {
        const std::size_t plus = hotkey.find('+', start);
        const std::string key = hotkey.substr(start, plus == std::string::npos ? std::string::npos : plus - start);
        if (key.empty()) throw Error("invalid_argument", "key contains an empty component");
        keys.push_back(key);
        if (plus == std::string::npos) break;
        start = plus + 1;
    }
    return keys;
}

std::string canonical_key(std::string key) {
    key = lower(std::move(key));
    if (key == "control" || key == "commandorcontrol" || key == "cmdorctrl") return "ctrl";
    if (key == "command" || key == "cmd" || key == "meta" || key == "win" || key == "super") return "super";
    if (key == "option") return "alt";
    if (key == "return") return "enter";
    if (key == "esc") return "escape";
    if (key == "page_up") return "pageup";
    if (key == "page_down") return "pagedown";
    return key;
}

#if DCU_HAVE_X11
KeySym x11_keysym(const std::string& raw) {
    const std::string key = canonical_key(raw);
    static const std::map<std::string, KeySym> aliases = {
        {"ctrl", XK_Control_L}, {"shift", XK_Shift_L}, {"alt", XK_Alt_L}, {"super", XK_Super_L},
        {"enter", XK_Return}, {"tab", XK_Tab}, {"escape", XK_Escape}, {"backspace", XK_BackSpace},
        {"delete", XK_Delete}, {"space", XK_space}, {"left", XK_Left}, {"right", XK_Right},
        {"up", XK_Up}, {"down", XK_Down}, {"home", XK_Home}, {"end", XK_End},
        {"insert", XK_Insert}, {"pageup", XK_Page_Up}, {"pagedown", XK_Page_Down},
        {"pause", XK_Pause}, {"printscreen", XK_Print}, {"capslock", XK_Caps_Lock},
    };
    const auto found = aliases.find(key);
    if (found != aliases.end()) return found->second;
    if (key.size() == 1) return XStringToKeysym(key.c_str());
    KeySym value = XStringToKeysym(raw.c_str());
    if (value == NoSymbol) value = XStringToKeysym(key.c_str());
    return value;
}
#endif

std::uint32_t common_keysym(const std::string& raw) {
    const std::string key = canonical_key(raw);
    static const std::map<std::string, std::uint32_t> aliases = {
        {"ctrl", 0xffe3}, {"shift", 0xffe1}, {"alt", 0xffe9}, {"super", 0xffeb},
        {"enter", 0xff0d}, {"tab", 0xff09}, {"escape", 0xff1b}, {"backspace", 0xff08},
        {"delete", 0xffff}, {"space", 0x20}, {"left", 0xff51}, {"right", 0xff53},
        {"up", 0xff52}, {"down", 0xff54}, {"home", 0xff50}, {"end", 0xff57},
        {"insert", 0xff63}, {"pageup", 0xff55}, {"pagedown", 0xff56}, {"pause", 0xff13},
    };
    const auto found = aliases.find(key);
    if (found != aliases.end()) return found->second;
    if (key.size() == 1) return static_cast<std::uint8_t>(key[0]);
    return 0;
}

std::uint32_t evdev_keycode(const std::string& raw) {
    const std::string key = canonical_key(raw);
    static const std::map<std::string, std::uint32_t> aliases = {
        {"escape", 1}, {"1", 2}, {"2", 3}, {"3", 4}, {"4", 5}, {"5", 6},
        {"6", 7}, {"7", 8}, {"8", 9}, {"9", 10}, {"0", 11}, {"minus", 12},
        {"=", 13}, {"backspace", 14}, {"tab", 15}, {"q", 16}, {"w", 17},
        {"e", 18}, {"r", 19}, {"t", 20}, {"y", 21}, {"u", 22}, {"i", 23},
        {"o", 24}, {"p", 25}, {"[", 26}, {"]", 27}, {"enter", 28}, {"ctrl", 29},
        {"a", 30}, {"s", 31}, {"d", 32}, {"f", 33}, {"g", 34}, {"h", 35},
        {"j", 36}, {"k", 37}, {"l", 38}, {";", 39}, {"'", 40}, {"`", 41},
        {"shift", 42}, {"\\", 43}, {"z", 44}, {"x", 45}, {"c", 46}, {"v", 47},
        {"b", 48}, {"n", 49}, {"m", 50}, {",", 51}, {".", 52}, {"/", 53},
        {"alt", 56}, {"space", 57}, {"capslock", 58}, {"f1", 59}, {"f2", 60},
        {"f3", 61}, {"f4", 62}, {"f5", 63}, {"f6", 64}, {"f7", 65}, {"f8", 66},
        {"f9", 67}, {"f10", 68}, {"numlock", 69}, {"scrolllock", 70},
        {"f11", 87}, {"f12", 88}, {"rightctrl", 97}, {"rightalt", 100},
        {"home", 102}, {"up", 103}, {"pageup", 104}, {"left", 105}, {"right", 106},
        {"end", 107}, {"down", 108}, {"pagedown", 109}, {"insert", 110},
        {"delete", 111}, {"pause", 119}, {"super", 125},
    };
    const auto found = aliases.find(key);
    return found == aliases.end() ? 0U : found->second;
}

void LinuxBackend::suspend_stop_key_for_escape() {
    // GNOME's stop accelerator cannot tell injected input from the keyboard,
    // so an injected Escape would be swallowed and counted toward the user's
    // emergency stop. Refuse the action rather than risk either outcome.
    std::string reason = "the GNOME extension is unavailable";
#if DCU_HAVE_GIO
    if (shell_bus_) {
        try {
            if (shell_bus_->suspend_stop_key(kStopKeySuspendMs)) return;
            reason = "the GNOME extension reported no active session";
        } catch (const std::exception& error) {
            reason = error.what();
        }
    }
#endif
    throw Error("stop_key_conflict",
                "Escape is the emergency stop key and could not be released to the application ("
                + reason + "); run `dcu setup` to update the GNOME extension, then log in again "
                "so GNOME Shell loads it");
}

void LinuxBackend::key_event(const std::string& key, bool pressed, Context& context) {
    if (pressed) check_context(context);
    const std::string canonical = canonical_key(key);
    const std::uint32_t keysym = common_keysym(canonical);
    // Suspend immediately before injection so portal setup cannot consume the
    // suspension window.
    const bool injects_escape_press = pressed && canonical == "escape";
    if (wayland_) {
        portal_ready_for_input(context);
        if (eis_session_selected_) {
            if (!eis_ || !eis_->connected()) {
                throw Error("eis_disconnected", "The authorized EIS input session disconnected");
            }
            const std::uint32_t code = evdev_keycode(canonical);
            if (!code) throw Error("invalid_argument", "Unknown key: " + key);
            if (injects_escape_press) suspend_stop_key_for_escape();
            eis_->key(code, pressed, context);
        } else if (eis_ && eis_->connected()) {
            throw Error("protocol_error", "Connected EIS transport is missing its session selection state");
        } else {
            if (!keysym) throw Error("invalid_argument", "Unknown key: " + key);
            if (injects_escape_press) suspend_stop_key_for_escape();
            portal_notify_key(keysym, pressed, context);
        }
        mark_input_complete();
        return;
    }
#if DCU_HAVE_X11 && DCU_HAVE_XTEST
    if (!display_) throw Error("input_unavailable", "X11 display is unavailable");
    const KeySym symbol = x11_keysym(canonical);
    if (symbol == NoSymbol) throw Error("invalid_argument", "Unknown key: " + key);
    const KeyCode code = XKeysymToKeycode(display_, symbol);
    if (!code) throw Error("invalid_argument", "Key has no X11 keycode: " + key);
    if (injects_escape_press) suspend_stop_key_for_escape();
    XTestFakeKeyEvent(display_, code, pressed ? True : False, CurrentTime);
    XFlush(display_);
    mark_input_complete();
#else
    (void)key; (void)pressed;
    throw Error("unsupported", "X11 keyboard input requires the XTest extension");
#endif
}

void LinuxBackend::hotkey_event(const std::vector<std::string>& keys, Context& context) {
    if (keys.empty()) throw Error("invalid_argument", "hotkey requires at least one key");
    std::vector<std::string> modifiers;
    std::vector<std::string> regular;
    for (const auto& raw : keys) {
        const std::string key = canonical_key(raw);
        if (key == "ctrl" || key == "shift" || key == "alt" || key == "super") modifiers.push_back(key);
        else regular.push_back(key);
    }
    if (regular.size() != 1) throw Error("invalid_argument", "hotkey requires exactly one non-modifier key");
    require_not_toggled(regular.front());
    std::vector<std::string> pressed;
    Context release_context;
    try {
        for (const auto& modifier : modifiers) {
            if (toggled_.contains(modifier)) continue;
            key_event(modifier, true, context);
            pressed.push_back(modifier);
        }
        key_event(regular.front(), true, context);
        key_event(regular.front(), false, release_context);
        for (auto it = pressed.rbegin(); it != pressed.rend(); ++it) key_event(*it, false, release_context);
    } catch (...) {
        for (auto it = pressed.rbegin(); it != pressed.rend(); ++it) {
            try { key_event(*it, false, release_context); } catch (...) { }
        }
        throw;
    }
}

void LinuxBackend::type_text_impl(const std::string& text, Context& context) {
    if (!toggled_.empty()) {
        // Keycode typing is changed by any held key (Shift alters case, Ctrl,
        // Alt and Super turn text into shortcuts).
        std::string names;
        for (const auto& key : toggled_) names += (names.empty() ? "" : ", ") + (key == "super" ? std::string("win") : key);
        throw Error("toggles_active", "type-text is refused while " + names +
                                          " is toggled on; release it with `dcu toggle off --all` first or use paste-text");
    }
    for (unsigned char value : text) {
        check_context(context);
        if (value >= 0x20 && value <= 0x7e) {
            key_event(std::string(1, static_cast<char>(value)), true, context);
            key_event(std::string(1, static_cast<char>(value)), false, context);
        } else if (value == '\n') {
            key_event("enter", true, context);
            key_event("enter", false, context);
        } else if (value == '\t') {
            key_event("tab", true, context);
            key_event("tab", false, context);
        } else {
            throw Error("unsupported", "type-text currently accepts printable ASCII, tab, and newline; use paste-text for Unicode");
        }
    }
}

void LinuxBackend::release_inputs() noexcept {
    input_release_pending_.store(true, std::memory_order_release);
    // EIS tracks and releases every key it pressed, toggled keys included.
    if (eis_) eis_->release();
#if DCU_HAVE_GIO
    if (wayland_ && remote_desktop_started_ && !eis_session_selected_) {
        // The legacy Notify* transport keeps no key state of its own.
        for (const auto& key : toggled_) {
            try {
                Context release_context;
                portal_notify_key(common_keysym(key), false, release_context);
            } catch (...) { }
        }
    }
#endif
#if DCU_HAVE_X11 && DCU_HAVE_XTEST
    if (display_ && !wayland_) {
        for (unsigned int button = 1; button <= 3; ++button) XTestFakeButtonEvent(display_, button, False, CurrentTime);
        const std::array<KeySym, 4> modifiers{XK_Control_L, XK_Shift_L, XK_Alt_L, XK_Super_L};
        for (const KeySym symbol : modifiers) {
            const KeyCode code = XKeysymToKeycode(display_, symbol);
            if (code) XTestFakeKeyEvent(display_, code, False, CurrentTime);
        }
        if (toggled_.contains("space")) {
            const KeyCode code = XKeysymToKeycode(display_, XK_space);
            if (code) XTestFakeKeyEvent(display_, code, False, CurrentTime);
        }
        XFlush(display_);
    }
#endif
    toggled_.clear();
}

void LinuxBackend::release_toggled_keys() noexcept {
    for (const auto& key : toggled_) {
        try {
            Context release_context;
            key_event(key, false, release_context);
        } catch (...) { }
    }
    toggled_.clear();
}

void LinuxBackend::require_not_toggled(const std::string& key) const {
    const std::string canonical = canonical_key(key);
    if (toggled_.contains(canonical)) {
        const std::string name = canonical == "super" ? "win" : canonical;
        throw Error("toggles_active", name + " is toggled on; pressing it again would release it. "
                                      "Use `dcu toggle off --key " + name + "` instead");
    }
}

void LinuxBackend::set_toggle(const std::string& key, bool down, Context& context) {
    if (!session_active_.load(std::memory_order_acquire)) {
        throw Error("session_required", "Start a computer-use session before input actions");
    }
    if (interrupted_.load(std::memory_order_acquire)) throw Error("cancelled", "Session interrupted");
    const std::string canonical = canonical_key(key);
    if (!down) {
        Context release_context;
        key_event(canonical, false, release_context);
        toggled_.erase(canonical);
    } else {
        // Track before the press so every release path includes the key.
        toggled_.insert(canonical);
        try {
            key_event(canonical, true, context);
        } catch (...) {
            try {
                Context release_context;
                key_event(canonical, false, release_context);
            } catch (...) { }
            toggled_.erase(canonical);
            throw;
        }
    }
    last_activity_ = Clock::now();
}

void LinuxBackend::release_toggles() noexcept {
    release_toggled_keys();
}

namespace {
void wait_checked(Context& context, int milliseconds) {
    const int bounded = std::max(0, milliseconds);
    const auto deadline = Clock::now() + std::chrono::milliseconds(bounded);
    while (Clock::now() < deadline) {
        check_context(context);
        const auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(deadline - Clock::now());
        std::this_thread::sleep_for(std::min(std::chrono::milliseconds(10), std::max(std::chrono::milliseconds(1), remaining)));
    }
}

// x/y are pixels of the selected screenshot; the transform converts them to
// a window-local point.
std::pair<int, int> point_from_json(const Json& params, const char* x_key, const char* y_key,
                                    const WindowInfo& window, const CoordinateTransform& transform) {
    if (!params.contains(x_key) || !params.contains(y_key)) {
        throw Error("invalid_argument", std::string(x_key) + " and " + y_key + " are required");
    }
    const double x = number(params, x_key, 0.0);
    const double y = number(params, y_key, 0.0);
    const auto& image = transform.image;
    if (!std::isfinite(x) || !std::isfinite(y) || x < 0 || y < 0 ||
        x >= static_cast<double>(image.width) || y >= static_cast<double>(image.height)) {
        throw Error("invalid_argument", "Input coordinates must be finite and inside the " + transform.space +
                                            " screenshot");
    }
    const int local_x = std::clamp(static_cast<int>(std::lround(x / image.scale_x)), 0, std::max(0, window.width - 1));
    const int local_y = std::clamp(static_cast<int>(std::lround(y / image.scale_y)), 0, std::max(0, window.height - 1));
    return {local_x, local_y};
}

Json point_json(const std::pair<int, int>& point) {
    return Json{{"x", point.first}, {"y", point.second}};
}
} // namespace

Json LinuxBackend::click(const Json& params, Context& context) {
    ensure_active(params);
    WindowInfo window = select_window(params);
    window = current_window(window);
    validate_observation(params, window);
    std::optional<CoordinateTransform> transform;
    if (!params.contains("elementIndex")) transform = coordinate_transform(params, window);
    activate(window);
    std::pair<int, int> local;
#if DCU_HAVE_ATSPI
    if (params.contains("elementIndex")) {
        AtspiAccessible* element = atspi_element_for_index(window, params, context);
        const auto rect = atspi_rect(element);
        if (!rect) {
            g_object_unref(element);
            throw Error("element_not_found", "The selected accessibility element has no screen rectangle");
        }
        local = {rect->at(0) - window.x + rect->at(2) / 2, rect->at(1) - window.y + rect->at(3) / 2};
        g_object_unref(element);
    } else
#endif
    {
        if (!transform) transform = coordinate_transform(params, window);
        local = point_from_json(params, "x", "y", window, *transform);
    }
    const int x = window.x + local.first;
    const int y = window.y + local.second;
    move_pointer(x, y, context, &window);
    const auto keys = params.contains("modifiers") ? key_list(Json{{"key", string_value(params, "modifiers")}})
                                                    : std::vector<std::string>{};
    std::vector<std::string> held;
    try {
        for (const auto& modifier : keys) {
            // A toggled key is already down and must stay down after the click.
            if (toggled_.contains(canonical_key(modifier))) continue;
            key_event(modifier, true, context);
            held.push_back(modifier);
        }
        button_event(button_name(params), true, context);
        Context release_context;
        HeldButton guard([this, button = button_name(params), &release_context] {
            try { button_event(button, false, release_context); } catch (...) { }
        }, true);
        wait_checked(context, 30);
        button_event(button_name(params), false, release_context);
        guard.dismiss();
        for (auto it = held.rbegin(); it != held.rend(); ++it) key_event(*it, false, release_context);
    } catch (...) {
        Context release_context;
        for (auto it = held.rbegin(); it != held.rend(); ++it) {
            try { key_event(*it, false, release_context); } catch (...) { }
        }
        throw;
    }
    mark_input_complete();
    Json result = action_result();
    if (transform) {
        result["coordinateSpace"] = transform->space;
        result["windowPoint"] = point_json(local);
    }
    return result;
}

Json LinuxBackend::drag(const Json& params, Context& context) {
    ensure_active(params);
    WindowInfo window = current_window(select_window(params));
    validate_observation(params, window);
    std::optional<CoordinateTransform> transform;
    if (!params.contains("fromElementIndex") && !params.contains("toElementIndex")) {
        transform = coordinate_transform(params, window);
    }
    activate(window);
    std::pair<int, int> from;
    std::pair<int, int> to;
#if DCU_HAVE_ATSPI
    if (params.contains("fromElementIndex") || params.contains("toElementIndex")) {
        if (!params.contains("fromElementIndex") || !params.contains("toElementIndex")) {
            throw Error("invalid_argument", "fromElementIndex and toElementIndex must be supplied together");
        }
        Json from_params = params;
        from_params["elementIndex"] = params.at("fromElementIndex");
        Json to_params = params;
        to_params["elementIndex"] = params.at("toElementIndex");
        AtspiAccessible* from_element = atspi_element_for_index(window, from_params, context);
        AtspiAccessible* to_element = atspi_element_for_index(window, to_params, context);
        const auto from_rect = atspi_rect(from_element);
        const auto to_rect = atspi_rect(to_element);
        g_object_unref(from_element);
        g_object_unref(to_element);
        if (!from_rect || !to_rect) throw Error("element_not_found", "Drag elements have no screen rectangle");
        from = {from_rect->at(0) - window.x + from_rect->at(2) / 2,
                from_rect->at(1) - window.y + from_rect->at(3) / 2};
        to = {to_rect->at(0) - window.x + to_rect->at(2) / 2,
              to_rect->at(1) - window.y + to_rect->at(3) / 2};
    } else
#endif
    {
        transform = coordinate_transform(params, window);
        from = point_from_json(params, "fromX", "fromY", window, *transform);
        to = point_from_json(params, "toX", "toY", window, *transform);
    }
    const int duration = integer(params, "durationMs", kDefaultDragDurationMs);
    const int steps = integer(params, "steps", kDefaultDragSteps);
    const int hold_before = integer(params, "holdBeforeMs", kDefaultHoldBeforeMs);
    const int hold_after = integer(params, "holdAfterMs", kDefaultHoldAfterMs);
    if (duration < 0 || steps < 1 || hold_before < 0 || hold_after < 0) {
        throw Error("invalid_argument", "durationMs, steps, and hold values must be non-negative (steps >= 1)");
    }
    const std::string button = button_name(params);
    move_pointer(window.x + from.first, window.y + from.second, context, &window);
    Context release_context;
    // Modifiers are held for the whole drag. Declared before the button guard
    // so the button is released first, then the modifiers.
    std::vector<std::string> held_modifiers;
    HeldButton modifier_guard([this, &held_modifiers, &release_context] {
        for (auto it = held_modifiers.rbegin(); it != held_modifiers.rend(); ++it) {
            try { key_event(*it, false, release_context); } catch (...) { }
        }
    }, true);
    if (params.contains("modifiers")) {
        for (const auto& modifier : key_list(Json{{"key", string_value(params, "modifiers")}})) {
            if (toggled_.contains(canonical_key(modifier))) continue;
            key_event(modifier, true, context);
            held_modifiers.push_back(modifier);
        }
    }
    button_event(button, true, context);
    HeldButton guard([this, button, &release_context] {
        try { button_event(button, false, release_context); } catch (...) { }
    }, true);
    wait_checked(context, hold_before);
    for (int step = 1; step <= steps; ++step) {
        check_context(context);
        const double fraction = static_cast<double>(step) / steps;
        const int x = static_cast<int>(std::lround(from.first + (to.first - from.first) * fraction));
        const int y = static_cast<int>(std::lround(from.second + (to.second - from.second) * fraction));
        move_pointer(window.x + x, window.y + y, context, &window);
        const int elapsed = static_cast<int>(std::lround(static_cast<double>(duration) * step / steps));
        const int previous = static_cast<int>(std::lround(static_cast<double>(duration) * (step - 1) / steps));
        wait_checked(context, elapsed - previous);
    }
    wait_checked(context, hold_after);
    button_event(button, false, release_context);
    guard.dismiss();
    modifier_guard.reset();
    input_release_pending_.store(false, std::memory_order_release);
    mark_input_complete();
    Json result = action_result();
    if (transform) {
        result["coordinateSpace"] = transform->space;
        result["windowFrom"] = point_json(from);
        result["windowTo"] = point_json(to);
    }
    return result;
}

Json LinuxBackend::scroll(const Json& params, Context& context) {
    ensure_active(params);
    WindowInfo window = current_window(select_window(params));
    validate_observation(params, window);
    const auto transform = coordinate_transform(params, window);
    const auto point = point_from_json(params, "x", "y", window, transform);
    activate(window);
    move_pointer(window.x + point.first, window.y + point.second, context, &window);
    const std::string direction = lower(string_value(params, "direction"));
    if (direction != "up" && direction != "down" && direction != "left" && direction != "right") {
        throw Error("invalid_argument", "direction must be up, down, left, or right");
    }
    scroll_event(std::max(1, integer(params, "amount", 3)), direction, context);
    mark_input_complete();
    Json result = action_result();
    result["coordinateSpace"] = transform.space;
    result["windowPoint"] = point_json(point);
    return result;
}

Json LinuxBackend::type_text(const Json& params, Context& context) {
    ensure_active(params);
    WindowInfo window = current_window(select_window(params));
    validate_observation(params, window);
    activate(window);
    const std::string text = string_value(params, "text");
    if (text.empty()) throw Error("invalid_argument", "text is required");
    type_text_impl(text, context);
    mark_input_complete();
    return action_result();
}

Json LinuxBackend::press_key(const Json& params, Context& context) {
    ensure_active(params);
    WindowInfo window = current_window(select_window(params));
    validate_observation(params, window);
    activate(window);
    const std::string key = string_value(params, "key");
    if (key.empty()) throw Error("invalid_argument", "key is required");
    require_not_toggled(key);
    key_event(key, true, context);
    Context release_context;
    key_event(key, false, release_context);
    mark_input_complete();
    return action_result();
}

Json LinuxBackend::hotkey(const Json& params, Context& context) {
    ensure_active(params);
    WindowInfo window = current_window(select_window(params));
    validate_observation(params, window);
    activate(window);
    hotkey_event(key_list(params), context);
    mark_input_complete();
    return action_result();
}

namespace {
int run_with_stdin(const std::vector<std::string>& argv, const std::string& input) {
    if (argv.empty()) return -1;
    int pipe_fds[2];
    if (pipe(pipe_fds) != 0) return -1;
    const pid_t child = fork();
    if (child < 0) {
        close(pipe_fds[0]);
        close(pipe_fds[1]);
        return -1;
    }
    if (child == 0) {
        dup2(pipe_fds[0], STDIN_FILENO);
        close(pipe_fds[0]);
        close(pipe_fds[1]);
        std::vector<char*> args;
        args.reserve(argv.size() + 1);
        for (const auto& item : argv) args.push_back(const_cast<char*>(item.c_str()));
        args.push_back(nullptr);
        int devnull = open("/dev/null", O_WRONLY);
        if (devnull >= 0) dup2(devnull, STDERR_FILENO);
        execvp(args[0], args.data());
        _exit(127);
    }
    close(pipe_fds[0]);
    std::size_t offset = 0;
    while (offset < input.size()) {
        const ssize_t written = write(pipe_fds[1], input.data() + offset, input.size() - offset);
        if (written < 0 && errno == EINTR) continue;
        if (written <= 0) break;
        offset += static_cast<std::size_t>(written);
    }
    close(pipe_fds[1]);
    int status = 0;
    waitpid(child, &status, 0);
    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

std::optional<std::string> read_command(const std::vector<std::string>& argv) {
    if (argv.empty()) return std::nullopt;
    int pipe_fds[2];
    if (pipe(pipe_fds) != 0) return std::nullopt;
    const pid_t child = fork();
    if (child < 0) {
        close(pipe_fds[0]);
        close(pipe_fds[1]);
        return std::nullopt;
    }
    if (child == 0) {
        dup2(pipe_fds[1], STDOUT_FILENO);
        close(pipe_fds[0]);
        close(pipe_fds[1]);
        int devnull = open("/dev/null", O_WRONLY);
        if (devnull >= 0) dup2(devnull, STDERR_FILENO);
        std::vector<char*> args;
        args.reserve(argv.size() + 1);
        for (const auto& item : argv) args.push_back(const_cast<char*>(item.c_str()));
        args.push_back(nullptr);
        execvp(args[0], args.data());
        _exit(127);
    }
    close(pipe_fds[1]);
    std::string output;
    std::array<char, 4096> buffer{};
    while (output.size() < 1024 * 1024) {
        const ssize_t count = read(pipe_fds[0], buffer.data(), buffer.size());
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) break;
        output.append(buffer.data(), static_cast<std::size_t>(count));
    }
    close(pipe_fds[0]);
    int status = 0;
    waitpid(child, &status, 0);
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) return std::nullopt;
    return output;
}

std::optional<std::string> read_clipboard_value(bool wayland) {
    if (wayland && command_exists("wl-paste")) return read_command({"wl-paste", "--no-newline"});
    if (command_exists("xclip")) return read_command({"xclip", "-selection", "clipboard", "-o"});
    if (command_exists("xsel")) return read_command({"xsel", "--clipboard", "--output"});
    if (command_exists("wl-paste")) return read_command({"wl-paste", "--no-newline"});
    return std::nullopt;
}

void write_clipboard_value(bool wayland, const std::string& text) {
    if (wayland && command_exists("wl-copy")) {
        if (run_with_stdin({"wl-copy"}, text) != 0) throw Error("input_unavailable", "wl-copy failed");
        return;
    }
    if (command_exists("xclip")) {
        if (run_with_stdin({"xclip", "-selection", "clipboard"}, text) != 0) throw Error("input_unavailable", "xclip failed");
        return;
    }
    if (command_exists("xsel")) {
        if (run_with_stdin({"xsel", "--clipboard", "--input"}, text) != 0) throw Error("input_unavailable", "xsel failed");
        return;
    }
    throw Error("unsupported", "paste-text requires wl-copy, xclip, or xsel");
}
} // namespace

Json LinuxBackend::set_value(const Json& params, Context& context) {
    ensure_active(params);
    WindowInfo window = current_window(select_window(params));
    validate_observation(params, window);
    activate(window);
    if (!params.contains("elementIndex")) throw Error("invalid_argument", "set-value requires elementIndex");
    if (!params.contains("value")) throw Error("invalid_argument", "value is required");
#if DCU_HAVE_ATSPI
    AtspiAccessible* element = atspi_element_for_index(window, params, context);
    bool changed = false;
    std::string value;
    if (params.at("value").is_string()) value = params.at("value").get<std::string>();
    else value = params.at("value").dump();
    AtspiEditableText* editable = atspi_accessible_get_editable_text_iface(element);
    if (editable) {
        changed = atspi_editable_text_set_text_contents(editable, value.c_str(), nullptr) != FALSE;
        g_object_unref(editable);
    }
    if (!changed) {
        AtspiValue* numeric = atspi_accessible_get_value_iface(element);
        if (numeric && params.at("value").is_number()) {
            changed = atspi_value_set_current_value(numeric, params.at("value").get<double>(), nullptr) != FALSE;
        }
        if (numeric) g_object_unref(numeric);
    }
    g_object_unref(element);
    if (!changed) throw Error("unsupported", "The selected AT-SPI element does not expose an editable value");
    mark_input_complete();
    return action_result();
#else
    (void)context;
    throw Error("unsupported", "AT-SPI set-value is not available in this build");
#endif
}

Json LinuxBackend::paste_text(const Json& params, Context& context) {
    ensure_active(params);
    WindowInfo window = current_window(select_window(params));
    validate_observation(params, window);
    activate(window);
    const std::string text = string_value(params, "text");
    if (text.empty()) throw Error("invalid_argument", "text is required");
    const auto old_clipboard = read_clipboard_value(wayland_);
    try {
        write_clipboard_value(wayland_, text);
        hotkey_event({"ctrl", "v"}, context);
        wait_checked(context, 150);
    } catch (...) {
        try { write_clipboard_value(wayland_, old_clipboard.value_or("")); } catch (...) { }
        throw;
    }
    try { write_clipboard_value(wayland_, old_clipboard.value_or("")); } catch (...) { }
    mark_input_complete();
    return action_result();
}

Json LinuxBackend::execute(const std::string& method, const Json& params, Context& context) {
    // Common daemon serializes calls, but this guard still ensures a stale
    // session cannot mutate the desktop after a heartbeat/interrupt event.
    if (method != "doctor" && method != "capabilities" && method != "session.start" &&
        method != "session.stop") {
        if (interrupted_.load(std::memory_order_acquire)) throw Error("cancelled", "Session interrupted");
    }
    if (method == "doctor") return doctor();
    if (method == "capabilities") return capabilities();
    if (method == "session.start") return session_start(params, context);
    if (method == "session.stop") return session_stop(params);
    const auto run = [&]() -> Json {
        if (method == "list-windows") return list_windows(params);
        if (method == "list-apps") return list_apps(params);
        if (method == "get-app-state") return get_app_state(params, context);
        if (method == "get-full-screenshot") return full_screenshot(params);
        if (method == "click" || method == "drag" || method == "scroll" || method == "type-text" ||
            method == "press-key" || method == "hotkey" || method == "set-value" ||
            method == "paste-text") {
            // A modal parent ignores input, so it would only look delivered.
            ensure_active(params);
            reject_blocked_target(params);
        }
        if (method == "click") return click(params, context);
        if (method == "drag") return drag(params, context);
        if (method == "scroll") return scroll(params, context);
        if (method == "type-text") return type_text(params, context);
        if (method == "press-key") return press_key(params, context);
        if (method == "hotkey") return hotkey(params, context);
        if (method == "set-value") return set_value(params, context);
        if (method == "paste-text") return paste_text(params, context);
        throw Error("unsupported", "Unknown Linux backend method: " + method);
    };
    Json result = run();
    // The idle timeout counts from the last successful request, as in the
    // dispatcher, not from session start.
    last_activity_ = Clock::now();
    return result;
}

#if DCU_HAVE_PIPEWIRE && DCU_HAVE_GIO
struct LinuxBackend::PipeWireCapture {
    pw_thread_loop* loop = nullptr;
    pw_context* pw_ctx = nullptr;
    pw_core* core = nullptr;
    pw_stream* stream = nullptr;
    spa_hook listener{};
    mutable std::mutex mutex;
    std::condition_variable condition;
    bool frame_ready = false;
    bool failed = false;
    bool has_frame = false;
    bool input_requires_fresh_frame = false;
    bool loop_started = false;
    int frame_width = 0;
    int frame_height = 0;
    int frame_stride = 0;
    spa_video_format frame_format = SPA_VIDEO_FORMAT_UNKNOWN;
    std::vector<std::uint8_t> frame;
    std::uint64_t frame_sequence = 0;
    std::uint64_t source_frame_sequence = 0;
    std::int64_t frame_timestamp_ns = 0;
    std::uint64_t frame_generation = 0;
    std::uint64_t input_watermark_sequence = 0;
    std::chrono::system_clock::time_point frame_captured_at{};

    ~PipeWireCapture() { destroy(); }

    void destroy() noexcept {
        if (loop && loop_started) {
            pw_thread_loop_lock(loop);
            if (stream) {
                pw_stream_disconnect(stream);
            }
            pw_thread_loop_unlock(loop);
            pw_thread_loop_stop(loop);
            loop_started = false;
        }
        if (stream) {
            pw_stream_destroy(stream);
            stream = nullptr;
        }
        if (core) {
            pw_core_disconnect(core);
            core = nullptr;
        }
        if (pw_ctx) {
            pw_context_destroy(pw_ctx);
            pw_ctx = nullptr;
        }
        if (loop) {
            pw_thread_loop_destroy(loop);
            loop = nullptr;
        }
    }

    void mark_input_complete() noexcept {
        std::lock_guard lock(mutex);
        input_watermark_sequence = frame_sequence;
        input_requires_fresh_frame = true;
    }

    std::uint64_t coordinate_generation() const noexcept {
        std::lock_guard lock(mutex);
        return frame_generation;
    }

    static void state_changed(void* data, enum pw_stream_state, enum pw_stream_state, const char* error) {
        auto* self = static_cast<PipeWireCapture*>(data);
        if (error) {
            std::lock_guard lock(self->mutex);
            self->failed = true;
            self->condition.notify_all();
        }
    }

    static void param_changed(void* data, std::uint32_t id, const struct spa_pod* param) {
        auto* self = static_cast<PipeWireCapture*>(data);
        if (!param || id != SPA_PARAM_Format) return;
        spa_video_info_raw info{};
        if (spa_format_video_raw_parse(param, &info) < 0 || info.size.width == 0 || info.size.height == 0) {
            std::lock_guard lock(self->mutex);
            self->failed = true;
            self->condition.notify_all();
            return;
        }
        std::lock_guard lock(self->mutex);
        const bool format_changed = self->frame_width != static_cast<int>(info.size.width) ||
                                    self->frame_height != static_cast<int>(info.size.height) ||
                                    self->frame_format != info.format;
        self->frame_width = static_cast<int>(info.size.width);
        self->frame_height = static_cast<int>(info.size.height);
        self->frame_format = info.format;
        if (format_changed) {
            ++self->frame_generation;
            self->has_frame = false;
            self->frame_ready = false;
        }
        self->condition.notify_all();
    }

    static void process(void* data) {
        auto* self = static_cast<PipeWireCapture*>(data);
        if (!self->stream) return;
        pw_buffer* buffer = pw_stream_dequeue_buffer(self->stream);
        if (!buffer) return;
        spa_buffer* spa = buffer->buffer;
        if (spa && spa->n_datas > 0 && spa->datas[0].data && spa->datas[0].chunk) {
            int width = 0;
            int height = 0;
            spa_video_format format = SPA_VIDEO_FORMAT_UNKNOWN;
            {
                std::lock_guard lock(self->mutex);
                width = self->frame_width;
                height = self->frame_height;
                format = self->frame_format;
            }
            const int stride = spa->datas[0].chunk->stride;
            const std::size_t row_bytes = static_cast<std::size_t>(std::max(0, width)) * 4;
            const std::size_t source_stride = static_cast<std::size_t>(std::abs(stride));
            const std::size_t required = height > 0 && source_stride > 0
                ? source_stride * static_cast<std::size_t>(height) : 0;
            const std::size_t available = spa->datas[0].chunk->size;
            if (width > 0 && height > 0 && stride > 0 && source_stride >= row_bytes &&
                required <= available && spa->datas[0].chunk->offset <= spa->datas[0].maxsize &&
                available <= spa->datas[0].maxsize - spa->datas[0].chunk->offset &&
                (format == SPA_VIDEO_FORMAT_BGRx || format == SPA_VIDEO_FORMAT_RGBx ||
                 format == SPA_VIDEO_FORMAT_RGBA || format == SPA_VIDEO_FORMAT_BGRA ||
                 format == SPA_VIDEO_FORMAT_xRGB || format == SPA_VIDEO_FORMAT_xBGR ||
                 format == SPA_VIDEO_FORMAT_ARGB || format == SPA_VIDEO_FORMAT_ABGR)) {
                const auto* source = static_cast<const std::uint8_t*>(spa->datas[0].data) +
                                     spa->datas[0].chunk->offset;
                const auto* header = static_cast<const spa_meta_header*>(
                    spa_buffer_find_meta_data(spa, SPA_META_Header, sizeof(spa_meta_header)));
                const auto received_at = std::chrono::system_clock::now();
                auto captured_at = received_at;
                std::uint64_t source_sequence = 0;
                std::int64_t frame_timestamp_ns = 0;
                if (header) {
                    source_sequence = header->seq;
                    frame_timestamp_ns = header->pts;
                    // SPA timestamps and the stream clock are monotonic
                    // nanoseconds.  Translate a sane timestamp delta to wall
                    // time so capturedAt identifies frame production rather
                    // than callback scheduling time.
                    if (header->pts > 0) {
                        pw_time stream_time{};
                        int time_result = -1;
#if PW_CHECK_VERSION(0, 3, 50)
                        time_result = pw_stream_get_time_n(self->stream, &stream_time,
                                                           sizeof(stream_time));
#else
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
                        time_result = pw_stream_get_time(self->stream, &stream_time);
#pragma GCC diagnostic pop
#endif
                        if (time_result == 0 &&
                            stream_time.now >= header->pts) {
                            const auto age = stream_time.now - header->pts;
                            if (age <= std::chrono::duration_cast<std::chrono::nanoseconds>(
                                          std::chrono::minutes(5)).count()) {
                                captured_at -= std::chrono::nanoseconds(age);
                            }
                        }
                    }
                }
                std::lock_guard lock(self->mutex);
                self->frame_stride = stride;
                self->frame.assign(source, source + source_stride * static_cast<std::size_t>(height));
                self->frame_captured_at = captured_at;
                ++self->frame_sequence;
                self->source_frame_sequence = source_sequence;
                self->frame_timestamp_ns = frame_timestamp_ns;
                self->has_frame = true;
                self->frame_ready = true;
                self->condition.notify_all();
            } else {
                std::lock_guard lock(self->mutex);
                self->failed = true;
                self->condition.notify_all();
            }
        }
        pw_stream_queue_buffer(self->stream, buffer);
    }

    void start(LinuxBackend& backend, const WindowInfo& target, Context& context) {
        if (stream) return;
        backend.portal_ready_for_input(context);
        if (!backend.remote_stream_valid_ || backend.remote_stream_id_ == UINT32_MAX) {
            throw Error("capture_unavailable", "RemoteDesktop did not return a ScreenCast stream");
        }
        frame_width = std::max(1, target.width);
        frame_height = std::max(1, target.height);
        failed = false;
        has_frame = false;
        frame_ready = false;
        const int fd = backend.portal_bus_->open_pipewire_remote(
            backend.remote_desktop_session_, &context);
        bool descriptor_transferred = false;
        try {
            pw_init(nullptr, nullptr);
            loop = pw_thread_loop_new("dcu-capture", nullptr);
            if (!loop) throw Error("capture_unavailable", "Could not create PipeWire loop");
            pw_ctx = pw_context_new(pw_thread_loop_get_loop(loop), nullptr, 0);
            if (!pw_ctx) throw Error("capture_unavailable", "Could not create PipeWire context");
            // pw_context_connect_fd takes ownership of the descriptor.  Do
            // not close it after this call; PipeWire closes it on disconnect.
            core = pw_context_connect_fd(pw_ctx, fd, nullptr, 0);
            descriptor_transferred = true;
            if (!core) throw Error("capture_unavailable", "Could not connect to PipeWire remote");
            std::string target_serial;
            if (backend.remote_stream_serial_ != 0) target_serial = std::to_string(backend.remote_stream_serial_);
            pw_properties* properties = target_serial.empty()
                ? nullptr : pw_properties_new("target.object", target_serial.c_str(), nullptr);
            stream = pw_stream_new(core, "desktop-computer-use-capture", properties);
            if (!stream) throw Error("capture_unavailable", "Could not create PipeWire stream");
            pw_stream_events events{};
            events.version = PW_VERSION_STREAM_EVENTS;
            events.state_changed = &PipeWireCapture::state_changed;
            events.param_changed = &PipeWireCapture::param_changed;
            events.process = &PipeWireCapture::process;
            pw_stream_add_listener(stream, &listener, &events, this);
            const std::uint32_t requested_width = static_cast<std::uint32_t>(std::max(1, frame_width));
            const std::uint32_t requested_height = static_cast<std::uint32_t>(std::max(1, frame_height));
            std::uint8_t pod_buffer[2048];
            spa_pod_builder builder = SPA_POD_BUILDER_INIT(pod_buffer, sizeof(pod_buffer));
            const spa_pod* params[1] = {reinterpret_cast<const spa_pod*>(spa_pod_builder_add_object(
                &builder, SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
                SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
                SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
                SPA_FORMAT_VIDEO_format, SPA_POD_CHOICE_ENUM_Id(3, SPA_VIDEO_FORMAT_BGRx,
                                                                  SPA_VIDEO_FORMAT_RGBx, SPA_VIDEO_FORMAT_RGBA),
                SPA_FORMAT_VIDEO_size, SPA_POD_CHOICE_RANGE_Rectangle(
                    SPA_RECTANGLE(requested_width, requested_height), SPA_RECTANGLE(1, 1),
                    SPA_RECTANGLE(8192, 8192)),
                SPA_FORMAT_VIDEO_framerate, SPA_POD_CHOICE_RANGE_Fraction(
                    SPA_FRACTION(30, 1), SPA_FRACTION(0, 1), SPA_FRACTION(120, 1))))};
            if (!params[0]) throw Error("capture_unavailable", "Could not build PipeWire video format");
            if (pw_thread_loop_start(loop) != 0) {
                throw Error("capture_unavailable", "Could not start PipeWire loop");
            }
            loop_started = true;
            pw_thread_loop_lock(loop);
            const int connect_result = pw_stream_connect(
                stream, PW_DIRECTION_INPUT, backend.remote_stream_id_,
                static_cast<pw_stream_flags>(PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS), params, 1);
            pw_thread_loop_unlock(loop);
            if (connect_result != 0) {
                throw Error("capture_unavailable", "Could not connect PipeWire ScreenCast stream");
            }
        } catch (...) {
            if (!descriptor_transferred) ::close(fd);
            destroy();
            throw;
        }
    }

    RgbaImage capture(const WindowInfo& window, LinuxBackend& backend, Context& context) {
        if (!stream) start(backend, window, context);
        bool require_fresh_frame = false;
        std::uint64_t watermark = 0;
        bool wait_for_frame = false;
        {
            std::lock_guard lock(mutex);
            if (failed) throw Error("capture_unavailable", "PipeWire reported a capture stream failure");
            require_fresh_frame = input_requires_fresh_frame;
            watermark = input_watermark_sequence;
            wait_for_frame = !has_frame || (require_fresh_frame && frame_sequence <= watermark);
        }

        if (wait_for_frame) {
            const auto deadline = Clock::now() + std::chrono::seconds(5);
            while (Clock::now() < deadline) {
                check_context(context);
                std::unique_lock lock(mutex);
                if (failed || (has_frame && (!require_fresh_frame || frame_sequence > watermark))) break;
                condition.wait_for(lock, std::chrono::milliseconds(50));
            }
        }
        check_context(context);

        std::lock_guard lock(mutex);
        if (failed) throw Error("capture_unavailable", "PipeWire reported a capture stream failure");
        if (!has_frame || frame_width <= 0 || frame_height <= 0 || frame_stride < frame_width * 4 ||
            frame.size() < static_cast<std::size_t>(frame_stride) * frame_height) {
            throw Error("capture_unavailable", "No frame arrived from the PipeWire ScreenCast stream");
        }
        const bool fresh_frame = wait_for_frame && frame_sequence > watermark;
        if (require_fresh_frame && !fresh_frame) {
            throw Error("capture_not_ready", "No new PipeWire frame arrived after the input operation");
        }
        RgbaImage result;
        result.width = window.width;
        result.height = window.height;
        result.rgba.resize(static_cast<std::size_t>(window.width) * window.height * 4);
        result.captured_at = frame_captured_at;
        result.frame_sequence = frame_sequence;
        result.source_frame_sequence = source_frame_sequence;
        result.frame_timestamp_ns = frame_timestamp_ns;
        result.frame_generation = frame_generation;
        result.fresh_frame = fresh_frame;
        result.cached_frame = !fresh_frame;
        frame_ready = false;
        if (fresh_frame) input_requires_fresh_frame = false;
        int origin_x = backend.remote_stream_x_;
        int origin_y = backend.remote_stream_y_;
        if (backend.remote_stream_width_ <= 0 || backend.remote_stream_height_ <= 0) {
            origin_x = window.x;
            origin_y = window.y;
        }
        for (int y = 0; y < result.height; ++y) {
            const int sy = std::clamp(window.y - origin_y + y, 0, frame_height - 1);
            for (int x = 0; x < result.width; ++x) {
                const int sx = std::clamp(window.x - origin_x + x, 0, frame_width - 1);
                const std::size_t source = static_cast<std::size_t>(sy) * frame_stride +
                                           static_cast<std::size_t>(sx) * 4;
                const std::size_t target = (static_cast<std::size_t>(y) * result.width + x) * 4;
                const bool bgr = frame_format == SPA_VIDEO_FORMAT_BGRx || frame_format == SPA_VIDEO_FORMAT_BGRA;
                const bool x_bgr = frame_format == SPA_VIDEO_FORMAT_xBGR || frame_format == SPA_VIDEO_FORMAT_ABGR;
                const bool x_rgb = frame_format == SPA_VIDEO_FORMAT_xRGB || frame_format == SPA_VIDEO_FORMAT_ARGB;
                if (bgr) {
                    result.rgba[target] = frame[source + 2];
                    result.rgba[target + 1] = frame[source + 1];
                    result.rgba[target + 2] = frame[source];
                } else if (x_bgr) {
                    result.rgba[target] = frame[source + 3];
                    result.rgba[target + 1] = frame[source + 2];
                    result.rgba[target + 2] = frame[source + 1];
                } else if (x_rgb) {
                    result.rgba[target] = frame[source + 1];
                    result.rgba[target + 1] = frame[source + 2];
                    result.rgba[target + 2] = frame[source + 3];
                } else {
                    result.rgba[target] = frame[source];
                    result.rgba[target + 1] = frame[source + 1];
                    result.rgba[target + 2] = frame[source + 2];
                }
                result.rgba[target + 3] = 255;
            }
        }
        return result;
    }
};
#endif

std::string LinuxBackend::coordinate_signature() const {
    std::ostringstream signature;
    signature << (wayland_ ? "wayland" : (x11_ ? "x11" : "none"));

#if DCU_HAVE_X11
    if (display_) {
        signature << ":root=" << DisplayWidth(display_, DefaultScreen(display_))
                  << 'x' << DisplayHeight(display_, DefaultScreen(display_));
#if DCU_HAVE_XRANDR
        if (XRRScreenResources* resources = XRRGetScreenResourcesCurrent(display_, root_window_)) {
            for (int index = 0; index < resources->ncrtc; ++index) {
                const RRCrtc crtc = resources->crtcs[index];
                XRRCrtcInfo* info = XRRGetCrtcInfo(display_, resources, crtc);
                if (!info) continue;
                signature << ":crtc=" << crtc << ',' << info->x << ',' << info->y << ','
                          << info->width << ',' << info->height << ',' << info->rotation;
                XRRFreeCrtcInfo(info);
            }
            XRRFreeScreenResources(resources);
        }
#endif
    }
#endif

#if DCU_HAVE_GIO
    signature << ":portal=" << remote_desktop_session_ << ',' << remote_stream_id_ << ','
              << remote_stream_serial_ << ',' << remote_mapping_id_ << ','
              << remote_stream_x_ << ',' << remote_stream_y_ << ','
              << remote_stream_width_ << ',' << remote_stream_height_ << ','
              << remote_logical_width_ << ',' << remote_logical_height_;
#endif

#if DCU_HAVE_PIPEWIRE && DCU_HAVE_GIO
    if (pipewire_capture_) signature << ":framegen=" << pipewire_capture_->coordinate_generation();
#endif

    if (eis_) {
        signature << ":eis=" << (eis_->connected() ? 1 : 0) << ',' << (eis_->ready() ? 1 : 0);
        for (const auto& region : eis_->regions()) {
            signature << ':' << region.mapping_id << ',' << std::setprecision(17)
                      << region.x << ',' << region.y << ',' << region.width << ',' << region.height;
        }
    }
    return signature.str();
}

void LinuxBackend::refresh_coordinate_state() {
    const std::string current = coordinate_signature();
    if (coordinate_signature_.empty()) {
        coordinate_signature_ = current;
        coordinate_revision_ = 1;
        return;
    }
    if (current == coordinate_signature_) return;
    coordinate_signature_ = current;
    ++coordinate_revision_;
    clear_observations();
}

void LinuxBackend::mark_input_complete() noexcept {
#if DCU_HAVE_PIPEWIRE && DCU_HAVE_GIO
    if (pipewire_capture_) pipewire_capture_->mark_input_complete();
#endif
}

#if DCU_HAVE_X11
RgbaImage capture_x11(Display* display, Window root, const WindowInfo& window) {
    if (!display || window.width <= 0 || window.height <= 0) {
        throw Error("capture_unavailable", "X11 window geometry is unavailable");
    }
    XImage* image = XGetImage(display, root, window.x, window.y,
                              static_cast<unsigned int>(window.width),
                              static_cast<unsigned int>(window.height), AllPlanes, ZPixmap);
    if (!image) throw Error("capture_unavailable", "XGetImage could not capture the target window");
    RgbaImage result;
    result.width = window.width;
    result.height = window.height;
    result.captured_at = std::chrono::system_clock::now();
    result.fresh_frame = true;
    result.rgba.resize(static_cast<std::size_t>(result.width) * result.height * 4);
    const unsigned long red_mask = image->red_mask;
    const unsigned long green_mask = image->green_mask;
    const unsigned long blue_mask = image->blue_mask;
    const auto component = [](unsigned long value, unsigned long mask) -> std::uint8_t {
        if (!mask) return 0;
        unsigned long shifted = value & mask;
        unsigned int shift = 0;
        while (((mask >> shift) & 1UL) == 0UL && shift < sizeof(unsigned long) * 8U) ++shift;
        const unsigned long range = mask >> shift;
        if (!range) return 0;
        return static_cast<std::uint8_t>((shifted >> shift) * 255UL / range);
    };
    for (int y = 0; y < result.height; ++y) {
        for (int x = 0; x < result.width; ++x) {
            const unsigned long pixel = XGetPixel(image, x, y);
            const std::size_t offset = (static_cast<std::size_t>(y) * result.width + x) * 4;
            result.rgba[offset] = component(pixel, red_mask);
            result.rgba[offset + 1] = component(pixel, green_mask);
            result.rgba[offset + 2] = component(pixel, blue_mask);
            result.rgba[offset + 3] = 255;
        }
    }
    XDestroyImage(image);
    return result;
}
#endif

std::vector<std::uint8_t> resize_rgba(const RgbaImage& input, int width, int height) {
    if (input.width == width && input.height == height) return input.rgba;
    std::vector<std::uint8_t> output(static_cast<std::size_t>(width) * height * 4);
    for (int y = 0; y < height; ++y) {
        const int source_y = std::min(input.height - 1, y * input.height / height);
        for (int x = 0; x < width; ++x) {
            const int source_x = std::min(input.width - 1, x * input.width / width);
            const std::size_t source = (static_cast<std::size_t>(source_y) * input.width + source_x) * 4;
            const std::size_t target = (static_cast<std::size_t>(y) * width + x) * 4;
            std::copy_n(input.rgba.data() + source, 4, output.data() + target);
        }
    }
    return output;
}

#if DCU_HAVE_JPEG
void write_jpeg_file(const std::string& path, const RgbaImage& input, int width, int height, int quality) {
    FILE* file = std::fopen(path.c_str(), "wb");
    if (!file) throw Error("capture_unavailable", "Could not create screenshot file");
    jpeg_compress_struct compressor{};
    jpeg_error_mgr error{};
    compressor.err = jpeg_std_error(&error);
    jpeg_create_compress(&compressor);
    jpeg_stdio_dest(&compressor, file);
    compressor.image_width = static_cast<JDIMENSION>(width);
    compressor.image_height = static_cast<JDIMENSION>(height);
    compressor.input_components = 3;
    compressor.in_color_space = JCS_RGB;
    jpeg_set_defaults(&compressor);
    jpeg_set_quality(&compressor, std::clamp(quality, 1, 100), TRUE);
    jpeg_start_compress(&compressor, TRUE);
    const auto rgba = resize_rgba(input, width, height);
    std::vector<std::uint8_t> row(static_cast<std::size_t>(width) * 3);
    while (compressor.next_scanline < compressor.image_height) {
        const std::size_t source = static_cast<std::size_t>(compressor.next_scanline) * width * 4;
        for (int x = 0; x < width; ++x) {
            row[static_cast<std::size_t>(x) * 3] = rgba[source + static_cast<std::size_t>(x) * 4];
            row[static_cast<std::size_t>(x) * 3 + 1] = rgba[source + static_cast<std::size_t>(x) * 4 + 1];
            row[static_cast<std::size_t>(x) * 3 + 2] = rgba[source + static_cast<std::size_t>(x) * 4 + 2];
        }
        JSAMPROW rows[] = {row.data()};
        jpeg_write_scanlines(&compressor, rows, 1);
    }
    jpeg_finish_compress(&compressor);
    jpeg_destroy_compress(&compressor);
    std::fclose(file);
}
#endif

#if DCU_HAVE_PNG
void write_png_file(const std::string& path, const RgbaImage& input, int width, int height) {
    FILE* file = std::fopen(path.c_str(), "wb");
    if (!file) throw Error("capture_unavailable", "Could not create screenshot file");
    png_structp png = png_create_write_struct(PNG_LIBPNG_VER_STRING, nullptr, nullptr, nullptr);
    png_infop info = png_create_info_struct(png);
    if (!png || !info) {
        if (png) png_destroy_write_struct(&png, nullptr);
        std::fclose(file);
        throw Error("capture_unavailable", "Could not initialize PNG encoder");
    }
    if (setjmp(png_jmpbuf(png))) {
        png_destroy_write_struct(&png, &info);
        std::fclose(file);
        throw Error("capture_unavailable", "PNG encoding failed");
    }
    png_init_io(png, file);
    png_set_IHDR(png, info, static_cast<png_uint_32>(width), static_cast<png_uint_32>(height),
                 8, PNG_COLOR_TYPE_RGBA, PNG_INTERLACE_NONE, PNG_COMPRESSION_TYPE_DEFAULT,
                 PNG_FILTER_TYPE_DEFAULT);
    png_write_info(png, info);
    const auto rgba = resize_rgba(input, width, height);
    std::vector<png_bytep> rows(static_cast<std::size_t>(height));
    for (int y = 0; y < height; ++y) rows[static_cast<std::size_t>(y)] = const_cast<png_bytep>(
        rgba.data() + static_cast<std::size_t>(y) * width * 4);
    png_write_image(png, rows.data());
    png_write_end(png, nullptr);
    png_destroy_write_struct(&png, &info);
    std::fclose(file);
}
#endif

RgbaImage LinuxBackend::capture_window(const WindowInfo& window, Context& context) {
    check_context(context);
    if (wayland_) {
#if DCU_HAVE_PIPEWIRE && DCU_HAVE_GIO
        // PipeWireCapture is defined below.  A portal stream is opened once
        // and frames are copied on demand, so warm observations do not create
        // a new permission dialog or a new PipeWire connection.
        if (!pipewire_capture_) pipewire_capture_ = std::make_unique<PipeWireCapture>();
        return pipewire_capture_->capture(window, *this, context);
#else
        throw Error("capture_unavailable", "Wayland capture requires PipeWire support");
#endif
    }
#if DCU_HAVE_X11
    if (x11_ && display_) return capture_x11(display_, root_window_, window);
#endif
    throw Error("capture_unavailable", "No supported Linux capture backend is available");
}

namespace {
// Exact 2x2 box average. An odd trailing row or column is dropped, so output
// pixel (x, y) covers source pixels [2x, 2x + 1] x [2y, 2y + 1].
RgbaImage downscale_half(const RgbaImage& input) {
    RgbaImage output;
    output.width = std::max(1, input.width / 2);
    output.height = std::max(1, input.height / 2);
    output.rgba.assign(static_cast<std::size_t>(output.width) * output.height * 4, 0);
    for (int y = 0; y < output.height; ++y) {
        const int y0 = std::min(input.height - 1, y * 2);
        const int y1 = std::min(input.height - 1, y * 2 + 1);
        for (int x = 0; x < output.width; ++x) {
            const int x0 = std::min(input.width - 1, x * 2);
            const int x1 = std::min(input.width - 1, x * 2 + 1);
            const std::size_t target = (static_cast<std::size_t>(y) * output.width + x) * 4;
            for (int channel = 0; channel < 4; ++channel) {
                const auto at = [&](int sx, int sy) {
                    return static_cast<unsigned>(
                        input.rgba[(static_cast<std::size_t>(sy) * input.width + sx) * 4 + channel]);
                };
                output.rgba[target + channel] = static_cast<std::uint8_t>(
                    (at(x0, y0) + at(x1, y0) + at(x0, y1) + at(x1, y1) + 2) / 4);
            }
        }
    }
    return output;
}

RgbaImage resize_image(const RgbaImage& input, int width, int height) {
    RgbaImage output;
    output.rgba = resize_rgba(input, width, height);
    output.width = width;
    output.height = height;
    return output;
}
} // namespace

void LinuxBackend::save_image(const RgbaImage& image, const std::string& path, const std::string& format,
                              int quality, std::string* mime) {
    const int width = image.width;
    const int height = image.height;
    if (format == "png") {
#if DCU_HAVE_PNG
        write_png_file(path, image, width, height);
        *mime = "image/png";
#else
        throw Error("unsupported", "PNG encoding is not available in this build");
#endif
    } else if (format == "jpeg" || format == "jpg") {
#if DCU_HAVE_JPEG
        write_jpeg_file(path, image, width, height, quality);
        *mime = "image/jpeg";
#else
        throw Error("unsupported", "JPEG encoding is not available in this build");
#endif
    } else {
        throw Error("invalid_argument", "format must be jpeg or png");
    }
    ::chmod(path.c_str(), 0600);
}

#if DCU_HAVE_ATSPI
namespace {
struct AtspiRectGuard {
    AtspiRect* value = nullptr;
    ~AtspiRectGuard() { if (value) g_free(value); }
};

bool atspi_state(AtspiAccessible* node, AtspiStateType state) {
    if (!node) return false;
    AtspiStateSet* set = atspi_accessible_get_state_set(node);
    if (!set) return false;
    const gboolean result = atspi_state_set_contains(set, state);
    g_object_unref(set);
    return result != FALSE;
}

std::string atspi_name(AtspiAccessible* node) {
    if (!node) return {};
    gchar* value = atspi_accessible_get_name(node, nullptr);
    std::string result = value ? value : "";
    if (value) g_free(value);
    return result;
}

std::string atspi_role(AtspiAccessible* node) {
    if (!node) return {};
    gchar* value = atspi_accessible_get_role_name(node, nullptr);
    std::string result = value ? value : "";
    if (value) g_free(value);
    return result;
}

std::string atspi_id(AtspiAccessible* node) {
    if (!node) return {};
    gchar* value = atspi_accessible_get_accessible_id(node, nullptr);
    std::string result = value ? value : "";
    if (value) g_free(value);
    return result;
}

std::optional<std::array<int, 4>> atspi_rect(AtspiAccessible* node) {
    if (!node) return std::nullopt;
    AtspiComponent* component = atspi_accessible_get_component_iface(node);
    if (!component) return std::nullopt;
    AtspiRectGuard rect{atspi_component_get_extents(component, ATSPI_COORD_TYPE_SCREEN, nullptr)};
    g_object_unref(component);
    if (!rect.value || rect.value->width <= 0 || rect.value->height <= 0) return std::nullopt;
    return std::array<int, 4>{rect.value->x, rect.value->y, rect.value->width, rect.value->height};
}

AtspiAccessible* atspi_child(AtspiAccessible* node, int index) {
    if (!node) return nullptr;
    return atspi_accessible_get_child_at_index(node, index, nullptr);
}

AtspiAccessible* find_atspi_window(const WindowInfo& target) {
    AtspiAccessible* desktop = atspi_get_desktop(0);
    if (!desktop) return nullptr;
    const int apps = atspi_accessible_get_child_count(desktop, nullptr);
    for (int app_index = 0; app_index < apps; ++app_index) {
        AtspiAccessible* app = atspi_child(desktop, app_index);
        if (!app) continue;
        if (target.pid > 0 && atspi_accessible_get_process_id(app, nullptr) != static_cast<guint>(target.pid) &&
            lower(atspi_name(app)) != lower(target.app)) {
            g_object_unref(app);
            continue;
        }
        const int children = atspi_accessible_get_child_count(app, nullptr);
        for (int child_index = 0; child_index < children; ++child_index) {
            AtspiAccessible* child = atspi_child(app, child_index);
            if (!child) continue;
            const auto rect = atspi_rect(child);
            const std::string title = atspi_name(child);
            if (rect && rect->at(2) == target.width && rect->at(3) == target.height &&
                rect->at(0) == target.x && rect->at(1) == target.y) {
                return child;
            }
            if (!title.empty() && title == target.title && rect) return child;
            g_object_unref(child);
        }
        g_object_unref(app);
    }
    return nullptr;
}

bool atspi_sensitive(AtspiAccessible* node) {
    const std::string joined = lower(atspi_role(node) + " " + atspi_name(node) + " " + atspi_id(node));
    for (const auto& token : {"password", "passcode", "secret", "one-time code", "verification code"}) {
        if (joined.find(token) != std::string::npos) return true;
    }
    return false;
}

std::string atspi_value(AtspiAccessible* node) {
    if (!node || atspi_sensitive(node)) return atspi_sensitive(node) ? "[redacted]" : "";
    AtspiText* text = atspi_accessible_get_text_iface(node);
    if (text) {
        const int count = atspi_text_get_character_count(text, nullptr);
        if (count > 0) {
            gchar* value = atspi_text_get_text(text, 0, std::min(count, static_cast<int>(kMaxText)), nullptr);
            std::string result = value ? value : "";
            if (value) g_free(value);
            g_object_unref(text);
            if (count > static_cast<int>(kMaxText)) result += "...";
            return result;
        }
        g_object_unref(text);
    }
    AtspiValue* value = atspi_accessible_get_value_iface(node);
    if (value) {
        const double current = atspi_value_get_current_value(value, nullptr);
        g_object_unref(value);
        std::ostringstream out;
        out << current;
        return out.str();
    }
    return {};
}

Json atspi_actions(AtspiAccessible* node) {
    Json actions = Json::array();
    AtspiAction* action = atspi_accessible_get_action_iface(node);
    if (!action) return actions;
    const int count = atspi_action_get_n_actions(action, nullptr);
    for (int index = 0; index < count; ++index) {
        gchar* name = atspi_action_get_name(action, index, nullptr);
        if (name && *name) actions.push_back(name);
        if (name) g_free(name);
    }
    g_object_unref(action);
    return actions;
}
} // namespace
#endif

Json LinuxBackend::accessibility_snapshot(const WindowInfo& window, Context& context) {
#if !DCU_HAVE_ATSPI
    (void)window;
    (void)context;
    throw Error("unsupported", "AT-SPI accessibility support is not available in this build");
#else
    check_context(context);
    AtspiAccessible* root = find_atspi_window(window);
    if (!root) throw Error("accessibility_unavailable", "The target window is not exposed on the AT-SPI bus");
    Json elements = Json::array();
    std::ostringstream tree;
    std::size_t count = 0;
    std::function<void(AtspiAccessible*, std::size_t, const std::string&)> visit =
        [&](AtspiAccessible* node, std::size_t depth, const std::string& path) {
            if (!node || count >= kMaxTreeNodes || depth > kMaxTreeDepth) return;
            check_context(context);
            const int index = static_cast<int>(count++);
            const std::string name = atspi_name(node);
            const std::string role = atspi_role(node);
            const std::string value = atspi_value(node);
            Json record{{"index", index},
                        {"runtimeId", path},
                        {"automationId", atspi_id(node)},
                        {"name", name},
                        {"controlType", role},
                        {"localizedControlType", role},
                        {"className", ""},
                        {"value", value},
                        {"isSelected", atspi_state(node, ATSPI_STATE_SELECTED)},
                        {"actions", atspi_actions(node)}};
            const auto rect = atspi_rect(node);
            if (rect) record["frame"] = Json{{"x", rect->at(0) - window.x},
                                               {"y", rect->at(1) - window.y},
                                               {"width", rect->at(2)},
                                               {"height", rect->at(3)}};
            else record["frame"] = nullptr;
            elements.push_back(std::move(record));
            tree << std::string(depth * 2, ' ') << index << ": " << role;
            if (!name.empty()) tree << " " << name;
            if (!value.empty() && value != name) tree << " — " << value;
            tree << '\n';
            const int children = atspi_accessible_get_child_count(node, nullptr);
            for (int child = 0; child < children && count < kMaxTreeNodes; ++child) {
                AtspiAccessible* child_node = atspi_child(node, child);
                if (!child_node) continue;
                visit(child_node, depth + 1, path + "/" + std::to_string(child));
                g_object_unref(child_node);
            }
        };
    visit(root, 0, "0");
    g_object_unref(root);
    return Json{{"tree", tree.str()}, {"elements", std::move(elements)},
                {"truncated", count >= kMaxTreeNodes}, {"maxNodes", kMaxTreeNodes},
                {"maxDepth", kMaxTreeDepth}};
#endif
}

Json LinuxBackend::make_observation(const Json& params, Context& context) {
    check_context(context);
    WindowInfo window = select_window(params);
    refresh_coordinate_state();
    const std::string prior_id = string_value(params, "observationId");
    if (!prior_id.empty()) validate_observation(params, window);
    const bool include_screenshot = params.value("includeScreenshot", true);
    const bool include_text = params.value("includeText", false);
    const auto started = Clock::now();
    const std::string observation_id = uuid();
    ObservationRecord record;
    record.id = observation_id;
    // Images written before a failure below are not cached, so delete them here.
    struct PendingFiles {
        const ObservationRecord& record;
        bool armed = true;
        ~PendingFiles() { if (armed) remove_observation_files(record); }
    } pending{record};
    Json observation{{"observationId", observation_id},
                     {"capturedAt", utc_now()},
                     {"window", window_json(window)},
                     {"coordinateSpace", "window"},
                     {"coordinateRevision", coordinate_revision_},
                     {"timings", Json::object()},
                     {"overlayRegions", Json::array()}};
    if (const auto modal = blocking_modal(window)) {
        observation["modal"] = {{"windowId", modal->id}, {"title", modal->title}, {"app", modal->app}};
        observation["notice"] = "This window is blocked by modal dialog " + modal->id + " \"" +
                                modal->title + "\". Its controls are not in this window's "
                                "accessibility tree; observe and act on --window-id " +
                                modal->id + " instead.";
    }
    if (include_screenshot) {
        const auto capture_started = Clock::now();
        RgbaImage image = capture_window(window, context);
        const auto capture_done = Clock::now();
        if (image.captured_at.time_since_epoch().count() != 0) {
            observation["capturedAt"] = utc_now(image.captured_at);
        }
        const std::string format = lower(string_value(params, "format", "jpeg"));
        const int quality = integer(params, "quality", 85);
        // maxEdge is an optional cap on the reduced image only; 0 means no cap.
        const int max_edge = integer(params, "maxEdge", 0);
        const std::string extension = format == "png" ? ".png" : ".jpg";
        std::string mime;
        // Capture already resamples to the logical window size, so image-to-window
        // ratios are normally 1; they are kept so the transform stays exact.
        const double window_scale_x = static_cast<double>(image.width) / std::max(1, window.width);
        const double window_scale_y = static_cast<double>(image.height) / std::max(1, window.height);
        record.full = {image_directory_ + "/" + observation_id + "-full" + extension,
                       image.width, image.height, window_scale_x, window_scale_y};
        save_image(image, record.full.path, format, quality, &mime);

        // Reduced image: 0.5x above 1280x720 (2x2 box average), otherwise 1x,
        // then optionally capped by maxEdge.
        RgbaImage reduced_storage;
        const RgbaImage* reduced = &image;
        double factor_x = 1.0;
        double factor_y = 1.0;
        if (image.width > 1280 || image.height > 720) {
            reduced_storage = downscale_half(image);
            reduced = &reduced_storage;
            factor_x = factor_y = 0.5;
        }
        const int longest = std::max(reduced->width, reduced->height);
        if (max_edge > 0 && longest > max_edge) {
            const double cap = static_cast<double>(max_edge) / longest;
            const int width = std::max(1, static_cast<int>(std::lround(reduced->width * cap)));
            const int height = std::max(1, static_cast<int>(std::lround(reduced->height * cap)));
            factor_x *= static_cast<double>(width) / reduced->width;
            factor_y *= static_cast<double>(height) / reduced->height;
            reduced_storage = resize_image(*reduced, width, height);
            reduced = &reduced_storage;
        }
        record.reduced = {image_directory_ + "/" + observation_id + extension,
                          reduced->width, reduced->height,
                          factor_x * window_scale_x, factor_y * window_scale_y};
        save_image(*reduced, record.reduced.path, format, quality, &mime);
        record.has_screenshot = true;
        record.mime = mime;
        observation["screenshot"] = {{"path", record.reduced.path},
                                      {"mimeType", mime},
                                      {"width", record.reduced.width},
                                      {"height", record.reduced.height},
                                      {"variant", "reduced"},
                                      {"actionTransform", action_transform(record.reduced)},
                                      {"fullAvailable", true},
                                      {"fullWidth", record.full.width},
                                      {"fullHeight", record.full.height},
                                      {"scale", record.reduced.scale_x},
                                      {"scaleX", record.reduced.scale_x},
                                      {"scaleY", record.reduced.scale_y},
                                      {"sourceWidth", image.width},
                                      {"sourceHeight", image.height},
                                      {"frameSequence", image.frame_sequence},
                                      {"sourceFrameSequence", image.source_frame_sequence},
                                      {"frameTimestampNs", image.frame_timestamp_ns},
                                      {"frameGeneration", image.frame_generation},
                                      {"freshFrame", image.fresh_frame},
                                      {"cachedFrame", image.cached_frame}};
        observation["timings"]["captureMs"] = std::chrono::duration_cast<std::chrono::milliseconds>(
            capture_done - capture_started).count();

#if DCU_HAVE_GIO
        // X11 root capture and a monitor-wide PipeWire stream both include
        // Shell actors.  Report their screen-logical rectangles so callers
        // can avoid mistaking the activity indicator for application pixels.
        if ((x11_ || wayland_) && shell_bus_ && shell_bus_->available()) {
            observation["overlayRegionsSpace"] = "screen-logical";
            try {
                observation["overlayRegions"] = valid_overlay_regions(shell_bus_->get_overlay_regions());
            } catch (const Error&) {
                // Overlay metadata is advisory; retain the screenshot when a
                // concurrently restarting extension cannot answer this call.
            }
        }
#endif
    }
    if (include_text) {
        const auto text_started = Clock::now();
        observation["accessibility"] = accessibility_snapshot(window, context);
        observation["timings"]["accessibilityMs"] = std::chrono::duration_cast<std::chrono::milliseconds>(
            Clock::now() - text_started).count();
    }

    // Capture and accessibility traversal can take long enough for a window
    // to move or resize.  Do not publish an observation whose image or text
    // is paired with geometry that is no longer true at completion time.
    check_context(context);
    const WindowInfo final_window = current_window(window);
    check_context(context);
    if (final_window.id != window.id || final_window.x != window.x ||
        final_window.y != window.y || final_window.width != window.width ||
        final_window.height != window.height) {
        throw Error("stale_observation", "Target window geometry changed during observation; observe again");
    }
    observation["timings"]["totalMs"] = std::chrono::duration_cast<std::chrono::milliseconds>(
        Clock::now() - started).count();
    refresh_coordinate_state();
    observation["coordinateRevision"] = coordinate_revision_;
    record.window = window;
    record.created = Clock::now();
    record.coordinate_revision = coordinate_revision_;
    pending.armed = false;
    store_observation(std::move(record));
    return observation;
}

Json LinuxBackend::get_app_state(const Json& params, Context& context) {
    return make_observation(params, context);
}

#if DCU_HAVE_ATSPI
namespace {
AtspiAccessible* atspi_element_for_index(const WindowInfo& window, const Json& params, Context& context) {
    const int requested = integer(params, "elementIndex", -1);
    if (requested < 0) throw Error("invalid_argument", "elementIndex must be non-negative");
    const std::string observation_id = string_value(params, "observationId");
    if (observation_id.empty()) throw Error("invalid_argument", "element actions require observationId");
    AtspiAccessible* root = find_atspi_window(window);
    if (!root) throw Error("accessibility_unavailable", "The target window is not exposed on the AT-SPI bus");
    AtspiAccessible* result = nullptr;
    int count = 0;
    std::function<void(AtspiAccessible*, std::size_t)> visit = [&](AtspiAccessible* node, std::size_t depth) {
        if (!node || result || count >= static_cast<int>(kMaxTreeNodes) || depth > kMaxTreeDepth) return;
        check_context(context);
        if (count++ == requested) {
            result = static_cast<AtspiAccessible*>(g_object_ref(node));
            return;
        }
        const int children = atspi_accessible_get_child_count(node, nullptr);
        for (int child = 0; child < children && !result; ++child) {
            AtspiAccessible* child_node = atspi_child(node, child);
            if (!child_node) continue;
            visit(child_node, depth + 1);
            g_object_unref(child_node);
        }
    };
    visit(root, 0);
    g_object_unref(root);
    if (!result) throw Error("element_not_found", "elementIndex is not present in the current accessibility tree");
    return result;
}
} // namespace
#endif






namespace {

std::string env(const char* name) {
    const char* value = std::getenv(name);
    return value == nullptr ? std::string{} : std::string(value);
}

bool env_present(const char* name) { return !env(name).empty(); }

std::string uuid() {
    std::random_device rd;
    std::mt19937_64 gen(rd());
    std::uniform_int_distribution<std::uint64_t> dist;
    const std::uint64_t a = dist(gen);
    const std::uint64_t b = dist(gen);
    std::ostringstream out;
    out << std::hex << std::setfill('0') << std::setw(16) << a << std::setw(16) << b;
    return out.str();
}

std::string utc_now() {
    return utc_now(std::chrono::system_clock::now());
}

std::string utc_now(std::chrono::system_clock::time_point value) {
    const auto tt = std::chrono::system_clock::to_time_t(value);
    std::tm tm{};
    gmtime_r(&tt, &tm);
    const auto millis = std::chrono::duration_cast<std::chrono::milliseconds>(
                            value.time_since_epoch()) % 1000;
    std::ostringstream out;
    out << std::put_time(&tm, "%Y-%m-%dT%H:%M:%S") << '.' << std::setw(3)
        << std::setfill('0') << millis.count() << 'Z';
    return out.str();
}

std::string lower(std::string value) {
    std::transform(value.begin(), value.end(), value.begin(), [](unsigned char c) {
        return static_cast<char>(std::tolower(c));
    });
    return value;
}

bool command_exists(const std::string& command) {
    const std::string path = "/usr/bin/" + command;
    return ::access(path.c_str(), X_OK) == 0 || ::access(("/bin/" + command).c_str(), X_OK) == 0;
}

bool libei_runtime_available() {
    void* handle = dlopen("libei.so.1", RTLD_LAZY | RTLD_LOCAL);
    if (!handle) handle = dlopen("libei.so", RTLD_LAZY | RTLD_LOCAL);
    if (!handle) return false;
    // These are the sender-side symbols used by EisClient.  Older probes
    // looked for the removed ei_new/ei_connect pair and advertised EIS even
    // though the actual transport could never be initialized.
    const bool available = dlsym(handle, "ei_new_sender") != nullptr &&
                           dlsym(handle, "ei_setup_backend_fd") != nullptr;
    dlclose(handle);
    return available;
}

std::string runtime_directory() {
    std::string dir = env("XDG_RUNTIME_DIR");
    if (dir.empty()) dir = "/tmp";
    dir += "/desktop-computer-use";
    ::mkdir(dir.c_str(), 0700);
    ::chmod(dir.c_str(), 0700);
    return dir;
}

void check_context(Context& context) { context.check(); }

int integer(const Json& object, const char* key, int fallback) {
    if (!object.contains(key) || object.at(key).is_null()) return fallback;
    if (!object.at(key).is_number_integer()) {
        throw Error("invalid_argument", std::string(key) + " must be an integer");
    }
    return object.at(key).get<int>();
}

double number(const Json& object, const char* key, double fallback) {
    if (!object.contains(key) || object.at(key).is_null()) return fallback;
    if (!object.at(key).is_number()) {
        throw Error("invalid_argument", std::string(key) + " must be a number");
    }
    return object.at(key).get<double>();
}

std::string string_value(const Json& object, const char* key, std::string fallback) {
    if (!object.contains(key) || object.at(key).is_null()) return fallback;
    if (!object.at(key).is_string()) {
        throw Error("invalid_argument", std::string(key) + " must be a string");
    }
    return object.at(key).get<std::string>();
}

Json action_result() {
    return Json{{"delivered", true}, {"verification", {{"state", "unverified"}}}};
}

Json window_json(const WindowInfo& window) {
    Json result{{"id", window.id},
                {"app", window.app},
                {"title", window.title},
                {"pid", window.pid},
                {"x", window.x},
                {"y", window.y},
                {"width", window.width},
                {"height", window.height},
                {"active", window.active}};
    if (!window.owner_id.empty()) {
        result["ownerWindowId"] = window.owner_id;
        result["modal"] = window.modal;
    }
    return result;
}

Json valid_overlay_regions(const Json& candidate) {
    Json regions = Json::array();
    if (!candidate.is_array()) return regions;
    for (const auto& item : candidate) {
        if (!item.is_object() || !item.contains("kind") || !item.at("kind").is_string()) continue;
        const bool has_geometry = item.contains("x") && item.contains("y") &&
                                  item.contains("width") && item.contains("height");
        if (!has_geometry || !item.at("x").is_number() || !item.at("y").is_number() ||
            !item.at("width").is_number() || !item.at("height").is_number()) continue;
        const double x = item.at("x").get<double>();
        const double y = item.at("y").get<double>();
        const double width = item.at("width").get<double>();
        const double height = item.at("height").get<double>();
        if (!std::isfinite(x) || !std::isfinite(y) || !std::isfinite(width) ||
            !std::isfinite(height) || width <= 0.0 || height <= 0.0) continue;
        regions.push_back(Json{{"kind", item.at("kind")},
                                {"x", x},
                                {"y", y},
                                {"width", width},
                                {"height", height}});
    }
    return regions;
}



} // namespace

std::unique_ptr<Backend> make_backend() {
    return std::make_unique<LinuxBackend>();
}

} // namespace dcu
