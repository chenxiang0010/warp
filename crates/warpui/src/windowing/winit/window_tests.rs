use pathfinder_geometry::vector::vec2f;

use super::size_for_layout;

#[test]
fn minimizing_reports_the_last_real_size_instead_of_the_shrunken_client_rect() {
    let full_size = vec2f(2560., 1392.);
    let minimized_client_rect = vec2f(160., 28.);

    let (size, remembered) = size_for_layout(true, minimized_client_rect, full_size);

    assert_eq!(size, full_size);
    assert_eq!(remembered, full_size);
}

#[test]
fn restoring_reports_the_new_size_and_remembers_it() {
    let remembered_from_before_minimize = vec2f(2560., 1392.);
    let restored_size = vec2f(1280., 800.);

    let (size, remembered) = size_for_layout(false, restored_size, remembered_from_before_minimize);

    assert_eq!(size, restored_size);
    assert_eq!(remembered, restored_size);
}

#[test]
fn a_minimize_before_any_size_was_seen_reports_nothing_to_lay_out_against() {
    let minimized_client_rect = vec2f(160., 28.);

    let (size, remembered) = size_for_layout(true, minimized_client_rect, vec2f(0., 0.));

    // The minimized client rect is a lie either way, so report nothing rather than let layout
    // clamp a persisted panel width against it.
    assert_eq!(size, vec2f(0., 0.));
    assert_eq!(remembered, vec2f(0., 0.));
}

#[test]
fn a_zero_sized_unminimized_window_does_not_overwrite_the_remembered_size() {
    let remembered_from_before = vec2f(2560., 1392.);

    let (size, remembered) = size_for_layout(false, vec2f(0., 0.), remembered_from_before);

    assert_eq!(size, vec2f(0., 0.));
    assert_eq!(remembered, remembered_from_before);
}
