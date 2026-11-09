//! Native setting diagnostics retain bytes while adding the standard install hint.
use roastedduck_function::extension_entries::{add_install_hint, add_install_hint_bytes};
#[test]
fn native_hint_preserves_every_byte_and_text_contract() {
    for extension in ["httpfs", "custom_extension"] {
        for autoload in [false, true] {
            for autoinstall in [false, true] {
                let suffix = add_install_hint("", extension, autoload, autoinstall);
                for byte in 0..=255u8 {
                    let base = [b'm', byte];
                    let actual = add_install_hint_bytes(&base, extension, autoload, autoinstall);
                    let mut expected = base.to_vec();
                    expected.extend_from_slice(suffix.as_bytes());
                    assert_eq!(actual.as_bytes(), expected);
                }
                assert_eq!(
                    add_install_hint_bytes("雪".as_bytes(), extension, autoload, autoinstall).as_bytes(),
                    add_install_hint("雪", extension, autoload, autoinstall).as_bytes()
                );
            }
        }
    }
}

#[path = "../examples/native_install_hints.rs"]
#[allow(dead_code, reason = "reuse oracle comparator")]
mod codec;
#[test]
fn frozen_native_install_hints_match_cpp() {
    for line in include_str!("data/native_install_hints_1.5.6.tsv").lines() {
        codec::compare(line).unwrap();
    }
}
