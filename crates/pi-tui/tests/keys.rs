//! Port of packages/tui/test/keys.test.ts

use pi_js::env::testing::EnvGuard;
use pi_tui::keys::{
    Key, decode_kitty_printable, decode_printable_key, is_key_release, is_key_repeat, matches_key, parse_key,
    set_kitty_protocol_active,
};
use serial_test::serial;

struct KittyGuard;

impl KittyGuard {
    fn active(active: bool) -> Self {
        set_kitty_protocol_active(active);
        Self
    }
}

impl Drop for KittyGuard {
    fn drop(&mut self) {
        set_kitty_protocol_active(false);
    }
}

// ============================================================================
// matchesKey - Kitty protocol with alternate keys (non-Latin layouts)
// ============================================================================

#[test]
#[serial]
fn matches_ctrl_c_when_pressing_cyrillic_with_base_layout_key() {
    let _kitty = KittyGuard::active(true);
    let cyrillic_ctrl_c = "\x1b[1089::99;5u";
    assert!(matches_key(cyrillic_ctrl_c, "ctrl+c"));
}

#[test]
#[serial]
fn matches_ctrl_d_when_pressing_cyrillic_with_base_layout_key() {
    let _kitty = KittyGuard::active(true);
    let cyrillic_ctrl_d = "\x1b[1074::100;5u";
    assert!(matches_key(cyrillic_ctrl_d, "ctrl+d"));
}

#[test]
#[serial]
fn matches_ctrl_z_when_pressing_cyrillic_with_base_layout_key() {
    let _kitty = KittyGuard::active(true);
    let cyrillic_ctrl_z = "\x1b[1103::122;5u";
    assert!(matches_key(cyrillic_ctrl_z, "ctrl+z"));
}

#[test]
#[serial]
fn matches_ctrl_shift_p_with_base_layout_key() {
    let _kitty = KittyGuard::active(true);
    let cyrillic_ctrl_shift_p = "\x1b[1079::112;6u";
    assert!(matches_key(cyrillic_ctrl_shift_p, "ctrl+shift+p"));
}

#[test]
#[serial]
fn matches_direct_codepoint_when_no_base_layout_key() {
    let _kitty = KittyGuard::active(true);
    let latin_ctrl_c = "\x1b[99;5u";
    assert!(matches_key(latin_ctrl_c, "ctrl+c"));
}

#[test]
#[serial]
fn matches_super_modified_kitty_bindings_including_combined_modifiers() {
    let _kitty = KittyGuard::active(true);
    assert!(matches_key("\x1b[107;9u", "super+k"));
    assert!(matches_key("\x1b[13;9u", "super+enter"));
    assert!(matches_key("\x1b[107;13u", &Key::ctrl_super("k")));
    assert!(matches_key("\x1b[107;13u", "ctrl+super+k"));
    assert!(matches_key("\x1b[107;14u", "ctrl+shift+super+k"));
    assert!(!matches_key("\x1b[107;13u", "super+k"));
    assert_eq!(parse_key("\x1b[107;9u").as_deref(), Some("super+k"));
    assert_eq!(parse_key("\x1b[13;9u").as_deref(), Some("super+enter"));
    assert_eq!(parse_key("\x1b[107;13u").as_deref(), Some("ctrl+super+k"));
    assert_eq!(parse_key("\x1b[107;14u").as_deref(), Some("shift+ctrl+super+k"));
}

#[test]
#[serial]
fn matches_digit_bindings_via_kitty_csi_u() {
    let _kitty = KittyGuard::active(true);
    assert!(matches_key("\x1b[49u", "1"));
    assert!(matches_key("\x1b[49;5u", "ctrl+1"));
    assert!(!matches_key("\x1b[49;5u", "ctrl+2"));
    assert_eq!(parse_key("\x1b[49u").as_deref(), Some("1"));
    assert_eq!(parse_key("\x1b[49;5u").as_deref(), Some("ctrl+1"));
}

