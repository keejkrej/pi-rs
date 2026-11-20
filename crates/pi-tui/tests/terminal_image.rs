//! Port of packages/tui/test/terminal-image.test.ts (v1.0.0)

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pi_js::env::testing::EnvGuard;
use pi_tui::terminal_image::{
    KittyImageMetadata, NumberOrString, TerminalCapabilityOverrides, crop_kitty_image_line, delete_all_kitty_images,
    delete_all_kitty_placements, delete_kitty_image, detect_capabilities, encode_iterm2, encode_kitty,
    get_capabilities, get_kitty_image_metadata, get_kitty_image_placement, get_kitty_image_placement_rows, hyperlink,
    image_fallback, is_image_line, register_kitty_image_metadata, render_image, reset_capabilities_cache,
    set_capabilities, set_capability_overrides, set_cell_dimensions,
};
use pi_tui::{
    CellDimensions, Image, ImageDimensions, ImageOptions, ImageProtocol, ImageRenderOptions, ImageTheme,
    TerminalCapabilities, set_image_transcoder, utils::visible_width,
};
use regex::Regex;
use serial_test::serial;

const ENV_KEYS: &[&str] = &[
    "TERM",
    "TERM_PROGRAM",
    "ORCA_IMAGE_PROTOCOL",
    "TERMINAL_EMULATOR",
    "COLORTERM",
    "TMUX",
    "KITTY_WINDOW_ID",
    "GHOSTTY_RESOURCES_DIR",
    "WEZTERM_PANE",
    "ITERM_SESSION_ID",
    "WT_SESSION",
    "ALACRITTY_WINDOW_ID",
    "CMUX_WORKSPACE_ID",
    "WARP_SESSION_ID",
    "WARP_TERMINAL_SESSION_UUID",
    "PI_HYPERLINKS",
    "PI_IMAGE_PROTOCOL",
    "PI_TRUE_COLOR",
];

fn with_env<T>(overrides: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
    let mut guard = EnvGuard::empty();
    for key in ENV_KEYS {
        guard.stub(key, None);
    }
    for (k, v) in overrides {
        guard.stub(k, *v);
    }
    f()
}

fn reset_test_state() {
    reset_capabilities_cache();
    set_cell_dimensions(CellDimensions {
        width_px: 9,
        height_px: 18,
    });
}

#[test]
fn is_image_line_iterm2_at_start() {
    let line = "\x1b]1337;File=size=100,100;inline=1:base64encodeddata==\x07";
    assert!(is_image_line(line));
}

#[test]
fn is_image_line_iterm2_with_text_before() {
    let line = "Some text \x1b]1337;File=size=100,100;inline=1:base64data==\x07 more text";
    assert!(is_image_line(line));
}

#[test]
fn is_image_line_iterm2_in_middle_of_long_line() {
    let line = "Text before image...\x1b]1337;File=inline=1:verylongbase64data==...text after";
    assert!(is_image_line(line));
}

#[test]
fn is_image_line_iterm2_at_end() {
    let line = "Regular text ending with \x1b]1337;File=inline=1:base64data==\x07";
    assert!(is_image_line(line));
}

#[test]
fn is_image_line_iterm2_minimal() {
    let line = "\x1b]1337;File=:\x07";
    assert!(is_image_line(line));
}

#[test]
fn is_image_line_kitty_at_start() {
    let line = "\x1b_Ga=T,f=100,t=f,d=base64data...\x1b\\\x1b_Gm=i=1;\x1b\\";
    assert!(is_image_line(line));
}

#[test]
fn is_image_line_kitty_with_text_before() {
    let line = "Output: \x1b_Ga=T,f=100;data...\x1b\\\x1b_Gm=i=1;\x1b\\";
    assert!(is_image_line(line));
}

#[test]
fn is_image_line_kitty_with_padding() {
    let line = "  \x1b_Ga=T,f=100...\x1b\\\x1b_Gm=i=1;\x1b\\  ";
    assert!(is_image_line(line));
}

#[test]
fn is_image_line_bug_regression_long_line_and_no_image_terminal() {
    let base64_char = "A".repeat(100);
    let image_sequence = "\x1b]1337;File=size=800,600;inline=1:";
    let long_line = format!("Text prefix {image_sequence}{} suffix", base64_char.repeat(3000));
    assert!(long_line.len() > 300_000);
    assert!(is_image_line(&long_line));

    let line_with_image = "Read image file [image/jpeg]\x1b]1337;File=inline=1:base64data==\x07";
    assert!(is_image_line(line_with_image));

    let line_with_ansi_and_image = "\x1b[31mError output \x1b]1337;File=inline=1:image==\x07";
    assert!(is_image_line(line_with_ansi_and_image));

    let line_with_image_and_ansi = "\x1b_Ga=T,f=100:data...\x1b\\\x1b_Gm=i=1;\x1b\\\x1b[0m reset";
    assert!(is_image_line(line_with_image_and_ansi));
}

