#![cfg(target_os = "linux")]

use glib::variant::ToVariant;

#[test]
fn string_variant_iteration_preserves_ffi_output_pointer() {
    let variant = ["first", "second", "third", "fourth", "fifth"].to_variant();
    let mut iter = variant.array_iter_str().unwrap();
    assert_eq!(iter.next(), Some("first"));
    assert_eq!(iter.next_back(), Some("fifth"));
    assert_eq!(iter.nth(1), Some("third"));
    assert_eq!(iter.nth_back(0), Some("fourth"));
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next_back(), None);
    assert_eq!(variant.array_iter_str().unwrap().last(), Some("fifth"));
    assert_eq!(
        variant.array_iter_str().unwrap().collect::<Vec<_>>(),
        ["first", "second", "third", "fourth", "fifth"]
    );
}
