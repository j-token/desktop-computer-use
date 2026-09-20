#pragma once

#include <cstdint>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

namespace dcu {

struct Context;

/**
 * Small RAII wrapper around the sender side of libei (EI) used to talk to an
 * EIS server.  The implementation loads libei.so.1 at runtime so that the
 * Linux binary remains usable on Ubuntu installations without libei-dev and
 * can still fall back to the portal's legacy Notify* path in its caller.
 *
 * The file descriptor passed to connect() is duplicated.  The caller keeps
 * ownership of its descriptor; the duplicate is owned by libei after a
 * successful setup.
 *
 * One EisClient is intended to be used by one serialized backend worker.  A
 * Context is checked before every potentially blocking operation and while
 * waiting for the EIS handshake.  close() and release() are noexcept and are
 * safe to call during exception unwinding.
 */
class EisClient {
public:
    struct Region {
        std::string mapping_id;
        // EIS logical desktop coordinates. Width and height use the same
        // logical-pixel units as x and y.
        double x = 0.0;
        double y = 0.0;
        double width = 0.0;
        double height = 0.0;
    };

    EisClient();
    ~EisClient();

    EisClient(const EisClient&) = delete;
    EisClient& operator=(const EisClient&) = delete;
    EisClient(EisClient&&) noexcept;
    EisClient& operator=(EisClient&&) noexcept;

    /**
     * Attach to an already-authorized EIS socket returned by ConnectToEIS.
     * This performs the EI handshake, binds pointer/button/keyboard/scroll
     * capabilities, and waits for at least one resumed absolute device.
     */
    void connect(int fd, Context& context);

    bool connected() const noexcept;
    bool ready() const noexcept;
    bool mapping_ids_available() const noexcept;
    std::vector<Region> regions() const;

    /** Send one absolute point in the selected region's local coordinates. */
    void absolute(double x, double y, std::string_view mapping_id, Context& context);

    /** Send a desktop-logical point after selecting its containing region. */
    void absolute_global(double x, double y, Context& context);

    void button(std::uint32_t evdev_code, bool pressed, Context& context);
    void key(std::uint32_t evdev_keycode, bool pressed, Context& context);
    void scroll(double dx, double dy, Context& context);

    /** Release held keys/buttons while retaining the EIS connection. */
    void release() noexcept;

    /** Release input and tear down the EI context. */
    void close() noexcept;

private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

} // namespace dcu