#[test]
fn is_image_line_negative_and_mixed_cases() {
    assert!(!is_image_line(
        "This is just a regular text line without any escape sequences"
    ));
    assert!(!is_image_line("\x1b[31mRed text\x1b[0m and \x1b[32mgreen text\x1b[0m"));
    assert!(!is_image_line("\x1b[1A\x1b[2KLine cleared and moved up"));
    assert!(!is_image_line("Some text with ]1337;File but missing ESC at start"));
    assert!(!is_image_line("Some text with _G but missing ESC at start"));
    assert!(!is_image_line(""));
    assert!(!is_image_line("\n"));
    assert!(!is_image_line("\n\n"));

    let mixed_line = "Kitty: \x1b_Ga=T...\x1b\\\x1b_Gm=i=1;\x1b\\ iTerm2: \x1b]1337;File=inline=1:data==\x07";
    assert!(is_image_line(mixed_line));

    let complex_line = "Start \x1b]1337;File=img1==\x07 middle \x1b]1337;File=img2==\x07 end";
    assert!(is_image_line(complex_line));

    let file_path_line = "/path/to/File_1337_backup/image.jpg";
    assert!(!is_image_line(file_path_line));
}

#[test]
#[serial]
fn detect_capabilities_defaults_and_overrides() {
    with_env(&[], || {
        let caps = detect_capabilities(None);
        assert!(!caps.hyperlinks);
        assert_eq!(caps.images, None);
    });

    assert_eq!(
        with_env(
            &[
                ("PI_HYPERLINKS", Some("1")),
                ("PI_IMAGE_PROTOCOL", Some("kitty")),
                ("PI_TRUE_COLOR", Some("1")),
            ],
            || detect_capabilities(None)
        ),
        TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        }
    );

    assert_eq!(
        with_env(
            &[
                ("TERM_PROGRAM", Some("iterm.app")),
                ("PI_HYPERLINKS", Some("0")),
                ("PI_IMAGE_PROTOCOL", Some("none")),
                ("PI_TRUE_COLOR", Some("0")),
            ],
            || detect_capabilities(None)
        ),
        TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: false,
        }
    );

    assert_eq!(
        with_env(
            &[
                ("TERM_PROGRAM", Some("ghostty")),
                ("PI_HYPERLINKS", Some("auto")),
                ("PI_IMAGE_PROTOCOL", Some("auto")),
                ("PI_TRUE_COLOR", Some("auto")),
            ],
            || detect_capabilities(None)
        ),
        TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        }
    );
}

#[test]
#[serial]
fn detect_capabilities_programmatic_overrides() {
    with_env(
        &[
            ("PI_HYPERLINKS", Some("1")),
            ("PI_IMAGE_PROTOCOL", Some("kitty")),
            ("PI_TRUE_COLOR", Some("1")),
        ],
        || {
            set_capability_overrides(TerminalCapabilityOverrides {
                images: Some(None),
                true_color: Some(false),
                hyperlinks: Some(false),
            });
            assert_eq!(
                get_capabilities(),
                TerminalCapabilities {
                    images: None,
                    true_color: false,
                    hyperlinks: false,
                }
            );
            set_capability_overrides(TerminalCapabilityOverrides::default());
            assert_eq!(
                get_capabilities(),
                TerminalCapabilities {
                    images: Some(ImageProtocol::Kitty),
                    true_color: true,
                    hyperlinks: true,
                }
            );
            set_capability_overrides(TerminalCapabilityOverrides::default());
            reset_capabilities_cache();
        },
    );
}

