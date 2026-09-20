#include "dispatch.hpp"
#include <algorithm>
#include <fstream>
#include <iostream>
#include <thread>
#include <vector>
#include <filesystem>
#include <cstring>
#include <condition_variable>
#include <csignal>
#ifdef _WIN32
#include <windows.h>
#include <sddl.h>
#else
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/stat.h>
#include <poll.h>
#include <unistd.h>
#include <cerrno>
#endif

namespace dcu {
namespace {
constexpr std::size_t maximumFrameBytes = 16 * 1024 * 1024;
constexpr auto streamRetryInterval = std::chrono::milliseconds(2);
// A nonblocking pipe write of more than the 64 KiB output buffer writes nothing,
// so each write must stay comfortably inside it or the response never leaves.
constexpr std::size_t writeChunkBytes = 16 * 1024;
constexpr auto pipeAcceptInterval = std::chrono::milliseconds(5);
volatile std::sig_atomic_t terminateRequested = 0;
void request_termination(int) { terminateRequested = 1; }
#ifdef _WIN32
using Connection = HANDLE;
void close_connection(Connection connection) { DisconnectNamedPipe(connection); CloseHandle(connection); }
bool disconnected(Connection connection) { DWORD available = 0; return !PeekNamedPipe(connection, nullptr, 0, nullptr, &available, nullptr); }
int read_bytes(Connection connection, char* buffer, int size) {
    DWORD available = 0, read = 0;
    if (!PeekNamedPipe(connection, nullptr, 0, nullptr, &available, nullptr)) return 0;
    if (!available) return -1;
    if (!ReadFile(connection, buffer, std::min<DWORD>(available, size), &read, nullptr)) return 0;
    return static_cast<int>(read);
}
int write_bytes(Connection connection, const char* buffer, int size) {
    DWORD written = 0;
    if (!WriteFile(connection, buffer, size, &written, nullptr)) return 0;
    return written ? static_cast<int>(written) : -1;
}
struct PrivateSecurity {
    PSECURITY_DESCRIPTOR descriptor = nullptr;
    SECURITY_ATTRIBUTES attributes{sizeof(SECURITY_ATTRIBUTES), nullptr, FALSE};
    PrivateSecurity() {
        HANDLE token = nullptr;
        if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token)) throw Error("invalid_environment", "Cannot inspect Windows user token");
        DWORD size = 0;
        GetTokenInformation(token, TokenUser, nullptr, 0, &size);
        std::vector<unsigned char> buffer(size);
        const auto ok = GetTokenInformation(token, TokenUser, buffer.data(), size, &size);
        CloseHandle(token);
        if (!ok) throw Error("invalid_environment", "Cannot inspect Windows user SID");
        LPWSTR sid = nullptr;
        if (!ConvertSidToStringSidW(reinterpret_cast<TOKEN_USER*>(buffer.data())->User.Sid, &sid)) throw Error("invalid_environment", "Cannot format user SID");
        std::wstring sddl = L"D:P(A;;GA;;;" + std::wstring(sid) + L")";
        LocalFree(sid);
        if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.c_str(), SDDL_REVISION_1, &descriptor, nullptr)) throw Error("invalid_environment", "Cannot restrict pipe access");
        attributes.lpSecurityDescriptor = descriptor;
    }
    ~PrivateSecurity() { if (descriptor) LocalFree(descriptor); }
};
std::wstring widen(const std::string& value) {
    const auto size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.c_str(), -1, nullptr, 0);
    if (!size) throw Error("invalid_argument", "Invalid UTF-8 endpoint");
    std::wstring result(size, L'\0');
    MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.c_str(), -1, result.data(), size);
    result.pop_back();
    return result;
}
#else
using Connection = int;
void close_connection(Connection connection) { close(connection); }
bool disconnected(Connection connection) { char byte; const auto n = recv(connection, &byte, 1, MSG_PEEK | MSG_DONTWAIT); return n == 0 || (n < 0 && errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR); }
int read_bytes(Connection connection, char* buffer, int size) {
    const auto n = recv(connection, buffer, size, MSG_DONTWAIT);
    if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)) return -1;
    return n <= 0 ? 0 : static_cast<int>(n);
}
int write_bytes(Connection connection, const char* buffer, int size) {
    const auto n = send(connection, buffer, size, MSG_NOSIGNAL | MSG_DONTWAIT);
    if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)) return -1;
    return n <= 0 ? 0 : static_cast<int>(n);
}
#endif
Json read_request(Connection connection, const Dispatcher& dispatcher) {
    std::string frame;
    char buffer[8192];
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(15);
    while (frame.find('\n') == std::string::npos) {
        if (dispatcher.quitting || std::chrono::steady_clock::now() > deadline) {
            throw Error("transport_timeout", "Request frame timed out");
        }
        const auto bytesRead = read_bytes(connection, buffer, sizeof buffer);
        if (bytesRead == 0) throw Error("transport_closed", "Client disconnected");
        if (bytesRead < 0) {
            std::this_thread::sleep_for(streamRetryInterval);
            continue;
        }
        frame.append(buffer, bytesRead);
        if (frame.size() > maximumFrameBytes) {
            throw Error("invalid_argument", "Request exceeds 16 MiB");
        }
    }
    return Json::parse(frame.substr(0, frame.find('\n')));
}

