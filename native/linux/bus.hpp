#pragma once

#if DCU_HAVE_GIO

#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <functional>
#include <memory>
#include <string>
#include <thread>
#include "portal_bus.hpp"

namespace dcu {

// Construct and pump this subscription on the dedicated stop-monitor thread.
// It never calls input providers: cancellation lets their owning worker release
// held input without racing libei or an in-progress native operation.
class ShellStopSubscription {
    GDBusConnection* connection_;
    GMainContext* mainContext_ = nullptr;
    guint subscriptionId_ = 0;
    std::shared_ptr<std::atomic_bool> stopped_ = std::make_shared<std::atomic_bool>(false);

public:
    explicit ShellStopSubscription(GDBusConnection* connection) : connection_(connection) {
        if (!connection_) throw Error("setup_required", "GNOME session bus is unavailable");
        mainContext_ = g_main_context_new();
        g_object_ref(connection_);
        g_main_context_push_thread_default(mainContext_);
        auto callbackState = new std::shared_ptr<std::atomic_bool>(stopped_);
        subscriptionId_ = g_dbus_connection_signal_subscribe(
            connection_, "org.desktopcomputeruse.Shell", "org.desktopcomputeruse.Shell",
            "Stopped", "/org/desktopcomputeruse/Shell", nullptr, G_DBUS_SIGNAL_FLAGS_NONE,
            [](GDBusConnection*, const gchar*, const gchar*, const gchar*, const gchar*,
               GVariant*, gpointer data) {
                (**static_cast<std::shared_ptr<std::atomic_bool>*>(data)).store(true);
            }, callbackState,
            [](gpointer data) { delete static_cast<std::shared_ptr<std::atomic_bool>*>(data); });
    }
    ShellStopSubscription(const ShellStopSubscription&) = delete;
    ShellStopSubscription& operator=(const ShellStopSubscription&) = delete;
    ~ShellStopSubscription() {
        g_dbus_connection_signal_unsubscribe(connection_, subscriptionId_);
        g_main_context_pop_thread_default(mainContext_);
        g_main_context_unref(mainContext_);
        g_object_unref(connection_);
    }
    bool poll_stopped() {
        while (g_main_context_pending(mainContext_)) g_main_context_iteration(mainContext_, FALSE);
        return stopped_->load() || g_dbus_connection_is_closed(connection_);
    }
};

class GVariantRef {
public:
    explicit GVariantRef(GVariant* value = nullptr) : value_(value) {}
    ~GVariantRef() { if (value_) g_variant_unref(value_); }
    GVariantRef(const GVariantRef&) = delete;
    GVariantRef& operator=(const GVariantRef&) = delete;
    GVariant* get() const { return value_; }

private:
    GVariant* value_ = nullptr;
};

class GErrorRef {
public:
    ~GErrorRef() { if (error_) g_error_free(error_); }
    GError** out() { return &error_; }
    std::string message() const { return error_ ? error_->message : std::string{}; }

private:
    GError* error_ = nullptr;
};

inline GVariant* bus_empty_options() {
    GVariantBuilder builder;
    g_variant_builder_init(&builder, G_VARIANT_TYPE_VARDICT);
    return g_variant_builder_end(&builder);
}

class SessionBus {
    GDBusConnection* connection_ = nullptr;
    std::string error_;

    GVariantRef call(const char* method, GVariant* parameters,
                     const GVariantType* replyType, int timeoutMilliseconds) {
        if (!connection_) throw Error("setup_required", "GNOME session bus is unavailable");
        GErrorRef error;
        GVariant* reply = g_dbus_connection_call_sync(
            connection_, "org.desktopcomputeruse.Shell", "/org/desktopcomputeruse/Shell",
            "org.desktopcomputeruse.Shell", method, parameters, replyType,
            G_DBUS_CALL_FLAGS_NONE, timeoutMilliseconds, nullptr, error.out());
        if (!reply) throw Error("input_unavailable", error.message().empty()
            ? "GNOME extension rejected the request" : error.message());
        return GVariantRef(reply);
    }