#[test]
#[serial]
fn detect_capabilities_tmux_and_terminals() {
    let probed = AtomicBool::new(false);
    let caps = with_env(
        &[
            ("TMUX", Some("/tmp/tmux-1000/default,1234,0")),
            ("PI_HYPERLINKS", Some("1")),
            ("PI_IMAGE_PROTOCOL", Some("kitty")),
        ],
        || {
            detect_capabilities(Some(&|| {
                probed.store(true, Ordering::SeqCst);
                false
            }))
        },
    );
    assert!(!probed.load(Ordering::SeqCst));
    assert!(caps.hyperlinks);
    assert_eq!(caps.images, Some(ImageProtocol::Kitty));

    with_env(
        &[
            ("TMUX", Some("/tmp/tmux-1000/default,1234,0")),
            ("TERM_PROGRAM", Some("ghostty")),
        ],
        || {
            let caps = detect_capabilities(Some(&|| true));
            assert!(caps.hyperlinks);
            assert_eq!(caps.images, None);

            let caps2 = detect_capabilities(Some(&|| false));
            assert!(!caps2.hyperlinks);
            assert_eq!(caps2.images, None);
        },
    );

    with_env(
        &[("TERM", Some("tmux-256color")), ("TERM_PROGRAM", Some("iterm.app"))],
        || {
            let caps = detect_capabilities(Some(&|| true));
            assert!(caps.hyperlinks);
            assert_eq!(caps.images, None);
            let caps2 = detect_capabilities(Some(&|| false));
            assert!(!caps2.hyperlinks);
        },
    );

    with_env(&[("TERM", Some("screen-256color"))], || {
        let caps = detect_capabilities(None);
        assert!(!caps.hyperlinks);
        assert_eq!(caps.images, None);
    });

    with_env(&[("TERM_PROGRAM", Some("ghostty"))], || {
        assert!(detect_capabilities(None).hyperlinks);
    });
    with_env(
        &[
            ("TERM_PROGRAM", Some("ghostty")),
            ("CMUX_WORKSPACE_ID", Some("workspace")),
        ],
        || {
            let caps = detect_capabilities(None);
            assert_eq!(caps.images, Some(ImageProtocol::Kitty));
            assert!(caps.hyperlinks);
        },
    );
    with_env(&[("KITTY_WINDOW_ID", Some("1"))], || {
        assert!(detect_capabilities(None).hyperlinks);
    });
    with_env(&[("WEZTERM_PANE", Some("0"))], || {
        assert!(detect_capabilities(None).hyperlinks);
    });
    with_env(&[("TERM_PROGRAM", Some("WarpTerminal"))], || {
        let caps = detect_capabilities(None);
        assert_eq!(caps.images, Some(ImageProtocol::Kitty));
        assert!(caps.true_color);
        assert!(caps.hyperlinks);
    });
    with_env(&[("WARP_SESSION_ID", Some("some-session-id"))], || {
        let caps = detect_capabilities(None);
        assert_eq!(caps.images, Some(ImageProtocol::Kitty));
        assert!(caps.true_color);
        assert!(caps.hyperlinks);
    });
    with_env(
        &[(
            "WARP_TERMINAL_SESSION_UUID",
            Some("d0e1a2e5-7ca7-44cd-9037-ac7222011161"),
        )],
        || {
            let caps = detect_capabilities(None);
            assert_eq!(caps.images, Some(ImageProtocol::Kitty));
            assert!(caps.true_color);
            assert!(caps.hyperlinks);
        },
    );
    with_env(
        &[
            ("TERM_PROGRAM", Some("WarpTerminal")),
            ("TMUX", Some("/tmp/tmux-1000/default,1234,0")),
            ("TERM", Some("tmux-256color")),
        ],
        || {
            let caps = detect_capabilities(Some(&|| true));
            assert_eq!(caps.images, None);
            assert!(caps.hyperlinks);
        },
    );
    with_env(&[("TERM_PROGRAM", Some("iterm.app"))], || {
        assert!(detect_capabilities(None).hyperlinks);
    });
    with_env(&[("TERM_PROGRAM", Some("vscode"))], || {
        assert!(detect_capabilities(None).hyperlinks);
    });
    with_env(&[("TERM_PROGRAM", Some("zed"))], || {
        assert_eq!(
            detect_capabilities(None),
            TerminalCapabilities {
                images: None,
                true_color: true,
                hyperlinks: true,
            }
        );
    });
    with_env(
        &[("WT_SESSION", Some("session")), ("TERM", Some("xterm-256color"))],
        || {
            let caps = detect_capabilities(None);
            assert!(caps.true_color);
            assert!(caps.hyperlinks);
            assert_eq!(caps.images, None);
        },
    );
    with_env(
        &[
            ("TERMINAL_EMULATOR", Some("JetBrains-JediTerm")),
            ("TERM", Some("xterm-256color")),
        ],
        || {
            let caps = detect_capabilities(None);
            assert!(caps.true_color);
            assert!(!caps.hyperlinks);
            assert_eq!(caps.images, None);
        },
    );
    with_env(
        &[
            ("WT_SESSION", Some("session")),
            ("TMUX", Some("/tmp/tmux-1000/default,1234,0")),
            ("TERM", Some("tmux-256color")),
        ],
        || {
            let caps = detect_capabilities(Some(&|| false));
            assert!(!caps.true_color);
            assert!(!caps.hyperlinks);
            assert_eq!(caps.images, None);
        },
    );
    with_env(
        &[
            ("COLORTERM", Some("truecolor")),
            ("TMUX", Some("/tmp/tmux-1000/default,1234,0")),
            ("TERM", Some("tmux-256color")),
        ],
        || {
            let caps = detect_capabilities(Some(&|| false));
            assert!(caps.true_color);
            assert!(!caps.hyperlinks);
            assert_eq!(caps.images, None);
        },
    );
    with_env(&[("TERM", Some("xterm-direct"))], || {
        assert!(detect_capabilities(Some(&|| false)).true_color);
    });
}