#[test]
#[serial]
fn normalizes_kitty_keypad_functional_keys_to_logical_digits_symbols_and_navigation() {
    let _kitty = KittyGuard::active(true);
    assert!(matches_key("\x1b[57400u", "1"));
    assert!(matches_key("\x1b[57410u", "/"));
    assert!(matches_key("\x1b[57417u", "left"));
    assert!(matches_key("\x1b[57426u", "delete"));
    assert_eq!(parse_key("\x1b[57399u").as_deref(), Some("0"));
    assert_eq!(parse_key("\x1b[57409u").as_deref(), Some("."));
    assert_eq!(parse_key("\x1b[57413u").as_deref(), Some("+"));
    assert_eq!(parse_key("\x1b[57416u").as_deref(), Some(","));
    assert_eq!(parse_key("\x1b[57417u").as_deref(), Some("left"));
    assert_eq!(parse_key("\x1b[57418u").as_deref(), Some("right"));
    assert_eq!(parse_key("\x1b[57419u").as_deref(), Some("up"));
    assert_eq!(parse_key("\x1b[57420u").as_deref(), Some("down"));
    assert_eq!(parse_key("\x1b[57421u").as_deref(), Some("pageUp"));
    assert_eq!(parse_key("\x1b[57422u").as_deref(), Some("pageDown"));
    assert_eq!(parse_key("\x1b[57423u").as_deref(), Some("home"));
    assert_eq!(parse_key("\x1b[57424u").as_deref(), Some("end"));
    assert_eq!(parse_key("\x1b[57425u").as_deref(), Some("insert"));
    assert_eq!(parse_key("\x1b[57426u").as_deref(), Some("delete"));
}

#[test]
#[serial]
fn handles_shifted_key_in_format() {
    let _kitty = KittyGuard::active(true);
    let shifted_key = "\x1b[99:67:99;2u";
    assert!(matches_key(shifted_key, "shift+c"));
}

#[test]
#[serial]
fn handles_event_type_in_format() {
    let _kitty = KittyGuard::active(true);
    let release_event = "\x1b[1089::99;5:3u";
    assert!(matches_key(release_event, "ctrl+c"));
}

#[test]
#[serial]
fn handles_full_format_with_shifted_key_base_key_and_event_type() {
    let _kitty = KittyGuard::active(true);
    let full_format = "\x1b[1089:1057:99;6:2u";
    assert!(matches_key(full_format, "ctrl+shift+c"));
}

#[test]
#[serial]
fn prefers_codepoint_for_latin_letters_even_when_base_layout_differs() {
    let _kitty = KittyGuard::active(true);
    let dvorak_ctrl_k = "\x1b[107::118;5u";
    assert!(matches_key(dvorak_ctrl_k, "ctrl+k"));
    assert!(!matches_key(dvorak_ctrl_k, "ctrl+v"));
}

#[test]
#[serial]
fn prefers_codepoint_for_symbol_keys_even_when_base_layout_differs() {
    let _kitty = KittyGuard::active(true);
    let dvorak_ctrl_slash = "\x1b[47::91;5u";
    assert!(matches_key(dvorak_ctrl_slash, "ctrl+/"));
    assert!(!matches_key(dvorak_ctrl_slash, "ctrl+["));
}

#[test]
#[serial]
fn does_not_match_wrong_key_even_with_base_layout() {
    let _kitty = KittyGuard::active(true);
    let cyrillic_ctrl_c = "\x1b[1089::99;5u";
    assert!(!matches_key(cyrillic_ctrl_c, "ctrl+d"));
}

#[test]
#[serial]
fn does_not_match_wrong_modifiers_even_with_base_layout() {
    let _kitty = KittyGuard::active(true);
    let cyrillic_ctrl_c = "\x1b[1089::99;5u";
    assert!(!matches_key(cyrillic_ctrl_c, "ctrl+shift+c"));
}

// ============================================================================
// modifyOtherKeys matching
// ============================================================================

#[test]
#[serial]
fn matches_xterm_modify_other_keys_ctrl_c() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;5;99~", "ctrl+c"));
    assert_eq!(parse_key("\x1b[27;5;99~").as_deref(), Some("ctrl+c"));
}