bool should_cancel_on_disconnect(const Json& request, const std::string& token) {
    if (!request.is_object() || request.value("token", Json(nullptr)) != token) return false;
    if (!request.contains("method") || !request["method"].is_string()) return false;
    const auto& method = request["method"].get_ref<const std::string&>();
    return method != "doctor" && method != "capabilities" && method != "session.status";
}

void write_response(Connection connection, const std::string& response) {
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(15);
    std::size_t bytesSent = 0;
    while (bytesSent < response.size() && std::chrono::steady_clock::now() < deadline) {
        const auto bytesWritten = write_bytes(connection, response.data() + bytesSent,
                                              static_cast<int>(std::min(response.size() - bytesSent, writeChunkBytes)));
        if (bytesWritten == 0) {
            std::cerr << "transport: response truncated at " << bytesSent << " of " << response.size() << " bytes\n";
            break;
        }
        if (bytesWritten < 0) {
            std::this_thread::sleep_for(streamRetryInterval);
            continue;
        }
        bytesSent += bytesWritten;
    }
#ifdef _WIN32
    // DisconnectNamedPipe discards unread output. Clients close after parsing
    // the complete response; wait for that acknowledgement before teardown.
    while (bytesSent == response.size() && !disconnected(connection) &&
           std::chrono::steady_clock::now() < deadline) {
        std::this_thread::sleep_for(std::chrono::milliseconds(2));
    }
#endif
}