#[test]
fn iterm2_image_encoding_includes_decoded_payload_size() {
    let sequence = encode_iterm2(
        "AAAA",
        Some(pi_tui::terminal_image::EncodeITerm2Options {
            width: Some(NumberOrString::Number(2.0)),
            height: Some(NumberOrString::Text("auto".to_string())),
            ..Default::default()
        }),
    );
    assert_eq!(sequence, "\x1b]1337;File=inline=1;size=3;width=2;height=auto:AAAA\x07");
}

#[test]
#[serial]
fn kitty_image_cursor_movement_and_metadata() {
    let sequence = encode_kitty(
        "AAAA",
        Some(pi_tui::terminal_image::EncodeKittyOptions {
            columns: Some(2),
            rows: Some(2),
            move_cursor: Some(false),
            ..Default::default()
        }),
    );
    assert!(sequence.starts_with("\x1b_Ga=T,f=100,q=2,C=1,c=2,r=2;"));

    assert_eq!(delete_kitty_image(42), "\x1b_Ga=d,d=I,i=42,q=2\x1b\\");
    assert_eq!(delete_all_kitty_images(), "\x1b_Ga=d,d=A,q=2\x1b\\");
    assert_eq!(delete_all_kitty_placements(), "\x1b_Ga=d,d=a,q=2\x1b\\");

    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    });
    set_cell_dimensions(CellDimensions {
        width_px: 10,
        height_px: 10,
    });
    let result = render_image(
        "AAAA",
        ImageDimensions {
            width_px: 20,
            height_px: 20,
        },
        Some(ImageRenderOptions {
            max_width_cells: Some(2),
            ..Default::default()
        }),
    )
    .unwrap();
    assert!(!result.sequence.contains(",C=1,"));
    assert_eq!(result.rows, 2);

    let result2 = render_image(
        "AAAA",
        ImageDimensions {
            width_px: 20,
            height_px: 20,
        },
        Some(ImageRenderOptions {
            max_width_cells: Some(2),
            move_cursor: Some(false),
            ..Default::default()
        }),
    )
    .unwrap();
    assert!(result2.sequence.contains(",C=1,"));
    assert_eq!(result2.rows, 2);

    let result3 = render_image(
        "AAAA",
        ImageDimensions {
            width_px: 100,
            height_px: 100,
        },
        Some(ImageRenderOptions {
            max_width_cells: Some(3),
            image_id: Some(42),
            move_cursor: Some(false),
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(
        get_kitty_image_metadata(&result3.sequence),
        Some(KittyImageMetadata {
            image_id: 42,
            columns: 3,
            rows: 3,
            width_px: 100,
            height_px: 100,
        })
    );
    assert!(crop_kitty_image_line(&result3.sequence, 2, 1).contains("y=66,h=34,r=1"));

    register_kitty_image_metadata(KittyImageMetadata {
        image_id: 42,
        columns: 3,
        rows: 3,
        width_px: 100,
        height_px: 100,
    });
    let transmission = encode_kitty(
        &"A".repeat(8192),
        Some(pi_tui::terminal_image::EncodeKittyOptions {
            columns: Some(3),
            rows: Some(3),
            image_id: Some(42),
            move_cursor: Some(false),
        }),
    );
    let line = format!("left {} right", crop_kitty_image_line(&transmission, 2, 1));
    let placement = get_kitty_image_placement(&line).unwrap();
    assert_eq!(get_kitty_image_placement_rows(&line), Some(1));
    assert_eq!(
        placement.transmission_bytes as usize,
        line.len() - "left ".len() - " right".len()
    );
    assert_eq!(placement.estimated_decoded_bytes, 100 * 100 * 4);
    assert_eq!(placement.rows, 1);
    assert_eq!(placement.sequence, "\x1b_Ga=p,q=2,C=1,c=3,i=42,y=66,h=34,r=1\x1b\\");
    assert_eq!(placement.replacement_line, format!("left {} right", placement.sequence));
    assert!(!placement.replacement_line.contains("AAAA"));

    let result4 = render_image(
        "AAAA",
        ImageDimensions {
            width_px: 10,
            height_px: 100,
        },
        Some(ImageRenderOptions {
            max_width_cells: Some(10),
            max_height_cells: Some(5),
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(result4.rows, 5);
    assert!(result4.sequence.contains(",c=1,r=5"));

    reset_test_state();
}

#[test]
#[serial]
fn image_cell_sizing_kitty_and_iterm2_render_image() {
    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    });
    set_cell_dimensions(CellDimensions {
        width_px: 9,
        height_px: 18,
    });
    let r1 = render_image(
        "AAAA",
        ImageDimensions {
            width_px: 1200,
            height_px: 12,
        },
        Some(ImageRenderOptions {
            max_width_cells: Some(60),
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(r1.rows, 1);
    assert!(r1.sequence.contains(",c=60,r=1;"));

    set_cell_dimensions(CellDimensions {
        width_px: 15,
        height_px: 28,
    });
    let r2 = render_image(
        "AAAA",
        ImageDimensions {
            width_px: 615,
            height_px: 86,
        },
        Some(ImageRenderOptions {
            max_width_cells: Some(60),
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(r2.rows, 5);
    assert!(r2.sequence.contains(",c=60,r=5;"));

    set_cell_dimensions(CellDimensions {
        width_px: 1,
        height_px: 1,
    });
    for (width_px, columns) in [(1, 1), (140, 1), (149, 2)] {
        let r = render_image(
            "AAAA",
            ImageDimensions {
                width_px,
                height_px: 1000,
            },
            Some(ImageRenderOptions {
                max_width_cells: Some(30),
                max_height_cells: Some(10),
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(r.columns, columns);
        assert_eq!(r.rows, 10);
        assert!(r.sequence.contains(&format!(",c={columns},r=10;")));
    }

    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Iterm2),
        true_color: true,
        hyperlinks: true,
    });
    set_cell_dimensions(CellDimensions {
        width_px: 14,
        height_px: 28,
    });
    let r_iterm = render_image(
        "AAAA",
        ImageDimensions {
            width_px: 400,
            height_px: 900,
        },
        Some(ImageRenderOptions {
            max_width_cells: Some(30),
            max_height_cells: Some(15),
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(r_iterm.columns, 14);
    assert_eq!(r_iterm.rows, 15);
    assert_eq!(
        r_iterm.sequence,
        "\x1b]1337;File=inline=1;size=3;width=14;height=auto:AAAA\x07"
    );

    reset_test_state();
}

#[test]
#[serial]
fn image_component_caps_height_and_renders() {
    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    });
    set_cell_dimensions(CellDimensions {
        width_px: 10,
        height_px: 20,
    });
    let image = Image::new(
        "AAAA",
        "image/png",
        ImageTheme {
            fallback_color: Arc::new(|value| value.to_string()),
        },
        Some(ImageOptions {
            max_width_cells: Some(10),
            ..Default::default()
        }),
        Some(ImageDimensions {
            width_px: 10,
            height_px: 100,
        }),
    );
    let lines = image.render(12);
    assert_eq!(lines.len(), 5);
    assert!(lines[0].contains(",c=1,r=5"));

    set_cell_dimensions(CellDimensions {
        width_px: 10,
        height_px: 10,
    });
    let image2 = Image::new(
        "AAAA",
        "image/png",
        ImageTheme {
            fallback_color: Arc::new(|value| value.to_string()),
        },
        Some(ImageOptions {
            max_width_cells: Some(2),
            ..Default::default()
        }),
        Some(ImageDimensions {
            width_px: 20,
            height_px: 20,
        }),
    );
    let lines2 = image2.render(4);
    let image_id = image2.get_image_id().unwrap();
    assert!(lines2[0].starts_with("\x1b_G"));
    assert!(lines2[0].contains(",C=1,"));
    assert!(lines2[0].contains(&format!(",i={image_id}")));
    assert!(lines2[0].ends_with("\x1b\\"));
    assert_eq!(&lines2[1..], &[""]);

    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: false,
        hyperlinks: false,
    });
    let long_path = pi_js::path::join(&[
        &pi_js::env::home_dir(),
        "images",
        &format!("{}.png", "generated-image-with-a-very-long-absolute-path".repeat(4)),
    ]);
    let width = 40usize;
    let image3 = Image::new(
        "AAAA",
        "image/png",
        ImageTheme {
            fallback_color: Arc::new(|value| format!("\x1b[33m{value}\x1b[0m")),
        },
        Some(ImageOptions {
            filename: Some(long_path),
            ..Default::default()
        }),
        Some(ImageDimensions {
            width_px: 1280,
            height_px: 720,
        }),
    );
    let lines3 = image3.render(width);
    assert_eq!(lines3.len(), 1);
    assert!(visible_width(&lines3[0]) <= width as i64);
    assert!(lines3[0].contains("..."));
    assert!(lines3[0].contains('~'));

    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    });
    set_cell_dimensions(CellDimensions {
        width_px: 9,
        height_px: 18,
    });
    let image4 = Image::new(
        "AAAA",
        "image/png",
        ImageTheme {
            fallback_color: Arc::new(|value| value.to_string()),
        },
        Some(ImageOptions {
            max_width_cells: Some(60),
            image_id: Some(8938),
            ..Default::default()
        }),
        Some(ImageDimensions {
            width_px: 615,
            height_px: 86,
        }),
    );
    let lines4 = image4.render(62);
    assert_eq!(lines4.len(), 4);
    assert_eq!(&lines4[1..], &["", "", ""]);
    assert!(lines4[0].contains(",c=60,r=4,i=8938;"));
    assert_eq!(
        get_kitty_image_metadata(&lines4[0]),
        Some(KittyImageMetadata {
            image_id: 8938,
            columns: 60,
            rows: 4,
            width_px: 615,
            height_px: 86,
        })
    );
    let cropped = crop_kitty_image_line(&lines4[0], 1, 2);
    assert_eq!(
        get_kitty_image_placement(&cropped).unwrap().sequence,
        "\x1b_Ga=p,q=2,C=1,c=60,i=8938,y=21,h=44,r=2\x1b\\"
    );
    let narrower = image4.render(32);
    assert_eq!(narrower.len(), 2);
    assert!(narrower[0].contains(",c=30,r=2,i=8938;"));
    assert_eq!(get_kitty_image_metadata(&narrower[0]).unwrap().rows, 2);

    set_cell_dimensions(CellDimensions {
        width_px: 14,
        height_px: 28,
    });
    let image5 = Image::new(
        "AAAA",
        "image/png",
        ImageTheme {
            fallback_color: Arc::new(|value| value.to_string()),
        },
        Some(ImageOptions {
            max_width_cells: Some(30),
            image_id: Some(8938),
            ..Default::default()
        }),
        Some(ImageDimensions {
            width_px: 400,
            height_px: 900,
        }),
    );
    let lines5 = image5.render(32);
    assert_eq!(lines5.len(), 15);
    assert!(lines5[0].contains(",c=13,r=15,i=8938;"));
    assert_eq!(
        get_kitty_image_placement(&crop_kitty_image_line(&lines5[0], 1, 2))
            .unwrap()
            .sequence,
        "\x1b_Ga=p,q=2,C=1,c=13,i=8938,y=60,h=120,r=2\x1b\\"
    );
    let narrower5 = image5.render(22);
    assert_eq!(narrower5.len(), 10);
    assert!(narrower5[0].contains(",c=9,r=10,i=8938;"));

    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Iterm2),
        true_color: true,
        hyperlinks: true,
    });
    set_cell_dimensions(CellDimensions {
        width_px: 9,
        height_px: 18,
    });
    let image6 = Image::new(
        "AAAA",
        "image/png",
        ImageTheme {
            fallback_color: Arc::new(|value| value.to_string()),
        },
        Some(ImageOptions {
            max_width_cells: Some(60),
            ..Default::default()
        }),
        Some(ImageDimensions {
            width_px: 615,
            height_px: 86,
        }),
    );
    assert_eq!(
        image6.render(62),
        vec![
            "",
            "",
            "",
            "",
            "\x1b[4A\x1b]1337;File=inline=1;size=3;width=60;height=auto:AAAA\x07",
        ]
    );

    reset_test_state();
}

#[test]
#[serial]
fn image_fallback_tests() {
    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: false,
        hyperlinks: false,
    });
    let abs = pi_js::path::join(&[&pi_js::env::home_dir(), ".pi", "agent", "shot.png"]);
    let result = image_fallback(
        "image/png",
        Some(ImageDimensions {
            width_px: 1280,
            height_px: 720,
        }),
        Some(&abs),
    );
    assert_eq!(result, "[Image: ~/.pi/agent/shot.png [image/png] 1280x720]");

    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: false,
        hyperlinks: true,
    });
    let result_linked = image_fallback(
        "image/png",
        Some(ImageDimensions {
            width_px: 10,
            height_px: 10,
        }),
        Some(&abs),
    );
    assert!(result_linked.contains("\x1b]8;;file://"));
    assert!(result_linked.contains(&abs.replace('\\', "/")) || result_linked.contains(&abs));
    let strip_osc8 = Regex::new(r"\x1b\]8;;.*?\x1b\\").unwrap();
    let visible = strip_osc8.replace_all(&result_linked, "");
    assert_eq!(visible, "[Image: ~/.pi/agent/shot.png [image/png] 10x10]");

    let bare = image_fallback(
        "image/png",
        Some(ImageDimensions {
            width_px: 1,
            height_px: 1,
        }),
        Some("clankolas.png"),
    );
    assert_eq!(bare, "[Image: clankolas.png [image/png] 1x1]");
    assert!(!bare.contains("\x1b]8;"));

    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: false,
        hyperlinks: false,
    });
    assert_eq!(
        image_fallback(
            "image/png",
            Some(ImageDimensions {
                width_px: 8,
                height_px: 6,
            }),
            None,
        ),
        "[Image: [image/png] 8x6]"
    );

    reset_test_state();
}