#[test]
#[serial]
fn matches_xterm_modify_other_keys_ctrl_d() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;5;100~", "ctrl+d"));
    assert_eq!(parse_key("\x1b[27;5;100~").as_deref(), Some("ctrl+d"));
}

#[test]
#[serial]
fn matches_xterm_modify_other_keys_ctrl_z() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;5;122~", "ctrl+z"));
    assert_eq!(parse_key("\x1b[27;5;122~").as_deref(), Some("ctrl+z"));
}

#[test]
#[serial]
fn matches_xterm_modify_other_keys_enter_variants() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;5;13~", "ctrl+enter"));
    assert!(matches_key("\x1b[27;2;13~", "shift+enter"));
    assert!(matches_key("\x1b[27;3;13~", "alt+enter"));
    assert_eq!(parse_key("\x1b[27;5;13~").as_deref(), Some("ctrl+enter"));
    assert_eq!(parse_key("\x1b[27;2;13~").as_deref(), Some("shift+enter"));
    assert_eq!(parse_key("\x1b[27;3;13~").as_deref(), Some("alt+enter"));
}

#[test]
#[serial]
fn matches_xterm_modify_other_keys_tab_variants() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;2;9~", "shift+tab"));
    assert!(matches_key("\x1b[27;5;9~", "ctrl+tab"));
    assert!(matches_key("\x1b[27;3;9~", "alt+tab"));
    assert_eq!(parse_key("\x1b[27;2;9~").as_deref(), Some("shift+tab"));
    assert_eq!(parse_key("\x1b[27;5;9~").as_deref(), Some("ctrl+tab"));
    assert_eq!(parse_key("\x1b[27;3;9~").as_deref(), Some("alt+tab"));
}

#[test]
#[serial]
fn matches_xterm_modify_other_keys_backspace_variants() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;1;127~", "backspace"));
    assert!(matches_key("\x1b[27;5;127~", "ctrl+backspace"));
    assert!(matches_key("\x1b[27;3;127~", "alt+backspace"));
    assert_eq!(parse_key("\x1b[27;1;127~").as_deref(), Some("backspace"));
    assert_eq!(parse_key("\x1b[27;5;127~").as_deref(), Some("ctrl+backspace"));
    assert_eq!(parse_key("\x1b[27;3;127~").as_deref(), Some("alt+backspace"));
}

#[test]
#[serial]
fn matches_xterm_modify_other_keys_escape() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;1;27~", "escape"));
    assert_eq!(parse_key("\x1b[27;1;27~").as_deref(), Some("escape"));
}

#[test]
#[serial]
fn matches_xterm_modify_other_keys_space_variants() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;1;32~", "space"));
    assert!(matches_key("\x1b[27;5;32~", "ctrl+space"));
    assert_eq!(parse_key("\x1b[27;1;32~").as_deref(), Some("space"));
    assert_eq!(parse_key("\x1b[27;5;32~").as_deref(), Some("ctrl+space"));
}

#[test]
#[serial]
fn matches_xterm_modify_other_keys_symbol_combos() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;5;47~", "ctrl+/"));
    assert_eq!(parse_key("\x1b[27;5;47~").as_deref(), Some("ctrl+/"));
}

#[test]
#[serial]
fn matches_xterm_modify_other_keys_digit_combos() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;5;49~", "ctrl+1"));
    assert!(matches_key("\x1b[27;2;49~", "shift+1"));
    assert_eq!(parse_key("\x1b[27;5;49~").as_deref(), Some("ctrl+1"));
    assert_eq!(parse_key("\x1b[27;2;49~").as_deref(), Some("shift+1"));
}

#[test]
#[serial]
fn matches_xterm_modify_other_keys_shifted_uppercase_letters() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;2;69~", "shift+e"));
    assert!(matches_key("\x1b[27;6;69~", "ctrl+shift+e"));
    assert_eq!(parse_key("\x1b[27;2;69~").as_deref(), Some("shift+e"));
    assert_eq!(parse_key("\x1b[27;6;69~").as_deref(), Some("shift+ctrl+e"));
}

