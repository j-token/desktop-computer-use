#include "dcu/window_relations.hpp"

#include <cstdlib>
#include <iostream>

namespace {
void require(bool condition, const char* message) {
    if (!condition) {
        std::cerr << "FAILED: " << message << '\n';
        std::exit(1);
    }
}
} // namespace

int main() {
    using dcu::find_blocking_modal;
    using dcu::WindowRelation;

    require(!find_blocking_modal({}, "main"), "no windows means no modal");

    // A modeless owned window (palette, find dialog) does not block its owner.
    const std::vector<WindowRelation> modeless{{"palette", "main", false}, {"main", "", false}};
    require(!find_blocking_modal(modeless, "main"), "modeless owned window is not a modal");

    // Owned windows precede their owner; the first modal child wins.
    const std::vector<WindowRelation> single{
        {"other-app-dialog", "other", true},
        {"dialog", "main", true},
        {"palette", "main", true},
        {"main", "", false}};
    const auto found = find_blocking_modal(single, "main");
    require(found && single[*found].id == "dialog", "modal dialog of the target is found");
    require(!find_blocking_modal(single, "dialog"), "the modal itself is not blocked");
    require(!find_blocking_modal(single, "unrelated"), "an unrelated window is not blocked");

    // A dialog opened from a dialog: input goes to the deepest one.
    const std::vector<WindowRelation> nested{
        {"confirm", "settings", true}, {"settings", "main", true}, {"main", "", false}};
    const auto deepest = find_blocking_modal(nested, "main");
    require(deepest && nested[*deepest].id == "confirm", "nested modal resolves to the deepest dialog");
    const auto fromMiddle = find_blocking_modal(nested, "settings");
    require(fromMiddle && nested[*fromMiddle].id == "confirm", "intermediate dialog is blocked too");

    // Malformed ownership cycles terminate.
    const std::vector<WindowRelation> cycle{{"a", "b", true}, {"b", "a", true}};
    const auto cyclic = find_blocking_modal(cycle, "a");
    require(cyclic && cycle[*cyclic].id == "b", "ownership cycle stops at the first repeat");
    const std::vector<WindowRelation> self{{"a", "a", true}};
    require(!find_blocking_modal(self, "a"), "self-owned window is ignored");

    std::cout << "window relation tests passed\n";
    return 0;
}