#[test]
fn hyperlink_tests() {
    assert_eq!(
        hyperlink("click me", "https://example.com"),
        "\x1b]8;;https://example.com\x1b\\click me\x1b]8;;\x1b\\"
    );

    let styled = "\x1b[4m\x1b[34mclick me\x1b[0m";
    let result = hyperlink(styled, "https://example.com");
    assert!(result.starts_with("\x1b]8;;https://example.com\x1b\\"));
    assert!(result.contains(styled));
    assert!(result.ends_with("\x1b]8;;\x1b\\"));

    assert_eq!(
        hyperlink("", "https://example.com"),
        "\x1b]8;;https://example.com\x1b\\\x1b]8;;\x1b\\"
    );

    let file_res = hyperlink("README.md", "file:///home/user/README.md");
    assert!(file_res.contains("file:///home/user/README.md"));
    assert!(file_res.contains("README.md"));
}

#[test]
#[serial]
fn detect_capabilities_enables_hyperlinks_and_kitty_images_for_herdr() {
    // #10573
    with_env(
        &[
            ("TERM_PROGRAM", Some("herdr")),
            ("TERM", Some("xterm-256color")),
            ("COLORTERM", Some("truecolor")),
            ("KITTY_WINDOW_ID", Some("1")),
        ],
        || {
            let caps = detect_capabilities(None);
            assert!(caps.hyperlinks);
            assert_eq!(caps.images, Some(ImageProtocol::Kitty));
            assert!(caps.true_color);
        },
    );
    with_env(&[("TERM_PROGRAM", Some("herdr")), ("PI_HYPERLINKS", Some("0"))], || {
        assert!(!detect_capabilities(None).hyperlinks);
    });
}