#[test]
#[serial]
fn matches_ctrl_alt_letter_via_csi_u_when_kitty_inactive() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[104;7u", "ctrl+alt+h"));
    assert_eq!(parse_key("\x1b[104;7u").as_deref(), Some("ctrl+alt+h"));
}

#[test]
#[serial]
fn matches_ctrl_alt_letter_via_xterm_modify_other_keys() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;7;104~", "ctrl+alt+h"));
    assert_eq!(parse_key("\x1b[27;7;104~").as_deref(), Some("ctrl+alt+h"));
}

// ============================================================================
// Legacy key matching
// ============================================================================

#[test]
#[serial]
fn matches_legacy_ctrl_c() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x03", "ctrl+c"));
}

#[test]
#[serial]
fn matches_legacy_ctrl_d() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x04", "ctrl+d"));
}

#[test]
#[serial]
fn matches_escape_key() {
    assert!(matches_key("\x1b", "escape"));
}

#[test]
#[serial]
fn matches_legacy_linefeed_as_enter() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\n", "enter"));
    assert_eq!(parse_key("\n").as_deref(), Some("enter"));
}

#[test]
#[serial]
fn treats_linefeed_as_shift_enter_when_kitty_active() {
    let _kitty = KittyGuard::active(true);
    assert!(matches_key("\n", "shift+enter"));
    assert!(!matches_key("\n", "enter"));
    assert_eq!(parse_key("\n").as_deref(), Some("shift+enter"));
}

#[test]
#[serial]
fn parses_ctrl_space() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x00", "ctrl+space"));
    assert_eq!(parse_key("\x00").as_deref(), Some("ctrl+space"));
}

#[test]
#[serial]
fn matches_legacy_ctrl_symbol() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1c", "ctrl+\\"));
    assert_eq!(parse_key("\x1c").as_deref(), Some("ctrl+\\"));
    assert!(matches_key("\x1d", "ctrl+]"));
    assert_eq!(parse_key("\x1d").as_deref(), Some("ctrl+]"));
    assert!(matches_key("\x1f", "ctrl+_"));
    assert!(matches_key("\x1f", "ctrl+-"));
    assert_eq!(parse_key("\x1f").as_deref(), Some("ctrl+-"));
}

#[test]
#[serial]
fn matches_legacy_ctrl_alt_symbol() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b\x1b", "ctrl+alt+["));
    assert_eq!(parse_key("\x1b\x1b").as_deref(), Some("ctrl+alt+["));
    assert!(matches_key("\x1b\x1c", "ctrl+alt+\\"));
    assert_eq!(parse_key("\x1b\x1c").as_deref(), Some("ctrl+alt+\\"));
    assert!(matches_key("\x1b\x1d", "ctrl+alt+]"));
    assert_eq!(parse_key("\x1b\x1d").as_deref(), Some("ctrl+alt+]"));
    assert!(matches_key("\x1b\x1f", "ctrl+alt+_"));
    assert!(matches_key("\x1b\x1f", "ctrl+alt+-"));
    assert_eq!(parse_key("\x1b\x1f").as_deref(), Some("ctrl+alt+-"));
}

#[test]
#[serial]
fn treats_raw_0x08_as_plain_backspace_outside_windows_terminal() {
    let _kitty = KittyGuard::active(false);
    let _env = EnvGuard::set("WT_SESSION", None);
    assert!(matches_key("\x7f", "backspace"));
    assert!(!matches_key("\x7f", "ctrl+backspace"));
    assert_eq!(parse_key("\x7f").as_deref(), Some("backspace"));
    assert!(matches_key("\x08", "backspace"));
    assert!(!matches_key("\x08", "ctrl+backspace"));
    assert_eq!(parse_key("\x08").as_deref(), Some("backspace"));
    assert!(matches_key("\x08", "ctrl+h"));
}

