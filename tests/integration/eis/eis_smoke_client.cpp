#include "eis_client.hpp"
#include "dcu/backend.hpp"
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <iostream>
#include <cstring>
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
    client.absolute(1.5, 2.5, "demo", context);
    client.button(272, true, context);
    client.scroll(0.0, -3.0, context);
    client.key(30, true, context);
    client.release();
    client.close();
  } catch (const dcu::Error &e) {
    std::cerr << e.code << ": " << e.what() << "\n";
    return 4;
  }
  close(fd);
  return 0;
}