void service_connection(Connection connection, Dispatcher& dispatcher, const std::string& token) {
    try {
        const auto request = read_request(connection, dispatcher);
        // A lost input owner cancels its operation instead of allowing late input.
        std::atomic_bool finished{false};
        std::mutex watchMutex;
        std::condition_variable watchWake;
        const bool cancelOnDisconnect = should_cancel_on_disconnect(request, token);
        std::jthread watcher([&](std::stop_token stop) {
            while (!finished.load() && !stop.stop_requested()) {
                std::unique_lock lock(watchMutex);
                watchWake.wait_for(lock, std::chrono::milliseconds(100), [&] { return finished.load() || stop.stop_requested(); });
                if (!finished && cancelOnDisconnect && disconnected(connection)) {
                    dispatcher.shutdown();
                    break;
                }
            }
        });
        auto response = dispatcher.handle(request).dump() + "\n";
        finished = true;
        watchWake.notify_all();
        watcher.join();
        if (response.size() > maximumFrameBytes) {
            response = Json{
                {"id", request.value("id", Json(nullptr))}, {"ok", false},
                {"error", {{"code", "response_too_large"}, {"message", "Response exceeds 16 MiB"}}}
            }.dump() + "\n";
        }
        write_response(connection, response);
    } catch (const std::exception& error) {
        std::cerr << "transport: " << error.what() << '\n';
    }
    close_connection(connection);
}
}
int serve(const std::string& endpoint, const std::string& token) {
    auto backend = make_backend();
    Dispatcher dispatcher(*backend, token);
    std::signal(SIGINT, request_termination);
    std::signal(SIGTERM, request_termination);
    std::atomic_int activeClients{0};
#ifdef _WIN32
    if (!endpoint.starts_with("\\\\.\\pipe\\")) throw Error("invalid_argument", "Expected local named pipe endpoint");
    const auto name = widen(endpoint);
    PrivateSecurity security;
    const auto mutexName = L"Local\\dcu-" + std::to_wstring(std::hash<std::string>{}(endpoint));
    HANDLE singleton = CreateMutexW(&security.attributes, TRUE, mutexName.c_str());
    if (!singleton || GetLastError() == ERROR_ALREADY_EXISTS) { if(singleton) CloseHandle(singleton); throw Error("daemon_running", "An endpoint owner already exists"); }
    while (!dispatcher.quitting) {
        if (terminateRequested) { dispatcher.quitting = true; break; }
        dispatcher.tick();
        HANDLE pipe = CreateNamedPipeW(name.c_str(), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS, 32, 65536, 65536, 0, &security.attributes);
        if (pipe == INVALID_HANDLE_VALUE) { ReleaseMutex(singleton); CloseHandle(singleton); throw Error("transport_error", "Cannot create private pipe"); }
        bool connected = false;
        while (!dispatcher.quitting) {
            if (terminateRequested) { dispatcher.quitting = true; break; }
            if (ConnectNamedPipe(pipe, nullptr)) { connected = true; break; }
            const auto error = GetLastError();
            if (error == ERROR_PIPE_CONNECTED) { connected = true; break; }
            if (error != ERROR_PIPE_LISTENING) break;
            dispatcher.tick();
            std::this_thread::sleep_for(pipeAcceptInterval);
        }
        if (!connected || activeClients >= 24) { close_connection(pipe); continue; }
        ++activeClients;
        std::thread([&, pipe] { service_connection(pipe, dispatcher, token); --activeClients; }).detach();
    }
    ReleaseMutex(singleton);
    CloseHandle(singleton);
#else
    struct stat parent{};
    const auto directory = std::filesystem::path(endpoint).parent_path();
    if (lstat(directory.c_str(), &parent) || !S_ISDIR(parent.st_mode) || parent.st_uid != getuid() || (parent.st_mode & 0077))
        throw Error("invalid_environment", "Endpoint directory must be owned by this user with mode 0700");
    sockaddr_un address{};
    address.sun_family = AF_UNIX;
    if (endpoint.size() >= sizeof address.sun_path) throw Error("invalid_argument", "Unix socket path is too long");
    std::memcpy(address.sun_path, endpoint.c_str(), endpoint.size() + 1);
    const int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (listener < 0) throw Error("transport_error", "Cannot create Unix socket");
    if (bind(listener, reinterpret_cast<sockaddr*>(&address), sizeof address)) { close(listener); throw Error("daemon_running", "Cannot bind socket; endpoint may already be owned"); }
    chmod(endpoint.c_str(), 0600);
    if (listen(listener, 24)) { close(listener); unlink(endpoint.c_str()); throw Error("transport_error", "Cannot listen on socket"); }
    while (!dispatcher.quitting) {
        if (terminateRequested) { dispatcher.quitting = true; break; }
        dispatcher.tick();
        pollfd fd{listener, POLLIN, 0};
        if (poll(&fd, 1, 100) <= 0) continue;
        const int client = accept4(listener, nullptr, nullptr, SOCK_CLOEXEC);
        if (client < 0) continue;
        struct ucred peer{};
        socklen_t size = sizeof peer;
        if (getsockopt(client, SOL_SOCKET, SO_PEERCRED, &peer, &size) || peer.uid != getuid() || activeClients >= 24) { close(client); continue; }
        ++activeClients;
        std::thread([&, client] { service_connection(client, dispatcher, token); --activeClients; }).detach();
    }
    close(listener);
    unlink(endpoint.c_str());
#endif
    dispatcher.shutdown();
    while (activeClients) std::this_thread::sleep_for(std::chrono::milliseconds(20));
    return 0;
}
}