#[test]
#[serial]
fn treats_raw_0x08_as_ctrl_backspace_in_local_windows_terminal() {
    let _kitty = KittyGuard::active(false);
    let _env = EnvGuard::set_many(&[
        ("WT_SESSION", Some("test-session")),
        ("SSH_CONNECTION", None),
        ("SSH_CLIENT", None),
        ("SSH_TTY", None),
    ]);
    assert!(matches_key("\x08", "ctrl+backspace"));
    assert!(!matches_key("\x08", "backspace"));
    assert_eq!(parse_key("\x08").as_deref(), Some("ctrl+backspace"));
    assert!(matches_key("\x08", "ctrl+h"));
}

#[test]
#[serial]
fn treats_raw_0x08_as_plain_backspace_in_windows_terminal_over_ssh() {
    let _kitty = KittyGuard::active(false);
    let _env = EnvGuard::set_many(&[
        ("WT_SESSION", Some("test-session")),
        ("SSH_CONNECTION", Some("1 2 3 4")),
        ("SSH_CLIENT", Some("1 2 3")),
        ("SSH_TTY", Some("/dev/pts/1")),
    ]);
    assert!(!matches_key("\x08", "ctrl+backspace"));
    assert!(matches_key("\x08", "backspace"));
    assert_eq!(parse_key("\x08").as_deref(), Some("backspace"));
    assert!(matches_key("\x08", "ctrl+h"));
}

#[test]
#[serial]
fn parses_legacy_alt_prefixed_sequences_when_kitty_inactive() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b ", "alt+space"));
    assert_eq!(parse_key("\x1b ").as_deref(), Some("alt+space"));
    assert!(matches_key("\x1b\x08", "alt+backspace"));
    assert_eq!(parse_key("\x1b\x08").as_deref(), Some("alt+backspace"));
    assert!(matches_key("\x1b\x03", "ctrl+alt+c"));
    assert_eq!(parse_key("\x1b\x03").as_deref(), Some("ctrl+alt+c"));
    assert!(matches_key("\x1bB", "alt+left"));
    assert_eq!(parse_key("\x1bB").as_deref(), Some("alt+left"));
    assert!(matches_key("\x1bF", "alt+right"));
    assert_eq!(parse_key("\x1bF").as_deref(), Some("alt+right"));
    assert!(matches_key("\x1ba", "alt+a"));
    assert_eq!(parse_key("\x1ba").as_deref(), Some("alt+a"));
    assert!(matches_key("\x1b1", "alt+1"));
    assert_eq!(parse_key("\x1b1").as_deref(), Some("alt+1"));
    assert!(matches_key("\x1b,", "alt+,"));
    assert_eq!(parse_key("\x1b,").as_deref(), Some("alt+,"));
    assert!(matches_key("\x1b.", "alt+."));
    assert_eq!(parse_key("\x1b.").as_deref(), Some("alt+."));
    assert!(matches_key("\x1by", "alt+y"));
    assert_eq!(parse_key("\x1by").as_deref(), Some("alt+y"));
    assert!(matches_key("\x1bz", "alt+z"));
    assert_eq!(parse_key("\x1bz").as_deref(), Some("alt+z"));

    set_kitty_protocol_active(true);
    assert!(!matches_key("\x1b ", "alt+space"));
    assert_eq!(parse_key("\x1b "), None);
    assert!(matches_key("\x1b\x08", "alt+backspace"));
    assert_eq!(parse_key("\x1b\x08").as_deref(), Some("alt+backspace"));
    assert!(!matches_key("\x1b\x03", "ctrl+alt+c"));
    assert_eq!(parse_key("\x1b\x03"), None);
    assert!(!matches_key("\x1bB", "alt+left"));
    assert_eq!(parse_key("\x1bB"), None);
    assert!(!matches_key("\x1bF", "alt+right"));
    assert_eq!(parse_key("\x1bF"), None);
    assert!(!matches_key("\x1ba", "alt+a"));
    assert_eq!(parse_key("\x1ba"), None);
    assert!(!matches_key("\x1b1", "alt+1"));
    assert_eq!(parse_key("\x1b1"), None);
    assert!(!matches_key("\x1b,", "alt+,"));
    assert_eq!(parse_key("\x1b,"), None);
    assert!(!matches_key("\x1b.", "alt+."));
    assert_eq!(parse_key("\x1b."), None);
    assert!(!matches_key("\x1by", "alt+y"));
    assert_eq!(parse_key("\x1by"), None);
}

