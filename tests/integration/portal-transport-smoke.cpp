#include "portal_bus.hpp"
#include <iostream>

int main() {
    dcu::PortalBus portal;
    if (!portal.available()) {
        std::cerr << portal.error() << '\n';
        return 1;
    }
    std::cout << "Portal session bus connection available\n";
    return 0;
}
