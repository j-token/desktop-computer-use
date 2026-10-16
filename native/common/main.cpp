#include "dispatch.hpp"
#include <fstream>
#include <iostream>
#ifdef _WIN32
#include <windows.h>
#include <roapi.h>
#endif

int main(int argc, char** argv) {
    try {
#ifdef _WIN32
        // DWM frame bounds and captured frames are always physical pixels, while
        // UIA bounds and SetCursorPos follow the caller's DPI awareness. Without
        // per-monitor awareness a scaled display mixes the two spaces and every
        // click lands off target.
        if (!SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)) {
            std::cerr << "Per-monitor DPI awareness unavailable (" << GetLastError()
                      << "); input may miss its target on scaled displays\n";
        }
        if (argc == 4 && std::string(argv[1]) == "--input-watchdog") {
            return dcu::windows::run_input_watchdog(static_cast<std::uint32_t>(std::stoul(argv[2])), argv[3]);
        }
#endif
        std::string endpoint, tokenFile;
        bool server = false;
        for (int i = 1; i < argc; ++i) {
            std::string arg = argv[i];
            if (arg == "--version") { std::cout << "0.1.0\n"; return 0; }
            if (arg == "--help") { std::cout << "desktop-computer-use-native --serve --endpoint PATH --token-file PATH\n"; return 0; }
            if (arg == "--serve") server = true;
            else if (arg == "--endpoint" && i + 1 < argc) endpoint = argv[++i];
            else if (arg == "--token-file" && i + 1 < argc) tokenFile = argv[++i];
            else throw dcu::Error("invalid_argument", "Unknown or incomplete native argument: " + arg);
        }
        if (!server || endpoint.empty() || tokenFile.empty()) throw dcu::Error("invalid_argument", "Use --help for daemon arguments");
        std::ifstream file(tokenFile);
        std::string token;
        std::getline(file, token);
        if (!token.empty() && token.back() == '\r') token.pop_back();
        if (!file || token.size() < 32 || token.size() > 256) throw dcu::Error("invalid_environment", "Missing or invalid private token file");
#ifdef _WIN32
        // WGC/UIA objects outlive individual connection worker threads. Keep
        // their shared MTA alive until serve destroys the persistent backend.
        const HRESULT initialized = RoInitialize(RO_INIT_MULTITHREADED);
        if (FAILED(initialized)) throw dcu::Error("initialization_failed", "Cannot initialize persistent Windows Runtime apartment");
        struct RuntimeApartment { ~RuntimeApartment() { RoUninitialize(); } } apartment;
#endif
        return dcu::serve(endpoint, token);
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