#[test]
#[serial]
fn matches_arrow_keys() {
    assert!(matches_key("\x1b[A", "up"));
    assert!(matches_key("\x1b[B", "down"));
    assert!(matches_key("\x1b[C", "right"));
    assert!(matches_key("\x1b[D", "left"));
}

#[test]
#[serial]
fn matches_ss3_arrows_and_home_end() {
    assert!(matches_key("\x1bOA", "up"));
    assert!(matches_key("\x1bOB", "down"));
    assert!(matches_key("\x1bOC", "right"));
    assert!(matches_key("\x1bOD", "left"));
    assert!(matches_key("\x1bOH", "home"));
    assert!(matches_key("\x1bOF", "end"));
}

#[test]
#[serial]
fn matches_xterm_ctrl_modified_viewport_navigation() {
    assert!(matches_key("\x1b[1;5H", "ctrl+home"));
    assert!(matches_key("\x1b[1;5F", "ctrl+end"));
    assert!(matches_key("\x1b[5;5~", "ctrl+pageUp"));
    assert!(matches_key("\x1b[6;5~", "ctrl+pageDown"));
    assert_eq!(parse_key("\x1b[1;5H").as_deref(), Some("ctrl+home"));
    assert_eq!(parse_key("\x1b[1;5F").as_deref(), Some("ctrl+end"));
    assert_eq!(parse_key("\x1b[5;5~").as_deref(), Some("ctrl+pageUp"));
    assert_eq!(parse_key("\x1b[6;5~").as_deref(), Some("ctrl+pageDown"));
}

#[test]
#[serial]
fn matches_legacy_function_keys_and_clear() {
    assert!(matches_key("\x1bOP", "f1"));
    assert!(matches_key("\x1b[24~", "f12"));
    assert!(matches_key("\x1b[E", "clear"));
}

#[test]
#[serial]
fn matches_alt_arrows() {
    assert!(matches_key("\x1bp", "alt+up"));
    assert!(!matches_key("\x1bp", "up"));
}

#[test]
#[serial]
fn matches_rxvt_modifier_sequences() {
    assert!(matches_key("\x1b[a", "shift+up"));
    assert!(matches_key("\x1bOa", "ctrl+up"));
    assert!(matches_key("\x1b[2$", "shift+insert"));
    assert!(matches_key("\x1b[2^", "ctrl+insert"));
    assert!(matches_key("\x1b[7$", "shift+home"));
}

// ============================================================================
// decodeKittyPrintable & decodePrintableKey
// ============================================================================

#[test]
#[serial]
fn decodes_kitty_keypad_functional_keys_to_printable_characters() {
    assert_eq!(decode_kitty_printable("\x1b[57399u").as_deref(), Some("0"));
    assert_eq!(decode_kitty_printable("\x1b[57400u").as_deref(), Some("1"));
    assert_eq!(decode_kitty_printable("\x1b[57409u").as_deref(), Some("."));
    assert_eq!(decode_kitty_printable("\x1b[57410u").as_deref(), Some("/"));
    assert_eq!(decode_kitty_printable("\x1b[57411u").as_deref(), Some("*"));
    assert_eq!(decode_kitty_printable("\x1b[57412u").as_deref(), Some("-"));
    assert_eq!(decode_kitty_printable("\x1b[57413u").as_deref(), Some("+"));
    assert_eq!(decode_kitty_printable("\x1b[57415u").as_deref(), Some("="));
    assert_eq!(decode_kitty_printable("\x1b[57416u").as_deref(), Some(","));
    assert_eq!(decode_kitty_printable("\x1b[57417u"), None);
}

