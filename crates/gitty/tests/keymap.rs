use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gitty::app::{Focus, Tab};
use gitty::keymap::{Action, Key, Keymap, State};

fn key(code: KeyCode, ctrl: bool) -> Key {
    Key { code, ctrl, alt: false }
}

#[test]
fn key_strings_parse() {
    let cases: &[(&str, Option<Key>)] = &[
        ("g", Some(key(KeyCode::Char('g'), false))),
        ("G", Some(key(KeyCode::Char('G'), false))),
        ("shift-g", Some(key(KeyCode::Char('G'), false))),
        ("ctrl-d", Some(key(KeyCode::Char('d'), true))),
        ("Ctrl-D", Some(key(KeyCode::Char('d'), true))),
        ("alt-x", Some(Key { code: KeyCode::Char('x'), ctrl: false, alt: true })),
        ("shift-tab", Some(key(KeyCode::BackTab, false))),
        ("F5", Some(key(KeyCode::F(5), false))),
        ("enter", Some(key(KeyCode::Enter, false))),
        ("space", Some(key(KeyCode::Char(' '), false))),
        ("pagedown", Some(key(KeyCode::PageDown, false))),
        ("<", Some(key(KeyCode::Char('<'), false))),
        ("F99", None),
        ("ctrl-", None),
        ("banana", None),
    ];
    for (s, want) in cases {
        assert_eq!(Key::parse(s), *want, "{s}");
    }
    assert_eq!(Key::parse("ctrl-d").unwrap().label(), "Ctrl-d");
    assert_eq!(Key::parse("shift-tab").unwrap().label(), "Shift-Tab");
}

fn table(s: &str) -> toml::Table {
    toml::from_str(s).unwrap()
}

fn history() -> State {
    State { tab: Tab::History, focus: Focus::History, compare: false }
}

fn ev(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn a_remapped_action_leaves_its_default_key_unbound() {
    let (m, w) = Keymap::from_config(&table("fetch = \"F5\"\n"));
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(m.resolve(&ev(KeyCode::F(5)), history()), Some(Action::Fetch));
    assert_eq!(m.resolve(&ev(KeyCode::Char('f')), history()), None);
    let (m, _) = Keymap::from_config(&table("down = [\"n\", \"ctrl-n\"]\n"));
    assert_eq!(m.resolve(&ev(KeyCode::Char('j')), history()), None);
    assert_eq!(m.resolve(&ev(KeyCode::Char('n')), history()), Some(Action::Down), "the user's key beats the default next_match");
    assert_eq!(m.resolve(&KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL), history()), Some(Action::Down));
}

#[test]
fn conflicts_warn_and_the_first_binding_wins() {
    let (m, w) = Keymap::from_config(&table("pull = \"g\"\nquit = \"g\"\n"));
    assert!(w.iter().any(|w| w.contains("bound to both") && w.contains("pull")), "{w:?}");
    assert_eq!(m.resolve(&ev(KeyCode::Char('g')), history()), Some(Action::Pull), "first in the file wins");
    assert!(w.iter().any(|w| w.contains("`quit` has no key") || w.contains("quit")), "{w:?}");
    // the same key in contexts that never overlap is fine
    let (_, w) = Keymap::from_config(&table("filter = \"x\"\nscope = \"x\"\ncancel = \"ctrl-x\"\n"));
    assert!(w.is_empty(), "Changes list and History never share a screen: {w:?}");
}

#[test]
fn unknown_names_and_keys_warn() {
    let (m, w) = Keymap::from_config(&table("fetchh = \"F5\"\npush = \"hyper-p\"\nquit = 3\n"));
    assert_eq!(w.len(), 3, "{w:?}");
    assert!(w[0].contains("unknown action `fetchh`"));
    assert!(w[1].contains("unknown key `hyper-p`"));
    assert_eq!(m.resolve(&ev(KeyCode::Char('P')), history()), Some(Action::Push), "a bad binding keeps the default");
    assert_eq!(m.resolve(&ev(KeyCode::Char('q')), history()), Some(Action::Quit));
}

#[test]
fn contexts_pick_the_meaning() {
    let m = Keymap::default();
    let changes_list = State { tab: Tab::Changes, focus: Focus::Files, compare: false };
    let changes_diff = State { tab: Tab::Changes, focus: Focus::Diff, compare: false };
    assert_eq!(m.resolve(&ev(KeyCode::Char('F')), changes_list), Some(Action::Filter));
    assert_eq!(m.resolve(&ev(KeyCode::Char('F')), changes_diff), Some(Action::Fullscreen));
    assert_eq!(m.resolve(&ev(KeyCode::Char('F')), history()), Some(Action::Fullscreen));
    assert_eq!(m.resolve(&ev(KeyCode::Char('r')), changes_diff), None, "history-only keys do nothing in Changes");
    let compare = State { tab: Tab::History, focus: Focus::History, compare: true };
    assert_eq!(m.resolve(&ev(KeyCode::Char('l')), compare), Some(Action::CompareAhead));
    assert_eq!(m.resolve(&ev(KeyCode::Char('l')), history()), Some(Action::ScrollRight));
}

#[test]
fn readme_key_table_matches_the_keymap() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md");
    let readme = std::fs::read_to_string(path).expect("README.md at the repo root");
    let start = readme.find("<!-- keys:start -->").expect("keys:start marker") + "<!-- keys:start -->".len();
    let end = readme.find("<!-- keys:end -->").expect("keys:end marker");
    let table = Keymap::default().markdown();
    // GITTY_BLESS=1 cargo test -p gitty-cli --test keymap rewrites the table
    if std::env::var_os("GITTY_BLESS").is_some() {
        std::fs::write(path, format!("{}\n{}\n{}", &readme[..start], table.trim(), &readme[end..])).unwrap();
        return;
    }
    assert_eq!(readme[start..end].trim(), table.trim(), "the README key table is stale: GITTY_BLESS=1 cargo test -p gitty-cli --test keymap");
}

#[test]
fn binding_a_fixed_key_warns_and_is_dropped() {
    let (map, w) = Keymap::from_config(&table("fetch = [\"ctrl-c\", \"F5\"]\npush = \"ctrl-z\"\n"));
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w[0].contains("Ctrl-c") && w[0].contains("fixed"), "{w:?}");
    assert!(w[1].contains("Ctrl-z"), "{w:?}");
    let labels = |a: &str| map.bindings().into_iter().find(|b| b.1 == a).map(|b| b.2.iter().map(|k| k.label()).collect::<Vec<_>>()).unwrap();
    assert_eq!(labels("fetch"), ["F5"]);
    assert_eq!(labels("push"), ["P"], "a binding with only fixed keys keeps the default");
}

#[test]
fn b_opens_the_branches_from_every_screen() {
    let m = Keymap::default();
    let changes = State { tab: Tab::Changes, focus: Focus::Files, compare: false };
    for s in [changes, history()] {
        assert_eq!(m.resolve(&ev(KeyCode::Char('B')), s), Some(Action::Branches));
    }
}
