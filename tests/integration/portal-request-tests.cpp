#define DCU_HAVE_GIO 1
#include "bus.hpp"
#include <atomic>
#include <iostream>
#include <stdexcept>
#include <unistd.h>

namespace {
std::atomic<unsigned> requestSequence{0};

void emit_response(GDBusConnection* connection, const std::string& path) {
    GVariantBuilder values;
    g_variant_builder_init(&values, G_VARIANT_TYPE_VARDICT);
    g_variant_builder_add(&values, "{sv}", "verified", g_variant_new_boolean(TRUE));
    g_dbus_connection_emit_signal(connection, nullptr, path.c_str(),
        "org.freedesktop.portal.Request", "Response",
        g_variant_new("(ua{sv})", 0, &values), nullptr);
}

void handle_mock_request(GDBusConnection* connection, const gchar*, const gchar*,
                         const gchar*, const gchar* method, GVariant*,
                         GDBusMethodInvocation* invocation, gpointer) {
    const auto path = "/org/freedesktop/portal/desktop/request/test/" +
                      std::to_string(++requestSequence);
    if (std::string(method) == "Immediate") {
        // Deliberately emit before the method reply to reproduce the subscription race.
        emit_response(connection, path);
        g_dbus_method_invocation_return_value(invocation, g_variant_new("(o)", path.c_str()));
        return;
    }

    g_dbus_method_invocation_return_value(invocation, g_variant_new("(o)", path.c_str()));
    auto delayedPath = new std::pair<GDBusConnection*, std::string>(connection, path);
    g_timeout_add(50, [](gpointer data) -> gboolean {
        auto& response = *static_cast<std::pair<GDBusConnection*, std::string>*>(data);
        emit_response(response.first, response.second);
        delete &response;
        return G_SOURCE_REMOVE;
    }, delayedPath);
}
} // namespace

int main() {
    // The fake portal exists on a private test bus; no real desktop permission,
    // input, capture, or user session is involved in this transport regression.
    GTestDBus* testBus = g_test_dbus_new(G_TEST_DBUS_NONE);
    g_test_dbus_up(testBus);
    GDBusConnection* connection = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, nullptr);
    GVariant* nameReply = g_dbus_connection_call_sync(connection, "org.freedesktop.DBus",
        "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
        g_variant_new("(su)", "org.freedesktop.portal.Desktop", 0),
        G_VARIANT_TYPE("(u)"), G_DBUS_CALL_FLAGS_NONE, 1000, nullptr, nullptr);
    if (!nameReply) return 1;
    g_variant_unref(nameReply);

    const char* interfaceXml =
        "<node><interface name='org.desktopcomputeruse.Test'>"
        "<method name='Immediate'><arg type='o' direction='out'/></method>"
        "<method name='Delayed'><arg type='o' direction='out'/></method>"
        "</interface></node>";
    GDBusNodeInfo* node = g_dbus_node_info_new_for_xml(interfaceXml, nullptr);
    GDBusInterfaceVTable callbacks{};
    callbacks.method_call = handle_mock_request;
    const guint registration = g_dbus_connection_register_object(connection,
        "/org/freedesktop/portal/desktop", node->interfaces[0], &callbacks,
        nullptr, nullptr, nullptr);
    GMainLoop* serviceLoop = g_main_loop_new(nullptr, FALSE);
    std::thread serviceThread([&] { g_main_loop_run(serviceLoop); });

    int exitCode = 0;
    try {
        dcu::SessionBus missingShell;
        if (missingShell.available()) throw std::runtime_error("Unowned GNOME proxy reported ready");
        dcu::GVariantRef shellName(g_dbus_connection_call_sync(connection, "org.freedesktop.DBus",
            "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
            g_variant_new("(su)", "org.desktopcomputeruse.Shell", 0),
            G_VARIANT_TYPE("(u)"), G_DBUS_CALL_FLAGS_NONE, 1000, nullptr, nullptr));
        if (!shellName.get()) throw std::runtime_error("Cannot register fake GNOME shell");
        dcu::SessionBus runningShell;
        if (!runningShell.available()) throw std::runtime_error("Owned GNOME service reported missing");
        if (!missingShell.available()) throw std::runtime_error("GNOME readiness retained a stale missing owner");
        {
            dcu::ShellStopSubscription stopMonitor(connection);
            g_dbus_connection_emit_signal(connection, nullptr, "/org/desktopcomputeruse/Shell",
                "org.desktopcomputeruse.Shell", "Stopped", g_variant_new("(s)", "escape"), nullptr);
            const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(1);
            while (!stopMonitor.poll_stopped() && std::chrono::steady_clock::now() < deadline) {
                std::this_thread::sleep_for(std::chrono::milliseconds(5));
            }
            if (!stopMonitor.poll_stopped()) throw std::runtime_error("GNOME stop signal was lost");
        }
        dcu::GVariantRef releasedName(g_dbus_connection_call_sync(connection, "org.freedesktop.DBus",
            "/org/freedesktop/DBus", "org.freedesktop.DBus", "ReleaseName",
            g_variant_new("(s)", "org.desktopcomputeruse.Shell"),
            G_VARIANT_TYPE("(u)"), G_DBUS_CALL_FLAGS_NONE, 1000, nullptr, nullptr));
        if (!releasedName.get() || runningShell.available()) {
            throw std::runtime_error("GNOME readiness retained a departed owner");
        }

        dcu::PortalBus portal;
        dcu::Context cancellation;
        bool consumed = false;
        portal.request_dict("org.desktopcomputeruse.Test", "Immediate", nullptr,
            &cancellation, [&](GVariant* values) {
                gboolean verified = FALSE;
                consumed = g_variant_lookup(values, "verified", "b", &verified) && verified;
            }, 1000);
        if (!consumed) throw std::runtime_error("Immediate response was lost");

        std::jthread cancelRequest([&] {
            std::this_thread::sleep_for(std::chrono::milliseconds(10));
            cancellation.cancelled = true;
        });
        bool rejectedCancellation = false;
        try {
            portal.request_dict("org.desktopcomputeruse.Test", "Delayed", nullptr,
                                &cancellation, [](GVariant*) {}, 1000);
        } catch (const dcu::Error& error) {
            rejectedCancellation = error.code == "cancelled";
        }
        cancelRequest.join();
        if (!rejectedCancellation) throw std::runtime_error("Cancelled request was accepted");

        std::this_thread::sleep_for(std::chrono::milliseconds(100));
        cancellation.cancelled = false;
        consumed = false;
        portal.request_dict("org.desktopcomputeruse.Test", "Immediate", nullptr,
                            &cancellation, [&](GVariant*) { consumed = true; }, 1000);
        if (!consumed) throw std::runtime_error("Request after cancellation failed");
        std::cout << "GNOME stop signal, immediate response, and late response after cancellation passed\n";
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        exitCode = 1;
    }

    g_main_loop_quit(serviceLoop);
    serviceThread.join();
    g_main_loop_unref(serviceLoop);
    g_dbus_connection_unregister_object(connection, registration);
    g_dbus_node_info_unref(node);
    g_object_unref(connection);
    g_test_dbus_down(testBus);
    g_object_unref(testBus);
    return exitCode;
}