#[test]
#[serial]
fn detect_capabilities_enables_hyperlinks_without_images_for_rex() {
    // #11131
    with_env(
        &[
            ("TERM_PROGRAM", Some("rex")),
            ("TERM", Some("xterm-256color")),
            ("KITTY_WINDOW_ID", Some("1")),
        ],
        || {
            let caps = detect_capabilities(None);
            assert!(caps.hyperlinks);
            assert_eq!(caps.images, Some(ImageProtocol::Kitty));
            assert!(caps.true_color);
        },
    );
    with_env(&[("TERM_PROGRAM", Some("rex")), ("PI_HYPERLINKS", Some("0"))], || {
        assert!(!detect_capabilities(None).hyperlinks);
    });
}

#[test]
#[serial]
fn detect_capabilities_keeps_images_disabled_for_orca_without_image_signal() {
    with_env(&[("TERM_PROGRAM", Some("Orca"))], || {
        assert_eq!(detect_capabilities(None).images, None);
    });
}

#[test]
#[serial]
fn detect_capabilities_enables_images_and_hyperlinks_for_orca_advertising_kitty_support() {
    with_env(
        &[("TERM_PROGRAM", Some("Orca")), ("ORCA_IMAGE_PROTOCOL", Some("kitty"))],
        || {
            let caps = detect_capabilities(None);
            assert_eq!(caps.images, Some(ImageProtocol::Kitty));
            assert!(caps.true_color);
            assert!(caps.hyperlinks);
        },
    );
}

