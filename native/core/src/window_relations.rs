/// Platform-neutral view of how listed windows own each other. `modal` marks a
/// dialog that blocks input to its owner: on Windows an owned window whose
/// owner is disabled, on Linux a transient window of a modal-dialog type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowRelation {
    pub id: String,
    pub owner_id: String,
    pub modal: bool,
}

/// Returns the index of the dialog that currently receives input instead of
/// `target_id`, following nested modals (a dialog opened from a dialog) to the
/// deepest one. Windows are expected in front-to-back order, so the first modal
/// child of a window is its frontmost one.
pub fn find_blocking_modal(windows: &[WindowRelation], target_id: &str) -> Option<usize> {
    const MAXIMUM_DEPTH: usize = 8;
    let mut found = None;
    let mut visited = vec![target_id];
    let mut current = target_id;
    for _ in 0..MAXIMUM_DEPTH {
        let Some(child) = windows.iter().position(|window| {
            window.modal && !window.owner_id.is_empty() && window.owner_id == current
        }) else {
            break;
        };
        let next = windows[child].id.as_str();
        if visited.contains(&next) {
            break;
        }
        found = Some(child);
        visited.push(next);
        current = next;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: &str, owner_id: &str, modal: bool) -> WindowRelation {
        WindowRelation {
            id: id.into(),
            owner_id: owner_id.into(),
            modal,
        }
    }

    #[test]
    fn no_windows_means_no_modal() {
        assert_eq!(find_blocking_modal(&[], "main"), None);
    }

    #[test]
    fn modeless_owned_window_is_not_a_modal() {
        let modeless = [window("palette", "main", false), window("main", "", false)];

        assert_eq!(find_blocking_modal(&modeless, "main"), None);
    }

    #[test]
    fn first_modal_child_of_the_target_is_found() {
        let single = [
            window("other-app-dialog", "other", true),
            window("dialog", "main", true),
            window("palette", "main", true),
            window("main", "", false),
        ];

        assert_eq!(find_blocking_modal(&single, "main"), Some(1));
        assert_eq!(
            find_blocking_modal(&single, "dialog"),
            None,
            "the modal itself is not blocked"
        );
        assert_eq!(
            find_blocking_modal(&single, "unrelated"),
            None,
            "an unrelated window is not blocked"
        );
    }

    #[test]
    fn nested_modal_resolves_to_the_deepest_dialog() {
        let nested = [
            window("confirm", "settings", true),
            window("settings", "main", true),
            window("main", "", false),
        ];

        assert_eq!(find_blocking_modal(&nested, "main"), Some(0));
        assert_eq!(
            find_blocking_modal(&nested, "settings"),
            Some(0),
            "intermediate dialog is blocked too"
        );
    }

    #[test]
    fn ownership_cycles_stop_at_the_first_repeat() {
        let cycle = [window("a", "b", true), window("b", "a", true)];
        let self_owned = [window("a", "a", true)];

        assert_eq!(find_blocking_modal(&cycle, "a"), Some(1));
        assert_eq!(find_blocking_modal(&self_owned, "a"), None);
    }
}
