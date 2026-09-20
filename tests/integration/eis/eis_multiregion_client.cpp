#include "eis_client.hpp"
#include "dcu/backend.hpp"
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <cstring>
#include <iostream>
int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "/tmp/dcu-eis-real.sock";
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) return 2;
    sockaddr_un address{};
    address.sun_family = AF_UNIX;
    std::strncpy(address.sun_path, path, sizeof(address.sun_path) - 1);
    if (connect(fd, reinterpret_cast<sockaddr*>(&address), sizeof(address)) != 0) return 3;
    dcu::Context context;
    dcu::EisClient client;
    try {
        client.connect(fd, context);
        client.absolute_global(901.5, 202.5, context);
        client.close();
    } catch (const dcu::Error &error) {
        std::cerr << error.code << ": " << error.what() << "\n";
        return 4;
    }
    close(fd);
    return 0;
}