#[test]
#[serial]
fn detect_capabilities_enables_hyperlinks_for_alacritty_without_term_program() {
    with_env(&[("TERM", Some("alacritty")), ("COLORTERM", Some("truecolor"))], || {
        assert_eq!(
            detect_capabilities(None),
            TerminalCapabilities {
                images: None,
                true_color: true,
                hyperlinks: true,
            }
        );
    });
    with_env(
        &[
            ("TERM", Some("xterm-256color")),
            ("COLORTERM", Some("truecolor")),
            ("ALACRITTY_WINDOW_ID", Some("1")),
        ],
        || {
            assert_eq!(
                detect_capabilities(None),
                TerminalCapabilities {
                    images: None,
                    true_color: true,
                    hyperlinks: true,
                }
            );
        },
    );
}

#[test]
#[serial]
fn reads_explicit_placement_rows_without_registered_metadata() {
    let sequence = encode_kitty(
        "AAAA",
        Some(pi_tui::terminal_image::EncodeKittyOptions {
            columns: Some(2),
            rows: Some(3),
            move_cursor: Some(false),
            ..Default::default()
        }),
    );
    assert_eq!(get_kitty_image_placement_rows(&sequence), Some(3));
}

struct ImageTranscodingGuard;

impl ImageTranscodingGuard {
    fn new() -> Self {
        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });
        set_cell_dimensions(CellDimensions {
            width_px: 10,
            height_px: 10,
        });
        Self
    }
}