#[test]
#[serial]
fn decodes_printable_xterm_modify_other_keys_sequences() {
    assert_eq!(decode_printable_key("\x1b[27;2;69~").as_deref(), Some("E"));
    assert_eq!(decode_printable_key("\x1b[27;2;196~").as_deref(), Some("Ä"));
    assert_eq!(decode_printable_key("\x1b[27;2;32~").as_deref(), Some(" "));
    assert_eq!(decode_printable_key("\x1b[27;2;13~"), None);
    assert_eq!(decode_printable_key("\x1b[27;6;69~"), None);
}

// ============================================================================
// parseKey
// ============================================================================

#[test]
#[serial]
fn returns_latin_key_name_when_base_layout_key_is_present() {
    let _kitty = KittyGuard::active(true);
    let cyrillic_ctrl_c = "\x1b[1089::99;5u";
    assert_eq!(parse_key(cyrillic_ctrl_c).as_deref(), Some("ctrl+c"));
}

#[test]
#[serial]
fn parse_key_prefers_codepoint_for_latin_letters_when_base_layout_differs() {
    let _kitty = KittyGuard::active(true);
    let dvorak_ctrl_k = "\x1b[107::118;5u";
    assert_eq!(parse_key(dvorak_ctrl_k).as_deref(), Some("ctrl+k"));
}

#[test]
#[serial]
fn parse_key_prefers_codepoint_for_symbol_keys_when_base_layout_differs() {
    let _kitty = KittyGuard::active(true);
    let dvorak_ctrl_slash = "\x1b[47::91;5u";
    assert_eq!(parse_key(dvorak_ctrl_slash).as_deref(), Some("ctrl+/"));
}

#[test]
#[serial]
fn returns_key_name_from_codepoint_when_no_base_layout() {
    let _kitty = KittyGuard::active(true);
    let latin_ctrl_c = "\x1b[99;5u";
    assert_eq!(parse_key(latin_ctrl_c).as_deref(), Some("ctrl+c"));
}

#[test]
#[serial]
fn parses_shifted_uppercase_csi_u_letters_as_shift_letter() {
    let _kitty = KittyGuard::active(true);
    assert!(matches_key("\x1b[69;2u", "shift+e"));
    assert_eq!(parse_key("\x1b[69;2u").as_deref(), Some("shift+e"));
}

#[test]
#[serial]
fn ignores_kitty_csi_u_with_unsupported_modifiers() {
    let _kitty = KittyGuard::active(true);
    assert_eq!(parse_key("\x1b[99;17u"), None);
}

#[test]
#[serial]
fn parses_legacy_ctrl_letter() {
    let _kitty = KittyGuard::active(false);
    assert_eq!(parse_key("\x03").as_deref(), Some("ctrl+c"));
    assert_eq!(parse_key("\x04").as_deref(), Some("ctrl+d"));
}

#[test]
#[serial]
fn parses_special_keys() {
    let _kitty = KittyGuard::active(false);
    assert_eq!(parse_key("\x1b").as_deref(), Some("escape"));
    assert_eq!(parse_key("\t").as_deref(), Some("tab"));
    assert_eq!(parse_key("\r").as_deref(), Some("enter"));
    assert_eq!(parse_key("\n").as_deref(), Some("enter"));
    assert_eq!(parse_key("\x00").as_deref(), Some("ctrl+space"));
    assert_eq!(parse_key(" ").as_deref(), Some("space"));
    assert_eq!(parse_key("1").as_deref(), Some("1"));
    assert!(matches_key("1", "1"));
    assert_eq!(parse_key("+").as_deref(), Some("+"));
    assert!(matches_key("+", "+"));
}

#[test]
#[serial]
fn parses_arrow_keys() {
    assert_eq!(parse_key("\x1b[A").as_deref(), Some("up"));
    assert_eq!(parse_key("\x1b[B").as_deref(), Some("down"));
    assert_eq!(parse_key("\x1b[C").as_deref(), Some("right"));
    assert_eq!(parse_key("\x1b[D").as_deref(), Some("left"));
}

