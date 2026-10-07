#pragma once

#include "dcu/backend.hpp"
#include <gio/gio.h>
#include <gio/gunixfdlist.h>
#include <chrono>
#include <functional>
#include <map>
#include <memory>
#include <string>
#include <thread>

namespace dcu {
namespace portal_detail {
constexpr const char* busName = "org.freedesktop.portal.Desktop";
constexpr const char* desktopPath = "/org/freedesktop/portal/desktop";

struct VariantDeleter {
    void operator()(GVariant* value) const { if (value) g_variant_unref(value); }
};
using Variant = std::unique_ptr<GVariant, VariantDeleter>;

struct ErrorMessage {
    GError* value = nullptr;
    ~ErrorMessage() { if (value) g_error_free(value); }
    std::string text(const char* fallback) const {
        return value ? value->message : fallback;
    }
};

struct PortalResponse {
    unsigned code = 2;
    Variant values;
};

class CallCancellation {
    GCancellable* cancellable_ = g_cancellable_new();
    std::jthread monitor_;

public:
    explicit CallCancellation(Context* context) {
        if (!context) return;
        monitor_ = std::jthread([this, context](std::stop_token stop) {
            while (!stop.stop_requested()) {
                if (context->cancelled) {
                    g_cancellable_cancel(cancellable_);
                    return;
                }
                std::this_thread::sleep_for(std::chrono::milliseconds(5));
            }
        });
    }
    ~CallCancellation() {
        monitor_.request_stop();
        if (monitor_.joinable()) monitor_.join();
        g_object_unref(cancellable_);
    }
    GCancellable* get() const { return cancellable_; }
};

class ResponseSubscription {
    GDBusConnection* connection_;
    GMainContext* mainContext_;
    guint subscriptionId_ = 0;
    using Responses = std::map<std::string, PortalResponse>;
    std::shared_ptr<Responses> responses_ = std::make_shared<Responses>();

public:
    explicit ResponseSubscription(GDBusConnection* connection)
        : connection_(connection), mainContext_(g_main_context_new()) {
        // Each request worker pumps only its own callbacks. Subscribe before
        // issuing the method, since a portal can answer before call_sync returns.
        g_main_context_push_thread_default(mainContext_);
        auto callbackState = new std::shared_ptr<Responses>(responses_);
        subscriptionId_ = g_dbus_connection_signal_subscribe(
            connection_, busName, "org.freedesktop.portal.Request", "Response",
            nullptr, nullptr, G_DBUS_SIGNAL_FLAGS_NONE,
            [](GDBusConnection*, const gchar*, const gchar* path, const gchar*,
               const gchar*, GVariant* parameters, gpointer userData) {
                auto& responses = **static_cast<std::shared_ptr<Responses>*>(userData);
                guint32 code = 2;
                GVariant* values = nullptr;
                g_variant_get(parameters, "(u@a{sv})", &code, &values);
                responses.insert_or_assign(path, PortalResponse{code, Variant(values)});
            }, callbackState,
            [](gpointer userData) {
                delete static_cast<std::shared_ptr<Responses>*>(userData);
            });
    }

    ResponseSubscription(const ResponseSubscription&) = delete;
    ResponseSubscription& operator=(const ResponseSubscription&) = delete;
    ~ResponseSubscription() {
        g_dbus_connection_signal_unsubscribe(connection_, subscriptionId_);
        g_main_context_pop_thread_default(mainContext_);
        g_main_context_unref(mainContext_);
    }

    PortalResponse wait(const std::string& path, Context* cancellation, int timeoutMilliseconds) {
        const auto deadline = std::chrono::steady_clock::now() +
                              std::chrono::milliseconds(timeoutMilliseconds);
        while (std::chrono::steady_clock::now() < deadline) {
            if (cancellation) cancellation->check();
            while (g_main_context_pending(mainContext_)) {
                g_main_context_iteration(mainContext_, FALSE);
            }
            const auto response = responses_->find(path);
            if (response != responses_->end()) return std::move(response->second);
            std::this_thread::sleep_for(std::chrono::milliseconds(5));
        }
        throw Error("timeout", "Portal permission request timed out");
    }
};
} // namespace portal_detail

// Transport only: the backend owns device selection, stream mapping, and the
// relationship between a RemoteDesktop session and its ScreenCast streams.
class PortalBus {
    GDBusConnection* connection_ = nullptr;
    std::string error_;

