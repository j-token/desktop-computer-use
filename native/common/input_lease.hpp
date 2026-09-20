#pragma once
#include "dcu/backend.hpp"
#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <sddl.h>
#include <vector>
#else
#include <fcntl.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace dcu {
// Independent of the transport endpoint: alternative runtime directories must
// not allow two daemons to inject input concurrently. Kernel lifetime handles
// release the lease after a process crash, without a stale PID-file takeover.
class InputLease {
#ifdef _WIN32
    HANDLE desktopMutex_ = nullptr;

    static std::wstring current_user_mutex_name() {
        HANDLE processToken = nullptr;
        if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &processToken)) {
            throw Error("session_lock_failed", "Cannot identify desktop owner");
        }
        struct TokenCloser {
            HANDLE token;
            ~TokenCloser() { CloseHandle(token); }
        } tokenCloser{processToken};

        DWORD tokenInformationBytes = 0;
        GetTokenInformation(processToken, TokenUser, nullptr, 0, &tokenInformationBytes);
        if (tokenInformationBytes == 0) {
            throw Error("session_lock_failed", "Cannot identify desktop owner");
        }
        std::vector<unsigned char> tokenInformation(tokenInformationBytes);
        if (!GetTokenInformation(processToken, TokenUser, tokenInformation.data(),
                                 tokenInformationBytes, &tokenInformationBytes)) {
            throw Error("session_lock_failed", "Cannot identify desktop owner");
        }

        const auto user = reinterpret_cast<TOKEN_USER*>(tokenInformation.data());
        LPWSTR userSid = nullptr;
        if (!ConvertSidToStringSidW(user->User.Sid, &userSid)) {
            throw Error("session_lock_failed", "Cannot identify desktop owner");
        }
        struct SidCloser {
            LPWSTR sid;
            ~SidCloser() { LocalFree(sid); }
        } sidCloser{userSid};
        return std::wstring(L"Local\\DesktopComputerUse.Input.") + userSid;
    }
#else
    int lockDescriptor_ = -1;
#endif
public:
    InputLease() = default;
    InputLease(const InputLease&) = delete;
    InputLease& operator=(const InputLease&) = delete;
    ~InputLease() { release(); }

    void acquire() {
#ifdef _WIN32
        if (desktopMutex_) throw Error("session_busy", "This daemon already owns desktop input");
        const auto mutexName = current_user_mutex_name();
        // The object's existence is the process lease. No thread owns this
        // mutex, so session cleanup must close the handle, not ReleaseMutex.
        HANDLE mutex = CreateMutexW(nullptr, FALSE, mutexName.c_str());
        const auto creationError = GetLastError();
        if (!mutex) throw Error("session_lock_failed", "Cannot create desktop input lease");
        if (creationError == ERROR_ALREADY_EXISTS) {
            CloseHandle(mutex);
            throw Error("session_busy", "Another daemon owns desktop input");
        }
        desktopMutex_ = mutex;
#else
        if (lockDescriptor_ >= 0) throw Error("session_busy", "This daemon already owns desktop input");
        const auto lockPath = "/tmp/desktop-computer-use-input-" + std::to_string(getuid()) + ".lock";
        const int descriptor = open(lockPath.c_str(), O_CREAT | O_RDWR | O_CLOEXEC | O_NOFOLLOW, 0600);
        if (descriptor < 0) {
            throw Error("session_lock_failed", "Cannot create private desktop input lease");
        }
        struct stat lockInformation{};
        const bool metadataAvailable = fstat(descriptor, &lockInformation) == 0;
        const bool ownedRegularFile = lockInformation.st_uid == getuid() && S_ISREG(lockInformation.st_mode);
        if (!metadataAvailable || !ownedRegularFile) {
            close(descriptor);
            throw Error("session_lock_failed", "Cannot create private desktop input lease");
        }
        if (flock(descriptor, LOCK_EX | LOCK_NB)) {
            close(descriptor);
            throw Error("session_busy", "Another daemon owns desktop input");
        }
        lockDescriptor_ = descriptor;
#endif
    }

    void release() noexcept {
#ifdef _WIN32
        if (!desktopMutex_) return;
        CloseHandle(desktopMutex_);
        desktopMutex_ = nullptr;
#else
        if (lockDescriptor_ < 0) return;
        close(lockDescriptor_);
        lockDescriptor_ = -1;
#endif
    }
};
}