#[test]
#[serial]
fn parses_ss3_arrows_and_home_end() {
    assert_eq!(parse_key("\x1bOA").as_deref(), Some("up"));
    assert_eq!(parse_key("\x1bOB").as_deref(), Some("down"));
    assert_eq!(parse_key("\x1bOC").as_deref(), Some("right"));
    assert_eq!(parse_key("\x1bOD").as_deref(), Some("left"));
    assert_eq!(parse_key("\x1bOH").as_deref(), Some("home"));
    assert_eq!(parse_key("\x1bOF").as_deref(), Some("end"));
}

#[test]
#[serial]
fn parses_legacy_function_and_modifier_sequences() {
    assert_eq!(parse_key("\x1bOP").as_deref(), Some("f1"));
    assert_eq!(parse_key("\x1b[24~").as_deref(), Some("f12"));
    assert_eq!(parse_key("\x1b[E").as_deref(), Some("clear"));
    assert_eq!(parse_key("\x1b[2^").as_deref(), Some("ctrl+insert"));
    assert_eq!(parse_key("\x1bp").as_deref(), Some("alt+up"));
}

#[test]
#[serial]
fn parses_double_bracket_page_up() {
    assert_eq!(parse_key("\x1b[[5~").as_deref(), Some("pageUp"));
}

#[test]
#[serial]
fn matches_shifted_symbols_by_their_produced_character() {
    let _kitty = KittyGuard::active(true);
    assert!(matches_key("\x1b[61:43;2u", "+"));
    assert!(matches_key("\x1b[61:43;2u", "shift+="));
    assert_eq!(parse_key("\x1b[61:43;2u").as_deref(), Some("+"));
    assert!(matches_key("\x1b[61:43;6u", "ctrl++"));
    assert_eq!(parse_key("\x1b[61:43;6u").as_deref(), Some("ctrl++"));
}

#[test]
#[serial]
fn matches_xterm_modify_other_keys_shifted_plus() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[27;6;43~", "ctrl++"));
    assert_eq!(parse_key("\x1b[27;6;43~").as_deref(), Some("ctrl++"));
}

#[test]
#[serial]
fn matches_kitty_function_keys_f1_to_f12_with_and_without_modifiers() {
    let _kitty = KittyGuard::active(false);
    assert!(matches_key("\x1b[P", "f1"));
    assert!(matches_key("\x1b[Q", "f2"));
    assert!(matches_key("\x1b[13~", "f3"));
    assert!(matches_key("\x1b[S", "f4"));
    assert!(matches_key("\x1b[1;5P", "ctrl+f1"));
    assert!(!matches_key("\x1b[1;5P", "f1"));
    assert!(matches_key("\x1b[1;2S", "shift+f4"));
    assert!(matches_key("\x1b[15;5~", "ctrl+f5"));
    assert!(matches_key("\x1b[24;3~", "alt+f12"));
    assert!(matches_key("\x1b[15;1:2~", "f5"));
    assert_eq!(parse_key("\x1b[P").as_deref(), Some("f1"));
    assert_eq!(parse_key("\x1b[1;5P").as_deref(), Some("ctrl+f1"));
    assert_eq!(parse_key("\x1b[13~").as_deref(), Some("f3"));
    assert_eq!(parse_key("\x1b[21;2~").as_deref(), Some("shift+f10"));
    assert_eq!(parse_key("\x1b[1;5R"), None);
    assert!(is_key_release("\x1b[1;1:3P"));
    assert!(is_key_repeat("\x1b[1;1:2S"));
}

#[test]
#[serial]
fn ignores_unmapped_kitty_functional_keys_such_as_f13_to_f35() {
    assert_eq!(decode_kitty_printable("\x1b[57376u"), None);
    assert_eq!(decode_kitty_printable("\x1b[57398;2u"), None);
    assert_eq!(decode_printable_key("\x1b[57382u"), None);
}
