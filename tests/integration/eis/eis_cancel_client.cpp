#include "eis_client.hpp"
#include "dcu/backend.hpp"
#include <sys/socket.h>
#include <unistd.h>
#include <chrono>
#include <iostream>
#include <thread>
int main() {
    int sockets[2] = {-1, -1};
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) != 0) return 2;
    dcu::Context context;
    dcu::EisClient client;
    bool cancelled = false;
    std::thread worker([&] {
        try { client.connect(sockets[0], context); }
        catch (const dcu::Error& error) { cancelled = error.code == "cancelled"; }
    });
    std::this_thread::sleep_for(std::chrono::milliseconds(100));
    context.cancelled.store(true);
    worker.join();
    close(sockets[0]);
    close(sockets[1]);
    std::cout << (cancelled ? "cancelled\n" : "unexpected\n");
    return cancelled ? 0 : 1;
}