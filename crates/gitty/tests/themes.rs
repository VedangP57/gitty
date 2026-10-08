use gitty::theme::{ColorDepth, Registry, BUILTIN_NAMES};
use ratatui::style::{Color, Modifier};

fn user_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (name, body) in files {
        std::fs::write(d.path().join(name), body).unwrap();
    }
    d
}

const ORIGINAL: [&str; 11] = [
    "github-dark", "github-light", "rose-pine", "rose-pine-dawn", "catppuccin-mocha", "catppuccin-latte",
    "tokyo-night", "dracula", "gruvbox-dark", "solarized-dark", "solarized-light",
];
const ADDED: [&str; 12] = [
    "nord", "one-dark", "one-light", "gruvbox-light", "catppuccin-frappe", "catppuccin-macchiato",
    "tokyo-night-storm", "tokyo-night-day", "kanagawa", "everforest-dark", "ayu-mirage", "nightfox",
];

#[test]
fn builtin_names_complete() {
    let want: Vec<String> = ORIGINAL.iter().chain(ADDED.iter()).map(|s| s.to_string()).collect();
    assert_eq!(BUILTIN_NAMES.map(String::from).to_vec(), want);
    let r = Registry::load(None);
    assert_eq!(r.names()[..want.len()], want[..]);
    assert!(r.errors().is_empty(), "{:?}", r.errors());
}

#[test]
fn theme_picker_lists_the_added_themes() {
    // the `T` picker is built from `Registry::names()`
    let names = Registry::load(None).names();
    for n in ADDED {
        assert!(names.iter().any(|x| x == n), "{n} missing from the picker list");
    }
}

