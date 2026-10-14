//! Screenshot file names, such as `000001 - Foundry VTT.png`.

use std::path::{Path, PathBuf};

use crate::encode::Format;

/// Longest window-title part of a file name, in characters, to keep paths well under Windows'
/// 260-character limit.
const MAX_TITLE_CHARS: usize = 80;

/// Turns a window title into text that's safe in a file name on Windows and Linux.
pub fn sanitize_title(title: &str) -> String {
    let replaced: String = title
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated: String = collapsed.chars().take(MAX_TITLE_CHARS).collect();
    // Windows doesn't allow names ending in a dot or space.
    let trimmed = truncated.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        "Window".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn file_name(number: u64, title: &str, format: Format) -> String {
    format!(
        "{number:06} - {}.{}",
        sanitize_title(title),
        format.extension()
    )
}

/// The folder name for one recording, such as `2026-10-14 19-30-05 - Foundry VTT`.
pub fn recording_name(started: &str, title: &str) -> String {
    format!("{started} - {}", sanitize_title(title))
}

/// A path in `base` for a new recording that isn't already taken by a folder or ZIP file. If two
/// recordings start in the same second, the second gets ` (2)` added, and so on.
pub fn unique_recording_dir(base: &Path, name: &str) -> PathBuf {
    (1u32..)
        .map(|n| {
            if n == 1 {
                name.to_string()
            } else {
                format!("{name} ({n})")
            }
        })
        .find(|candidate| {
            !base.join(candidate).exists() && !base.join(format!("{candidate}.zip")).exists()
        })
        .map(|candidate| base.join(candidate))
        .expect("there is always a free name")
}

/// Reads the number from a name shaped like `000042 - anything`.
#[cfg(test)]
fn leading_number(name: &str) -> Option<u64> {
    let (digits, _) = name.split_once(" - ")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_characters_windows_forbids() {
        assert_eq!(
            sanitize_title("Foundry VTT • Game: Night/1"),
            "Foundry VTT • Game Night 1"
        );
        assert_eq!(sanitize_title(r#"a<b>c"d|e?f*g\h"#), "a b c d e f g h");
    }

    #[test]
    fn removes_control_characters_and_extra_spaces() {
        assert_eq!(sanitize_title("Tab\there\nnew   line"), "Tab here new line");
    }

    #[test]
    fn trims_trailing_dots_and_spaces() {
        assert_eq!(sanitize_title("Loading... "), "Loading");
    }

    #[test]
    fn empty_titles_get_a_placeholder() {
        assert_eq!(sanitize_title(""), "Window");
        assert_eq!(sanitize_title(" :/ "), "Window");
    }

    #[test]
    fn long_titles_are_shortened() {
        let long = "é".repeat(200);
        assert_eq!(sanitize_title(&long).chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn file_names_are_padded_and_sortable() {
        assert_eq!(
            file_name(7, "Foundry VTT", Format::Png),
            "000007 - Foundry VTT.png"
        );
        assert_eq!(file_name(123456, "x", Format::Jpg), "123456 - x.jpg");
    }

    #[test]
    fn recording_folders_are_named_by_start_time_and_window() {
        assert_eq!(
            recording_name("2026-10-14 19-30-05", "Foundry VTT: Session 3"),
            "2026-10-14 19-30-05 - Foundry VTT Session 3"
        );
    }

    #[test]
    fn recordings_started_in_the_same_second_get_different_folders() {
        let base = crate::test_dir("unique-dirs");
        let name = "2026-10-14 19-30-05 - VTT";
        assert_eq!(unique_recording_dir(&base, name), base.join(name));
        std::fs::create_dir(base.join(name)).unwrap();
        assert_eq!(
            unique_recording_dir(&base, name),
            base.join(format!("{name} (2)"))
        );
        // A packed ZIP counts as taken too.
        std::fs::write(base.join(format!("{name} (2).zip")), b"").unwrap();
        assert_eq!(
            unique_recording_dir(&base, name),
            base.join(format!("{name} (3)"))
        );
    }

    // Property tests: proptest generates thousands of random inputs, including strange Unicode,
    // and checks that these rules hold for every one of them.
    mod properties {
        use super::super::*;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(5000))]

            #[test]
            fn any_title_becomes_a_safe_file_name(title in any::<String>()) {
                let clean = sanitize_title(&title);
                prop_assert!(!clean.is_empty());
                prop_assert!(clean.chars().count() <= MAX_TITLE_CHARS);
                prop_assert!(!clean.chars().any(|c| c.is_control() || r#"<>:"/\|?*"#.contains(c)));
                prop_assert!(!clean.ends_with('.') && !clean.ends_with(' ') && !clean.starts_with(' '));
            }

            #[test]
            fn every_file_name_reads_back_its_number(n in 0u64..u64::MAX, title in any::<String>()) {
                prop_assert_eq!(leading_number(&file_name(n, &title, Format::Png)), Some(n));
            }

            #[test]
            fn file_names_sort_in_shot_order(a in 1u64..999_999, b in 1u64..999_999, title in "[a-zA-Z ]{0,20}") {
                let (fa, fb) = (file_name(a, &title, Format::Png), file_name(b, &title, Format::Png));
                prop_assert_eq!(a.cmp(&b), fa.cmp(&fb));
            }
        }
    }
}
