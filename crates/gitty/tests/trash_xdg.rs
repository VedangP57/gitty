//! Where discarded files are copied: the desktop Trash of each platform, and on Linux the
//! freedesktop.org layout, so the file manager lists the copy and can restore it.

use std::ffi::OsString;

use gitty::write::{Trash, copy_to_trash, trash_location};

fn env<'a>(vars: &'a [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |k| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
}

#[test]
fn the_trash_is_the_desktop_one_on_each_platform() {
    assert_eq!(trash_location(env(&[("HOME", "/h")]), true), Some(Trash::Dir("/h/.Trash".into())));
    assert_eq!(trash_location(env(&[("HOME", "/h")]), false), Some(Trash::Xdg("/h/.local/share/Trash".into())));
    assert_eq!(trash_location(env(&[("HOME", "/h"), ("XDG_DATA_HOME", "/d")]), false), Some(Trash::Xdg("/d/Trash".into())));
    assert_eq!(trash_location(env(&[("HOME", "/h"), ("GITTY_TRASH_DIR", "/t")]), false), Some(Trash::Dir("/t".into())));
    assert_eq!(trash_location(env(&[]), false), None);
}

#[test]
fn a_linux_trash_copy_can_be_restored_by_the_file_manager() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let file = work.path().join("a b%.txt");
    std::fs::write(&file, "edited\n").unwrap();
    let trash = Trash::Xdg(home.path().join("Trash"));

    let dest = copy_to_trash(&file, &trash).unwrap();
    assert_eq!(dest.parent(), Some(home.path().join("Trash/files").as_path()));
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "edited\n");
    let name = dest.file_name().unwrap().to_string_lossy().into_owned();
    let info = std::fs::read_to_string(home.path().join(format!("Trash/info/{name}.trashinfo"))).unwrap();
    let lines: Vec<&str> = info.lines().collect();
    assert_eq!(lines[0], "[Trash Info]");
    assert!(lines[1].starts_with("Path=/") && lines[1].ends_with("/a%20b%25.txt"), "{}", lines[1]);
    let date = lines[2].strip_prefix("DeletionDate=").unwrap();
    let shape: String = date.chars().map(|c| if c.is_ascii_digit() { '9' } else { c }).collect();
    assert_eq!(shape, "9999-99-99T99:99:99", "{date}");

    // a second discard of the same file gets its own name and record
    let again = copy_to_trash(&file, &trash).unwrap();
    assert_ne!(again, dest);
    let again_name = again.file_name().unwrap().to_string_lossy().into_owned();
    assert!(home.path().join(format!("Trash/info/{again_name}.trashinfo")).is_file());
}