fn luminance(c: Color) -> f64 {
    let Color::Rgb(r, g, b) = c else { panic!("not truecolor: {c:?}") };
    let lin = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

fn contrast(a: Color, b: Color) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[test]
fn added_themes_are_legible_and_flagged_by_their_background() {
    let r = Registry::load(None);
    let reference = r.resolve("github-dark", ColorDepth::True, None).unwrap();
    for name in ADDED {
        let t = r.resolve(name, ColorDepth::True, None).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_eq!(t.is_light, luminance(t.ui.bg) > 0.18, "{name}: kind flag disagrees with the background");
        assert_eq!(t.is_light, ["one-light", "gruvbox-light", "tokyo-night-day"].contains(&name), "{name}");
        let body = contrast(t.ui.fg, t.ui.bg);
        assert!(body >= 4.5, "{name}: fg/bg contrast {body:.2}");
        let muted = contrast(t.ui.muted, t.ui.bg);
        assert!(muted >= 3.0, "{name}: muted/bg contrast {muted:.2}");
        let mut keys: Vec<_> = t.syntax.keys().collect();
        let mut want: Vec<_> = reference.syntax.keys().collect();
        keys.sort();
        want.sort();
        assert_eq!(keys, want, "{name}: syntax captures differ from the other builtins");
        for (cap, style) in &t.syntax {
            assert!(style.fg.is_some(), "{name}: {cap} has no colour");
        }
    }
}

#[test]
fn builtin_kind_flag_matches_background_luminance() {
    let r = Registry::load(None);
    for name in BUILTIN_NAMES {
        let t = r.resolve(name, ColorDepth::True, None).unwrap();
        assert_eq!(t.is_light, luminance(t.ui.bg) > 0.18, "{name}");
    }
}

#[test]
fn every_builtin_resolves_in_both_depths() {
    let r = Registry::load(None);
    for name in BUILTIN_NAMES {
        let t = r.resolve(name, ColorDepth::True, None).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_eq!(t.name, name);
        assert!(matches!(t.ui.bg, Color::Rgb(..)), "{name}");
        assert_ne!(t.diff.add_bg, t.diff.del_bg, "{name}");
        assert_ne!(t.diff.add_bg, t.ui.bg, "{name}");
        assert_ne!(t.diff.add_emph, t.diff.add_bg, "{name}");
        for cap in ["keyword", "string", "comment", "function", "type"] {
            assert!(t.syntax.contains_key(cap), "{name} lacks {cap}");
        }
        let t = r.resolve(name, ColorDepth::Ansi256, None).unwrap();
        assert!(matches!(t.ui.bg, Color::Indexed(_)), "{name}");
        assert!(matches!(t.diff.add_emph, Color::Indexed(_)), "{name}");
    }
}

#[test]
fn light_kind_flag() {
    let r = Registry::load(None);
    for (n, light) in [("github-light", true), ("github-dark", false), ("rose-pine-dawn", true), ("solarized-light", true), ("dracula", false)] {
        assert_eq!(r.resolve(n, ColorDepth::True, None).unwrap().is_light, light, "{n}");
    }
    assert_eq!(Registry::pick_auto(true), "github-light");
    assert_eq!(Registry::pick_auto(false), "github-dark");
}

#[test]
fn inherit_overrides_only_given_keys() {
    let d = user_dir(&[("mine.toml", "name = \"mine\"\ninherit = \"github-dark\"\n[ui]\naccent = \"#ff0000\"\n")]);
    let r = Registry::load(Some(d.path()));
    let base = r.resolve("github-dark", ColorDepth::True, None).unwrap();
    let mine = r.resolve("mine", ColorDepth::True, None).unwrap();
    assert_eq!(mine.ui.accent, Color::Rgb(255, 0, 0));
    assert_eq!(mine.ui.bg, base.ui.bg);
    assert_eq!(mine.diff.add_bg, base.diff.add_bg);
    assert!(!mine.is_light);
    assert!(r.names().contains(&"mine".to_string()));
}

#[test]
fn palette_names_usable_as_values() {
    let d = user_dir(&[("p.toml", "name = \"p\"\ninherit = \"github-dark\"\n[ui]\nahead = \"magenta\"\n[syntax]\nkeyword = { fg = \"green\", bold = true }\n")]);
    let r = Registry::load(Some(d.path()));
    let p = r.resolve("p", ColorDepth::True, None).unwrap();
    assert_eq!(p.ui.ahead, Color::Rgb(0xbc, 0x8c, 0xff)); // github-dark palette magenta
    assert_eq!(p.syntax["keyword"].fg, Some(Color::Rgb(0x3f, 0xb9, 0x50))); // palette green
    assert!(p.syntax["keyword"].add_modifier.contains(Modifier::BOLD));
}

#[test]
fn pull_request_colours_follow_the_palette_and_can_be_set() {
    let d = user_dir(&[("p.toml", "name = \"p\"\ninherit = \"github-dark\"\n[ui]\npr_merged = \"#112233\"\npr_draft = \"cyan\"\n")]);
    let r = Registry::load(Some(d.path()));
    let base = r.resolve("github-dark", ColorDepth::True, None).unwrap();
    let p = r.resolve("p", ColorDepth::True, None).unwrap();
    assert_eq!(base.ui.pr_open, base.ui.status_added);
    assert_eq!(base.ui.pr_closed, base.ui.error);
    assert_eq!(base.ui.pr_draft, base.ui.muted);
    assert_eq!(p.ui.pr_merged, Color::Rgb(0x11, 0x22, 0x33));
    assert_ne!(p.ui.pr_draft, base.ui.pr_draft);
    assert_eq!(p.ui.pr_open, base.ui.pr_open);
}

#[test]
fn inherit_cycle_errors() {
    let d = user_dir(&[
        ("a.toml", "name = \"a\"\ninherit = \"b\"\n"),
        ("b.toml", "name = \"b\"\ninherit = \"a\"\n"),
    ]);
    let r = Registry::load(Some(d.path()));
    let e = format!("{:#}", r.resolve("a", ColorDepth::True, None).unwrap_err());
    assert!(e.contains("cycle"), "{e}");
}

#[test]
fn unknown_parent_errors() {
    let d = user_dir(&[("a.toml", "name = \"a\"\ninherit = \"nope\"\n")]);
    let r = Registry::load(Some(d.path()));
    let e = format!("{:#}", r.resolve("a", ColorDepth::True, None).unwrap_err());
    assert!(e.contains("nope"), "{e}");
    assert!(r.resolve("missing", ColorDepth::True, None).is_err());
}

#[test]
fn incomplete_theme_names_missing_key() {
    let d = user_dir(&[("a.toml", "name = \"a\"\n[palette]\nbg = \"#000000\"\n")]);
    let r = Registry::load(Some(d.path()));
    let e = format!("{:#}", r.resolve("a", ColorDepth::True, None).unwrap_err());
    assert!(e.contains("fg"), "{e}");
}

#[test]
fn user_theme_overrides_builtin_by_name() {
    let d = user_dir(&[("gh.toml", "name = \"github-dark\"\ninherit = \"github-dark\"\n[palette]\nbg = \"#000000\"\n")]);
    let r = Registry::load(Some(d.path()));
    let t = r.resolve("github-dark", ColorDepth::True, None).unwrap();
    assert_eq!(t.ui.bg, Color::Rgb(0, 0, 0));
    assert_eq!(r.names().iter().filter(|n| *n == "github-dark").count(), 1);
}

#[test]
fn bad_user_toml_reported_not_fatal() {
    let d = user_dir(&[("bad.toml", "name = [unclosed"), ("ok.toml", "name = \"ok\"\ninherit = \"dracula\"\n"), ("notes.txt", "x")]);
    let r = Registry::load(Some(d.path()));
    assert_eq!(r.errors().len(), 1);
    assert!(r.errors()[0].contains("bad.toml"));
    assert!(r.resolve("ok", ColorDepth::True, None).is_ok());
}

#[test]
fn name_defaults_to_file_stem() {
    let d = user_dir(&[("stemmy.toml", "inherit = \"dracula\"\n")]);
    let r = Registry::load(Some(d.path()));
    assert!(r.resolve("stemmy", ColorDepth::True, None).is_ok());
}

#[test]
fn emph_alpha_override_changes_emph_only() {
    let r = Registry::load(None);
    let a = r.resolve("github-dark", ColorDepth::True, None).unwrap();
    let b = r.resolve("github-dark", ColorDepth::True, Some(0.40)).unwrap();
    assert_ne!(a.diff.add_emph, b.diff.add_emph);
    assert_ne!(a.diff.del_emph, b.diff.del_emph);
    assert_eq!(a.diff.add_bg, b.diff.add_bg);
    assert_eq!(a.ui.bg, b.ui.bg);
}

#[test]
fn colorterm_detection() {
    let env = |v: Option<&'static str>| move |k: &str| if k == "COLORTERM" { v.map(String::from) } else { None };
    assert_eq!(ColorDepth::detect(env(Some("truecolor"))), ColorDepth::True);
    assert_eq!(ColorDepth::detect(env(Some("24bit"))), ColorDepth::True);
    assert_eq!(ColorDepth::detect(env(Some(""))), ColorDepth::Ansi256);
    assert_eq!(ColorDepth::detect(env(None)), ColorDepth::Ansi256);
}
