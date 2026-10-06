#pragma once
#include <cstddef>
#include <optional>
#include <string>
#include <vector>

namespace dcu {

// Platform-neutral view of how listed windows own each other.  `modal` marks
// a dialog that blocks input to its owner: on Windows an owned window whose
// owner is disabled, on Linux a transient window of a modal-dialog type.
struct WindowRelation {
    std::string id;
    std::string ownerId;
    bool modal = false;
};

// Returns the index of the dialog that currently receives input instead of
// `targetId`, following nested modals (a dialog opened from a dialog) to the
// deepest one.  Windows are expected in front-to-back order, so the first
// modal child of a window is its frontmost one.
inline std::optional<std::size_t> find_blocking_modal(const std::vector<WindowRelation>& windows,
                                                      const std::string& targetId) {
    constexpr int maximumDepth = 8;
    std::optional<std::size_t> found;
    std::vector<std::string> visited{targetId};
    std::string current = targetId;
    for (int depth = 0; depth < maximumDepth; ++depth) {
        std::optional<std::size_t> child;
        for (std::size_t index = 0; index < windows.size(); ++index) {
            const auto& window = windows[index];
            if (window.modal && !window.ownerId.empty() && window.ownerId == current) {
                child = index;
                break;
            }
        }
        if (!child) break;
        const auto& next = windows[*child].id;
        bool cycle = false;
        for (const auto& seen : visited) cycle = cycle || seen == next;
        if (cycle) break;
        found = child;
        visited.push_back(next);
        current = next;
    }
    return found;
}

} // namespace dcu
