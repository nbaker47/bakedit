//! The order entries appear in the project panel: directories first, then files, each
//! compared case-insensitively with numbers in their natural order.

use std::cmp::Ordering;

/// Compare two entries of one directory.
///
/// Directories sort before files. Directory names are compared whole; file names are
/// compared by stem first and extension second (as Zed does), so `a.txt` comes before
/// `a-b.txt`. Names that only differ in case fall back to a byte comparison, which makes
/// the order total and stable.
pub(crate) fn compare_entries(a_is_dir: bool, a: &str, b_is_dir: bool, b: &str) -> Ordering {
    match (a_is_dir, b_is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (true, true) => natural_cmp(a, b).then_with(|| a.cmp(b)),
        (false, false) => {
            let (a_stem, a_extension) = split_extension(a);
            let (b_stem, b_extension) = split_extension(b);
            natural_cmp(a_stem, b_stem)
                .then_with(|| natural_cmp(a_extension, b_extension))
                .then_with(|| a.cmp(b))
        }
    }
}

/// Split a file name at its last dot. A leading dot is part of the stem, so `.gitignore`
/// has no extension.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot + 1..]),
        _ => (name, ""),
    }
}

/// Case-insensitive comparison in which runs of ASCII digits compare as numbers, so
/// `file2` sorts before `file10`. Numbers of any length are handled without overflow.
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a_bytes, b_bytes) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a_bytes.len() && j < b_bytes.len() {
        if a_bytes[i].is_ascii_digit() && b_bytes[j].is_ascii_digit() {
            let (a_start, b_start) = (i, j);
            while i < a_bytes.len() && a_bytes[i].is_ascii_digit() {
                i += 1;
            }
            while j < b_bytes.len() && b_bytes[j].is_ascii_digit() {
                j += 1;
            }
            let a_number = a[a_start..i].trim_start_matches('0');
            let b_number = b[b_start..j].trim_start_matches('0');
            let ordering = a_number
                .len()
                .cmp(&b_number.len())
                .then_with(|| a_number.cmp(b_number));
            if ordering != Ordering::Equal {
                return ordering;
            }
            continue;
        }
        // Both indices are on character boundaries: digits are single bytes and every
        // other step below advances by a whole character.
        let (Some(a_char), Some(b_char)) = (a[i..].chars().next(), b[j..].chars().next()) else {
            break;
        };
        let ordering = if a_char.is_ascii() && b_char.is_ascii() {
            a_char
                .to_ascii_lowercase()
                .cmp(&b_char.to_ascii_lowercase())
        } else {
            a_char.to_lowercase().cmp(b_char.to_lowercase())
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
        i += a_char.len_utf8();
        j += b_char.len_utf8();
    }
    (a_bytes.len() - i).cmp(&(b_bytes.len() - j))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut entries: Vec<(bool, &str)>) -> Vec<&str> {
        entries.sort_by(|a, b| compare_entries(a.0, a.1, b.0, b.1));
        entries.into_iter().map(|(_, name)| name).collect()
    }

    fn files(names: &[&'static str]) -> Vec<(bool, &'static str)> {
        names.iter().map(|name| (false, *name)).collect()
    }

    #[test]
    fn numbers_sort_naturally() {
        assert_eq!(
            sorted(files(&["file10.txt", "file2.txt", "file1.txt", "file.txt"])),
            ["file.txt", "file1.txt", "file2.txt", "file10.txt"]
        );
        assert_eq!(natural_cmp("v9", "v10"), Ordering::Less);
        assert_eq!(natural_cmp("v10", "v9"), Ordering::Greater);
        assert_eq!(natural_cmp("a2b3", "a2b10"), Ordering::Less);
    }

    #[test]
    fn huge_numbers_do_not_overflow() {
        let small = "n99999999999999999999999999999999999998";
        let large = "n99999999999999999999999999999999999999";
        assert_eq!(natural_cmp(small, large), Ordering::Less);
        assert_eq!(natural_cmp(large, small), Ordering::Greater);
        assert_eq!(
            natural_cmp("n100000000000000000000000000000000000000", large),
            Ordering::Greater
        );
    }

    #[test]
    fn leading_zeros_compare_by_value_then_bytes() {
        assert_eq!(natural_cmp("a007", "a7"), Ordering::Equal);
        assert_eq!(natural_cmp("a007", "a8"), Ordering::Less);
        // Equal by value: the byte comparison decides, deterministically.
        assert_eq!(sorted(files(&["a7", "a007"])), ["a007", "a7"]);
    }

    #[test]
    fn case_is_ignored_until_names_tie() {
        assert_eq!(
            sorted(files(&["banana", "Apple", "cherry", "apple"])),
            ["Apple", "apple", "banana", "cherry"]
        );
        assert_eq!(natural_cmp("README", "readme"), Ordering::Equal);
        assert_eq!(natural_cmp("Éclair", "éclair"), Ordering::Equal);
    }

    #[test]
    fn directories_come_before_files() {
        assert_eq!(
            sorted(vec![
                (false, "a.txt"),
                (true, "zeta"),
                (false, "Makefile"),
                (true, "alpha"),
                (true, ".github"),
                (false, ".gitignore"),
            ]),
            [
                ".github",
                "alpha",
                "zeta",
                ".gitignore",
                "a.txt",
                "Makefile"
            ]
        );
    }

    #[test]
    fn files_compare_by_stem_before_extension() {
        assert_eq!(
            sorted(files(&["a-b.txt", "a.txt", "a.md"])),
            ["a.md", "a.txt", "a-b.txt"]
        );
        assert_eq!(
            sorted(files(&["foo.test.ts", "foo.ts", "foo.d.ts"])),
            ["foo.ts", "foo.d.ts", "foo.test.ts"]
        );
        assert_eq!(
            sorted(files(&["main_test.rs", "main.rs", "main.c"])),
            ["main.c", "main.rs", "main_test.rs"]
        );
    }

    #[test]
    fn dotfiles_have_no_extension() {
        assert_eq!(split_extension(".gitignore"), (".gitignore", ""));
        assert_eq!(split_extension(".env.local"), (".env", "local"));
        assert_eq!(split_extension("archive.tar.gz"), ("archive.tar", "gz"));
        assert_eq!(split_extension("Makefile"), ("Makefile", ""));
        assert_eq!(split_extension("trailing."), ("trailing", ""));
    }

    #[test]
    fn prefixes_sort_first() {
        assert_eq!(natural_cmp("file", "file2"), Ordering::Less);
        assert_eq!(natural_cmp("file2", "file"), Ordering::Greater);
        assert_eq!(natural_cmp("", ""), Ordering::Equal);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
    }

    #[test]
    fn the_order_is_total() {
        // Antisymmetry and transitivity over a set with every kind of tie.
        let names = ["a1", "a01", "A1", "a1b", "a", "", "á", "a.b", "a-b"];
        for x in names {
            for y in names {
                let forward = compare_entries(false, x, false, y);
                let backward = compare_entries(false, y, false, x);
                assert_eq!(forward, backward.reverse(), "{x:?} vs {y:?}");
                if x != y {
                    assert_ne!(forward, Ordering::Equal, "{x:?} vs {y:?}");
                }
                for z in names {
                    if forward == Ordering::Less
                        && compare_entries(false, y, false, z) == Ordering::Less
                    {
                        assert_eq!(
                            compare_entries(false, x, false, z),
                            Ordering::Less,
                            "{x:?} < {y:?} < {z:?}"
                        );
                    }
                }
            }
        }
    }
}