    Json call_json(const char* method, GVariant* parameters, int timeoutMilliseconds) {
        const auto reply = call(method, parameters, G_VARIANT_TYPE("(s)"), timeoutMilliseconds);
        const gchar* payload = nullptr;
        g_variant_get(reply.get(), "(&s)", &payload);
        try {
            return Json::parse(payload ? payload : "{}");
        } catch (const std::exception& error) {
            throw Error("protocol_error", std::string("GNOME extension returned invalid JSON: ") + error.what());
        }
    }

    bool call_boolean(const char* method, GVariant* parameters, int timeoutMilliseconds) {
        const auto reply = call(method, parameters, G_VARIANT_TYPE("(b)"), timeoutMilliseconds);
        gboolean result = FALSE;
        g_variant_get(reply.get(), "(b)", &result);
        return result != FALSE;
    }

public:
    SessionBus() {
        const char* address = std::getenv("DBUS_SESSION_BUS_ADDRESS");
        if (!address || !*address) return;
        GErrorRef error;
        connection_ = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, error.out());
        if (!connection_) error_ = error.message();
    }
    SessionBus(const SessionBus&) = delete;
    SessionBus& operator=(const SessionBus&) = delete;
    ~SessionBus() { if (connection_) g_object_unref(connection_); }

    bool available() const {
        if (!connection_) return false;
        // These request threads do not run a persistent GLib main loop, so a
        // GDBusProxy owner cache can remain stale after Shell starts or exits.
        GVariantRef reply(g_dbus_connection_call_sync(
            connection_, "org.freedesktop.DBus", "/org/freedesktop/DBus",
            "org.freedesktop.DBus", "NameHasOwner",
            g_variant_new("(s)", "org.desktopcomputeruse.Shell"), G_VARIANT_TYPE("(b)"),
            G_DBUS_CALL_FLAGS_NONE, 500, nullptr, nullptr));
        if (!reply.get()) return false;
        gboolean hasOwner = FALSE;
        g_variant_get(reply.get(), "(b)", &hasOwner);
        return hasOwner != FALSE;
    }

    GDBusConnection* connection() const { return connection_; }
    const std::string& error() const { return error_; }

    Json call_string(const char* method, const std::string& argument, int timeoutMilliseconds = 5000) {
        return call_json(method, g_variant_new("(s)", argument.c_str()), timeoutMilliseconds);
    }
    Json call_string_no_arguments(const char* method, int timeoutMilliseconds = 5000) {
        return call_json(method, nullptr, timeoutMilliseconds);
    }
    bool call_bool(const char* method, int timeoutMilliseconds = 5000) {
        return call_boolean(method, nullptr, timeoutMilliseconds);
    }
    Json list_windows() { return call_string_no_arguments("ListWindows"); }

    bool activate(const std::string& windowId) {
        if (!call_boolean("Activate", g_variant_new("(s)", windowId.c_str()), 5000)) {
            throw Error("input_unavailable", "GNOME could not activate the target window");
        }
        return true;
    }
    bool pointer(double x, double y) {
        return call_boolean("Pointer", g_variant_new("(dd)", x, y), 2000);
    }
    // Asks the extension to unregister its Escape stop accelerator for the
    // given time so an injected Escape reaches the focused application and is
    // not counted as the user's emergency stop. The reply arrives only after
    // the binding has been removed.
    bool suspend_stop_key(std::uint32_t milliseconds) {
        return call_boolean("SuspendStopKey", g_variant_new("(u)", milliseconds), 2000);
    }
    Json get_overlay_regions() {
        auto regions = call_json("GetOverlayRegions", nullptr, 2000);
        if (!regions.is_array()) throw Error("protocol_error", "GNOME extension overlay regions must be an array");
        return regions;
    }
};
} // namespace dcu

#endif