    void require_connection() const {
        if (!connection_) throw Error("setup_required", "D-Bus session bus is unavailable");
    }

    void close_object(const std::string& path, const char* interfaceName) noexcept {
        if (!connection_ || path.empty()) return;
        // Cancellation must not block behind another portal roundtrip.
        g_dbus_connection_call(connection_, portal_detail::busName, path.c_str(),
                               interfaceName, "Close", nullptr, nullptr,
                               G_DBUS_CALL_FLAGS_NONE, 1000, nullptr, nullptr, nullptr);
    }

    int receive_descriptor(const char* interfaceName, const char* method,
                           const std::string& sessionHandle, Context* cancellation) {
        require_connection();
        if (cancellation) cancellation->check();
        portal_detail::CallCancellation callCancellation(cancellation);
        portal_detail::ErrorMessage error;
        GUnixFDList* descriptorList = nullptr;
        portal_detail::Variant reply(g_dbus_connection_call_with_unix_fd_list_sync(
            connection_, portal_detail::busName, portal_detail::desktopPath,
            interfaceName, method, g_variant_new("(oa{sv})", sessionHandle.c_str(), nullptr),
            G_VARIANT_TYPE("(h)"), G_DBUS_CALL_FLAGS_NONE, 5000,
            nullptr, &descriptorList, callCancellation.get(), &error.value));
        struct DescriptorListCloser {
            GUnixFDList* list;
            ~DescriptorListCloser() { if (list) g_object_unref(list); }
        } listCloser{descriptorList};
        if (!reply) {
            if (cancellation) cancellation->check();
            throw Error("portal_error", error.text("Portal descriptor request failed"));
        }

        gint32 descriptorIndex = -1;
        g_variant_get(reply.get(), "(h)", &descriptorIndex);
        if (!descriptorList || descriptorIndex < 0) {
            throw Error("protocol_error", "Portal returned no file descriptor");
        }
        const int descriptor = g_unix_fd_list_get(descriptorList, descriptorIndex, &error.value);
        if (descriptor < 0) throw Error("portal_error", error.text("Cannot receive portal descriptor"));
        return descriptor;
    }

public:
    PortalBus() {
        portal_detail::ErrorMessage error;
        connection_ = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, &error.value);
        if (!connection_) error_ = error.text("D-Bus session bus is unavailable");
    }
    PortalBus(const PortalBus&) = delete;
    PortalBus& operator=(const PortalBus&) = delete;
    ~PortalBus() { if (connection_) g_object_unref(connection_); }

    bool available() const { return connection_ != nullptr; }
    const std::string& error() const { return error_; }
    GDBusConnection* connection() const { return connection_; }

    std::string create_session(const std::string& requestToken, const std::string& sessionToken,
                               Context* cancellation) {
        GVariantBuilder options;
        g_variant_builder_init(&options, G_VARIANT_TYPE_VARDICT);
        g_variant_builder_add(&options, "{sv}", "handle_token", g_variant_new_string(requestToken.c_str()));
        // The request and session have separate object paths. The portal
        // requires a session token before it can begin user authorization.
        g_variant_builder_add(&options, "{sv}", "session_handle_token", g_variant_new_string(sessionToken.c_str()));
        std::string sessionHandle;
        request_dict("org.freedesktop.portal.RemoteDesktop", "CreateSession",
                     g_variant_new("(a{sv})", &options), cancellation,
                     [&sessionHandle](GVariant* values) {
                         portal_detail::Variant handle(g_variant_lookup_value(values, "session_handle", nullptr));
                         if (handle && (g_variant_is_of_type(handle.get(), G_VARIANT_TYPE_STRING) ||
                                        g_variant_is_of_type(handle.get(), G_VARIANT_TYPE_OBJECT_PATH))) {
                             sessionHandle = g_variant_get_string(handle.get(), nullptr);
                         }
                     });
        if (sessionHandle.empty()) throw Error("protocol_error", "RemoteDesktop returned no session handle");
        return sessionHandle;
    }

