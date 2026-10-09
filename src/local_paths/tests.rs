use std::path::Path;

use super::*;

fn checking(folders: &[&str]) -> LocalPaths {
    LocalPaths::new(folders.iter().map(Path::new))
}

fn refused(paths: &LocalPaths, text: &str) -> bool {
    paths.find(text, Surface::Prose).is_some()
}

#[test]
fn a_folder_and_what_is_under_it_are_found_whatever_the_case() {
    let paths = checking(&["/Users/me/.kelpie/"]);
    for text in [
        "/Users/me/.kelpie",
        "at /Users/me/.kelpie/wt/koji/7/src/a.rs:3",
        "/USERS/ME/.KELPIE/x",
        "(/Users/me/.kelpie)",
    ] {
        assert_eq!(paths.find(text, Surface::Code), Some(Leak::Path), "{text}");
    }
}

#[test]
fn a_longer_name_that_starts_like_a_folder_is_not_it() {
    let paths = checking(&["/Users/me"]);
    for text in [
        concat!("/Us", "ers/meg/x"),
        concat!("/Us", "ers/me-2"),
        concat!("/Us", "ers/me_b"),
        "nothing here",
    ] {
        assert_eq!(paths.find(text, Surface::Prose), None, "{text}");
    }
}

#[test]
fn a_folder_with_no_parent_names_nothing() {
    let paths = checking(&["/", ""]);
    assert!(!refused(&paths, "/usr/bin/env and /"));
}

#[test]
fn an_encoded_path_is_found() {
    let paths = checking(&["/Users/me"]);
    for text in [
        "http://localhost:5173/%2FUsers%2Fme%2Fapp%2Fsrc%2Fmain.ts",
        "at /%2fusers%2fme/app",
        // Encoded twice over.
        "/%252FUsers%252Fme%252Fapp",
        "/Users%2Fme/app",
        "C%3A%5CUsers%5Calex%5Capp",
        "%2Fhome%2Falex%2Fapp",
    ] {
        assert!(refused(&paths, text), "{text}");
    }
}

#[test]
fn a_windows_path_is_found_written_either_way() {
    let paths = LocalPaths::default();
    for text in [
        r"C:\Users\alex\app\src\main.ts",
        "c:/users/alex/app",
        // Escaped in JSON.
        r"D:\\Users\\alex\\app",
    ] {
        assert_eq!(paths.find(text, Surface::Prose), Some(Leak::Path), "{text}");
        assert_eq!(paths.find(text, Surface::Code), None, "{text}");
    }
    assert!(!refused(&paths, r"C:\Windows\System32 and C:\Users\ alone"));
}

#[test]
fn a_linux_home_and_the_scratch_folders_of_macos_are_found() {
    let paths = LocalPaths::default();
    for text in [
        concat!("/ho", "me/alex/app/src/a.rs"),
        "in /private/tmp/kelpie-1/x",
        "/private/tmp",
        "/var/folders/zz/abc123/T/out.png",
        "/private/var/folders/zz/abc123/T/out.png",
        concat!("file:///ho", "me/alex/x"),
    ] {
        assert_eq!(paths.find(text, Surface::Prose), Some(Leak::Path), "{text}");
        assert_eq!(paths.find(text, Surface::Code), None, "{text}");
    }
}

#[test]
fn a_path_that_only_looks_like_a_home_is_not_one() {
    let paths = LocalPaths::default();
    for text in [
        concat!("https://example.com/ho", "me/page"),
        concat!("src/ho", "me/mod.rs"),
        "see /home/ for more",
        "/homework/a",
        "/tmp/scratch",
        "/var/foldersmith",
        "/private/tmpl",
    ] {
        assert_eq!(paths.find(text, Surface::Prose), None, "{text}");
    }
}

#[test]
fn a_symlinked_folder_is_found_in_its_canonical_form_too() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real-kelpie");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link-kelpie");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let paths = LocalPaths::new([link.as_path()]);
    let canonical = std::fs::canonicalize(&real).unwrap();
    let text = format!("built in {}/target", canonical.display());
    assert_eq!(paths.find(&text, Surface::Code), Some(Leak::Path));
    let text = format!("built in {}/target", link.display());
    assert_eq!(paths.find(&text, Surface::Code), Some(Leak::Path));
}

#[test]
fn a_path_under_the_home_folder_is_refused_in_prose_only() {
    let paths = LocalPaths::default();
    for text in ["see ~/.ssh/config", "`~/.kelpie/x`", "at\t~/notes"] {
        assert_eq!(
            paths.find(text, Surface::Prose),
            Some(Leak::Tilde),
            "{text}"
        );
        assert_eq!(paths.find(text, Surface::Code), None, "{text}");
    }
    assert_eq!(paths.find("a~/b and ~ alone", Surface::Prose), None);
}

// A dotted address, put together here so no fixture holds one.
fn ip(a: u8, b: u8, c: u8, d: u8) -> String {
    format!("{a}.{b}.{c}.{d}")
}

#[test]
fn an_address_on_a_local_network_is_refused_in_prose_only() {
    let paths = LocalPaths::default();
    for text in [
        format!("open http://{}:3000/", ip(192, 168, 1, 20)),
        format!("ssh {}", ip(10, 0, 0, 5)),
        format!("host {}.", ip(172, 16, 4, 1)),
        ip(172, 31, 255, 255),
        ip(169, 254, 1, 2),
        "http://kelpie-mbp.local:5173/".to_owned(),
        "ssh alex@mac.local".to_owned(),
        "https://studio.local/app".to_owned(),
    ] {
        let text = text.as_str();
        assert_eq!(paths.find(text, Surface::Prose), Some(Leak::Lan), "{text}");
        assert_eq!(paths.find(text, Surface::Code), None, "{text}");
    }
}

#[test]
fn an_address_that_is_not_on_a_local_network_goes_through() {
    let paths = LocalPaths::default();
    for text in [
        format!("http://{}:5173/", ip(127, 0, 0, 1)),
        ip(8, 8, 8, 8),
        ip(172, 32, 0, 1),
        ip(192, 169, 0, 1),
        format!("v{}", ip(10, 0, 0, 1)),
        format!("1.{}.5", ip(10, 0, 0, 1)),
        "10.0.0".to_owned(),
        "self.local.iter() and self.local".to_owned(),
        "local and .local".to_owned(),
    ] {
        assert_eq!(paths.find(&text, Surface::Prose), None, "{text}");
    }
}