impl Drop for ImageTranscodingGuard {
    fn drop(&mut self) {
        set_image_transcoder(None);
        reset_capabilities_cache();
        set_cell_dimensions(CellDimensions {
            width_px: 9,
            height_px: 18,
        });
    }
}

#[test]
#[serial]
fn image_transcoding_tests() {
    // Kitty only accepts PNG (f=100); non-PNG images must be transcoded (#10292)
    let _guard = ImageTranscodingGuard::new();
    let jpeg = pi_js::b64::encode(b"jpeg");
    // Minimal PNG header (signature + IHDR) for a 40x10 image. Enough for get_png_dimensions.
    let png = pi_js::b64::encode(&[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
        0x00, 0x28, 0x00, 0x00, 0x00, 0x0a,
    ]);
    let theme = || ImageTheme {
        fallback_color: Arc::new(|value: &str| value.to_string()),
    };
    let render = |data: &str, mime_type: &str| {
        Image::new(
            data,
            mime_type,
            theme(),
            None,
            Some(ImageDimensions {
                width_px: 20,
                height_px: 20,
            }),
        )
        .render(20)
    };

    // 1. Sends converted PNG data sized from the PNG
    let calls: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let jpeg_clone = jpeg.clone();
    let png_clone = png.clone();
    let calls_ref = Arc::clone(&calls);
    set_image_transcoder(Some(Arc::new(move |data, _mime| {
        calls_ref.lock().unwrap().push(data.to_string());
        if data == jpeg_clone {
            Some(png_clone.clone())
        } else {
            None
        }
    })));
    let lines = render(&jpeg, "image/jpeg");
    assert!(lines[0].contains("f=100") && lines[0].contains(&format!(";{png}\x1b\\")));
    // 40x10 PNG at 18 columns: 5 rows, not the 18 rows of the 20x20 source dimensions.
    assert_eq!(lines.len(), 5);

    // 2. Renders a text fallback until a working transcoder is registered
    set_image_transcoder(None);
    let image = Image::new(&jpeg, "image/jpeg", theme(), None, None);
    assert!(image.render(80)[0].starts_with("[Image: [image/jpeg]"));
    set_image_transcoder(Some(Arc::new(|_, _| None)));
    image.invalidate();
    assert!(image.render(80)[0].starts_with("[Image: [image/jpeg]"));
    let jpeg_clone = jpeg.clone();
    let png_clone = png.clone();
    set_image_transcoder(Some(Arc::new(move |data, _mime| {
        if data == jpeg_clone {
            Some(png_clone.clone())
        } else {
            None
        }
    })));
    image.invalidate();
    assert!(image.render(80)[0].contains("\x1b_G"));

    // 3. Converts each image once
    calls.lock().unwrap().clear();
    let jpeg_clone = jpeg.clone();
    let png_clone = png.clone();
    let calls_ref = Arc::clone(&calls);
    set_image_transcoder(Some(Arc::new(move |data, _mime| {
        calls_ref.lock().unwrap().push(data.to_string());
        if data == jpeg_clone {
            Some(png_clone.clone())
        } else {
            None
        }
    })));
    let image = Image::new(&jpeg, "image/jpeg", theme(), None, None);
    image.render(80);
    render(&jpeg, "image/jpeg");
    for i in 0..40 {
        render(&format!("other-{i}"), "image/jpeg");
    }
    image.invalidate();
    image.render(40);
    assert_eq!(calls.lock().unwrap().iter().filter(|data| *data == &jpeg).count(), 1);

    // 4. Does not convert PNG data or iTerm2 output
    calls.lock().unwrap().clear();
    assert!(render(&png, "image/png")[0].contains(&format!(";{png}\x1b\\")));
    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Iterm2),
        true_color: true,
        hyperlinks: true,
    });
    assert!(
        render(&jpeg, "image/jpeg")
            .last()
            .is_some_and(|line| line.ends_with(&format!(":{jpeg}\x07")))
    );
    assert!(calls.lock().unwrap().is_empty());
}