    void request_dict(const char* interfaceName, const char* method, GVariant* parameters,
                      Context* cancellation, const std::function<void(GVariant*)>& consume,
                      int timeoutMilliseconds = 30000) {
        require_connection();
        if (cancellation) cancellation->check();
        const auto requestStarted = std::chrono::steady_clock::now();
        portal_detail::ResponseSubscription subscription(connection_);
        portal_detail::CallCancellation callCancellation(cancellation);
        portal_detail::ErrorMessage error;
        portal_detail::Variant reply(g_dbus_connection_call_sync(
            connection_, portal_detail::busName, portal_detail::desktopPath,
            interfaceName, method, parameters, G_VARIANT_TYPE("(o)"),
            G_DBUS_CALL_FLAGS_NONE, timeoutMilliseconds, callCancellation.get(), &error.value));
        if (!reply) {
            if (cancellation) cancellation->check();
            throw Error("permission_required", error.text("Portal request could not be created"));
        }

        const gchar* path = nullptr;
        g_variant_get(reply.get(), "(&o)", &path);
        if (!path) throw Error("protocol_error", "Portal returned no request path");
        try {
            const auto elapsedMilliseconds = std::chrono::duration_cast<std::chrono::milliseconds>(
                std::chrono::steady_clock::now() - requestStarted).count();
            const int remainingMilliseconds = static_cast<int>(timeoutMilliseconds - elapsedMilliseconds);
            auto response = subscription.wait(path, cancellation, remainingMilliseconds);
            if (response.code != 0) throw Error("permission_required", "Portal permission was denied");
            if (response.values) consume(response.values.get());
        } catch (...) {
            close_object(path, "org.freedesktop.portal.Request");
            throw;
        }
    }

    unsigned interface_version(const char* interfaceName, Context* cancellation = nullptr) const {
        require_connection();
        if (cancellation) cancellation->check();
        portal_detail::CallCancellation callCancellation(cancellation);
        portal_detail::ErrorMessage error;
        portal_detail::Variant reply(g_dbus_connection_call_sync(
            connection_, portal_detail::busName, portal_detail::desktopPath,
            "org.freedesktop.DBus.Properties", "Get", g_variant_new("(ss)", interfaceName, "version"),
            G_VARIANT_TYPE("(v)"), G_DBUS_CALL_FLAGS_NONE, 5000, callCancellation.get(), &error.value));
        if (!reply) {
            if (cancellation) cancellation->check();
            throw Error("portal_error", error.text("Cannot inspect portal version"));
        }
        GVariant* version = nullptr;
        g_variant_get(reply.get(), "(v)", &version);
        portal_detail::Variant ownedVersion(version);
        if (!g_variant_is_of_type(version, G_VARIANT_TYPE_UINT32)) {
            throw Error("protocol_error", "Portal version must be an unsigned integer");
        }
        return g_variant_get_uint32(version);
    }

    int open_pipewire_remote(const std::string& sessionHandle, Context* cancellation = nullptr) {
        return receive_descriptor("org.freedesktop.portal.ScreenCast", "OpenPipeWireRemote", sessionHandle, cancellation);
    }
    int connect_to_eis(const std::string& sessionHandle, Context* cancellation = nullptr) {
        return receive_descriptor("org.freedesktop.portal.RemoteDesktop", "ConnectToEIS", sessionHandle, cancellation);
    }
    void close_session(const std::string& sessionHandle) noexcept {
        close_object(sessionHandle, "org.freedesktop.portal.Session");
    }
};
} // namespace dcu
