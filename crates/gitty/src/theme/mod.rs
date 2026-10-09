//! Themes: a TOML palette plus optional UI, diff, avatar and syntax overrides, resolved once into
//! ratatui colours (truecolor, or the nearest xterm-256 entry).
//!
//! A theme's `[palette]` names base colours (bg, fg, muted, accent, border, red, green, yellow,
//! blue, magenta, cyan, optional panel/orange). Every UI and diff colour has a default derived
//! from the palette, so a theme only states what differs. Values are `#rrggbb`, `#rgb`, or a
//! palette name. `inherit = "<theme>"` layers a theme over another.

pub mod color;

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, anyhow, bail};
use ratatui::style::{Color, Modifier, Style};

use color::Rgb;

pub const BUILTIN_NAMES: [&str; 23] = [
    "github-dark",
    "github-light",
    "rose-pine",
    "rose-pine-dawn",
    "catppuccin-mocha",
    "catppuccin-latte",
    "tokyo-night",
    "dracula",
    "gruvbox-dark",
    "solarized-dark",
    "solarized-light",
    "nord",
    "one-dark",
    "one-light",
    "gruvbox-light",
    "catppuccin-frappe",
    "catppuccin-macchiato",
    "tokyo-night-storm",
    "tokyo-night-day",
    "kanagawa",
    "everforest-dark",
    "ayu-mirage",
    "nightfox",
];

const BUILTINS: [&str; 23] = [
    include_str!("builtin/github-dark.toml"),
    include_str!("builtin/github-light.toml"),
    include_str!("builtin/rose-pine.toml"),
    include_str!("builtin/rose-pine-dawn.toml"),
    include_str!("builtin/catppuccin-mocha.toml"),
    include_str!("builtin/catppuccin-latte.toml"),
    include_str!("builtin/tokyo-night.toml"),
    include_str!("builtin/dracula.toml"),
    include_str!("builtin/gruvbox-dark.toml"),
    include_str!("builtin/solarized-dark.toml"),
    include_str!("builtin/solarized-light.toml"),
    include_str!("builtin/nord.toml"),
    include_str!("builtin/one-dark.toml"),
    include_str!("builtin/one-light.toml"),
    include_str!("builtin/gruvbox-light.toml"),
    include_str!("builtin/catppuccin-frappe.toml"),
    include_str!("builtin/catppuccin-macchiato.toml"),
    include_str!("builtin/tokyo-night-storm.toml"),
    include_str!("builtin/tokyo-night-day.toml"),
    include_str!("builtin/kanagawa.toml"),
    include_str!("builtin/everforest-dark.toml"),
    include_str!("builtin/ayu-mirage.toml"),
    include_str!("builtin/nightfox.toml"),
];

const DEFAULT_EMPH_ALPHA: f32 = 0.25;
const MAX_INHERIT_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    True,
    Ansi256,
}

impl ColorDepth {
    pub fn detect(env: impl Fn(&str) -> Option<String>) -> ColorDepth {
        match env("COLORTERM").as_deref() {
            Some("truecolor" | "24bit") => ColorDepth::True,
            _ => ColorDepth::Ansi256,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiColors {
    pub bg: Color,
    pub panel: Color,
    pub border: Color,
    pub border_focus: Color,
    pub fg: Color,
    pub muted: Color,
    pub accent: Color,
    pub selection: Color,
    pub selection_inactive: Color,
    pub ahead: Color,
    pub behind: Color,
    pub badge_head_fg: Color,
    pub badge_head_bg: Color,
    pub badge_local_fg: Color,
    pub badge_local_bg: Color,
    pub badge_remote_fg: Color,
    pub badge_remote_bg: Color,
    pub badge_tag_fg: Color,
    pub badge_tag_bg: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    pub error: Color,
    pub warning: Color,
    pub status_added: Color,
    pub status_modified: Color,
    pub status_deleted: Color,
    pub status_renamed: Color,
    /// The pull-request badge in the top bar, by state (GitHub's colours).
    pub pr_open: Color,
    pub pr_draft: Color,
    pub pr_merged: Color,
    pub pr_closed: Color,
    /// The commit graph's lane colours, one per colour index of a lane (see [`lane_palette`]).
    pub lanes: [Color; LANE_COLOURS],
}

/// Colour indices a graph lane cycles through.
pub const LANE_COLOURS: usize = gitty_core::graph::COLOURS as usize;

/// Two colours a reader would take for one another.
fn same_looking(a: Rgb, b: Rgb) -> bool {
    let d = |x: u8, y: u8| i32::from(x).abs_diff(i32::from(y));
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2) < 48
}

/// How far a colour is from grey: its largest channel less its smallest.
fn chroma(c: Rgb) -> u8 {
    c.0.max(c.1).max(c.2) - c.0.min(c.1).min(c.2)
}

/// Below this a hue reads as grey next to the list's text.
const VIVID: u8 = 40;

/// The graph's lane colours: the vivid, distinct `hues`, the most vivid first (lane 0, the main
/// line, gets the strongest). None is like a colour in `avoid` (the backgrounds and the text
/// colours), nor like an earlier hue, nor the same colour once mapped to the terminal's depth. A
/// theme with fewer than [`LANE_COLOURS`] of them cycles those, and no two neighbours in the cycle
/// (the last and the first included) are the same while it has three or more.
fn lane_palette(hues: &[Rgb], avoid: &[Rgb], color: impl Fn(Rgb) -> Color) -> [Color; LANE_COLOURS] {
    let mut hues = hues.to_vec();
    hues.sort_by_key(|&h| std::cmp::Reverse(chroma(h)));
    let mut picked: Vec<(Rgb, Color)> = Vec::new();
    for &h in hues.iter().filter(|&&h| chroma(h) >= VIVID) {
        let c = color(h);
        if avoid.iter().any(|&a| same_looking(a, h) || color(a) == c) || picked.iter().any(|&(p, pc)| same_looking(p, h) || pc == c) {
            continue;
        }
        picked.push((h, c));
        if picked.len() == LANE_COLOURS {
            break;
        }
    }
    if picked.is_empty() {
        // no hue at all stands out: the most colourful one everywhere
        picked.push((hues[0], color(hues[0])));
    }
    let n = picked.len();
    let mut out: [Color; LANE_COLOURS] = std::array::from_fn(|i| picked[i % n].1);
    if n >= 3 && out[LANE_COLOURS - 1] == out[0] {
        out[LANE_COLOURS - 1] = picked[1].1;
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffColors {
    pub add_bg: Color,
    pub del_bg: Color,
    pub add_gutter: Color,
    pub del_gutter: Color,
    pub add_emph: Color,
    pub del_emph: Color,
    pub add_fg: Color,
    pub del_fg: Color,
    pub context_fg: Color,
    pub lineno: Color,
    pub lineno_add: Color,
    pub lineno_del: Color,
    pub hunk_bg: Color,
    pub hunk_fg: Color,
    pub expand_bg: Color,
    pub expand_fg: Color,
    pub filler: Color,
    pub cursor: Color,
    /// The conflict view: each side's rows, the side of the block the cursor is in, and its header.
    pub ours_bg: Color,
    pub ours_current_bg: Color,
    pub ours_head: Color,
    pub theirs_bg: Color,
    pub theirs_current_bg: Color,
    pub theirs_head: Color,
    /// The merge base of a diff3 block.
    pub base_bg: Color,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub is_light: bool,
    pub ui: UiColors,
    pub diff: DiffColors,
    pub avatar: [Color; 8],
    /// tree-sitter capture name → style.
    pub syntax: HashMap<String, Style>,
}

#[derive(Debug, Clone, Default)]
struct SyntaxSpec {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    italic: bool,
    underline: bool,
}

#[derive(Debug, Clone, Default)]
struct Spec {
    name: String,
    kind: Option<String>,
    inherit: Option<String>,
    emph_alpha: Option<f32>,
    row_alpha: Option<f32>,
    /// "palette.bg", "ui.accent", "diff.add_bg", "avatar.c0" → value.
    colors: HashMap<String, String>,
    syntax: HashMap<String, SyntaxSpec>,
}

fn parse_spec(text: &str, fallback_name: &str) -> anyhow::Result<Spec> {
    let table: toml::Table = toml::from_str(text)?;
    let mut s = Spec { name: fallback_name.to_string(), ..Spec::default() };
    let str_of = |v: &toml::Value, key: &str| v.as_str().map(String::from).ok_or_else(|| anyhow!("`{key}` must be a string"));
    let float_of = |v: &toml::Value, key: &str| {
        v.as_float().or_else(|| v.as_integer().map(|i| i as f64)).map(|f| f as f32).ok_or_else(|| anyhow!("`{key}` must be a number"))
    };
    for (k, v) in &table {
        match k.as_str() {
            "name" => s.name = str_of(v, k)?,
            "kind" => {
                let kind = str_of(v, k)?;
                if kind != "dark" && kind != "light" {
                    bail!("`kind` must be \"dark\" or \"light\"");
                }
                s.kind = Some(kind);
            }
            "inherit" => s.inherit = Some(str_of(v, k)?),
            "emph_alpha" => s.emph_alpha = Some(float_of(v, k)?),
            "row_alpha" => s.row_alpha = Some(float_of(v, k)?),
            "palette" | "ui" | "diff" | "avatar" => {
                let t = v.as_table().ok_or_else(|| anyhow!("[{k}] must be a table"))?;
                for (ck, cv) in t {
                    s.colors.insert(format!("{k}.{ck}"), str_of(cv, ck)?);
                }
            }
            "syntax" => {
                let t = v.as_table().ok_or_else(|| anyhow!("[syntax] must be a table"))?;
                for (cap, cv) in t {
                    let spec = match cv {
                        toml::Value::String(fg) => SyntaxSpec { fg: Some(fg.clone()), ..SyntaxSpec::default() },
                        toml::Value::Table(st) => {
                            let flag = |n: &str| st.get(n).and_then(toml::Value::as_bool).unwrap_or(false);
                            let col = |n: &str| st.get(n).and_then(toml::Value::as_str).map(String::from);
                            SyntaxSpec { fg: col("fg"), bg: col("bg"), bold: flag("bold"), italic: flag("italic"), underline: flag("underline") }
                        }
                        _ => bail!("syntax `{cap}` must be a colour or a table"),
                    };
                    s.syntax.insert(cap.clone(), spec);
                }
            }
            _ => {}
        }
    }
    Ok(s)
}

/// Built-in and user themes by name. User themes (`~/.config/gitty/themes/*.toml`) replace a
/// built-in of the same name; such a theme may `inherit` the built-in it replaces.
pub struct Registry {
    builtin: HashMap<String, Spec>,
    user: HashMap<String, Spec>,
    errors: Vec<String>,
}

impl Registry {
    pub fn load(user_dir: Option<&Path>) -> Registry {
        let mut r = Registry { builtin: HashMap::new(), user: HashMap::new(), errors: Vec::new() };
        for (name, text) in BUILTIN_NAMES.iter().zip(BUILTINS) {
            match parse_spec(text, name) {
                Ok(s) => {
                    r.builtin.insert(name.to_string(), s);
                }
                Err(e) => r.errors.push(format!("built-in theme {name}: {e:#}")),
            }
        }
        let Some(dir) = user_dir else { return r };
        let Ok(entries) = std::fs::read_dir(dir) else { return r };
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
        paths.sort();
        for p in paths {
            let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            match std::fs::read_to_string(&p).map_err(anyhow::Error::from).and_then(|t| parse_spec(&t, &stem)) {
                Ok(s) => {
                    r.user.insert(s.name.clone(), s);
                }
                Err(e) => r.errors.push(format!("{}: {e:#}", p.display())),
            }
        }
        r
    }

    /// Built-ins in their fixed order, then user themes alphabetically.
    pub fn names(&self) -> Vec<String> {
        let mut out: Vec<String> = BUILTIN_NAMES.iter().map(|s| s.to_string()).collect();
        let mut user: Vec<&String> = self.user.keys().filter(|n| !self.builtin.contains_key(*n)).collect();
        user.sort();
        out.extend(user.into_iter().cloned());
        out
    }

    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    pub fn pick_auto(light_terminal: bool) -> &'static str {
        if light_terminal { "github-light" } else { "github-dark" }
    }

    /// Root-first inheritance chain of `name`.
    fn chain(&self, name: &str) -> anyhow::Result<Vec<&Spec>> {
        let (mut cur, mut is_user) = match (self.user.get(name), self.builtin.get(name)) {
            (Some(u), _) => (u, true),
            (None, Some(b)) => (b, false),
            (None, None) => bail!("unknown theme `{name}`"),
        };
        let mut out = vec![cur];
        let mut seen = vec![(cur.name.clone(), is_user)];
        while let Some(parent) = &cur.inherit {
            let next = if *parent == cur.name && is_user {
                self.builtin.get(parent).map(|b| (b, false))
            } else {
                self.user.get(parent).map(|u| (u, true)).or_else(|| self.builtin.get(parent).map(|b| (b, false)))
            };
            let (spec, user) = next.ok_or_else(|| anyhow!("theme `{}` inherits unknown theme `{parent}`", cur.name))?;
            if seen.contains(&(spec.name.clone(), user)) {
                let path: Vec<&str> = seen.iter().map(|(n, _)| n.as_str()).collect();
                bail!("theme inheritance cycle: {} → {parent}", path.join(" → "));
            }
            if out.len() >= MAX_INHERIT_DEPTH {
                bail!("theme `{name}` inherits more than {MAX_INHERIT_DEPTH} levels deep");
            }
            seen.push((spec.name.clone(), user));
            out.push(spec);
            (cur, is_user) = (spec, user);
        }
        out.reverse();
        Ok(out)
    }

    pub fn resolve(&self, name: &str, depth: ColorDepth, emph_alpha: Option<f32>) -> anyhow::Result<Theme> {
        let chain = self.chain(name)?;
        let mut merged = Spec { name: name.to_string(), ..Spec::default() };
        for s in &chain {
            merged.kind = s.kind.clone().or(merged.kind);
            merged.emph_alpha = s.emph_alpha.or(merged.emph_alpha);
            merged.row_alpha = s.row_alpha.or(merged.row_alpha);
            merged.colors.extend(s.colors.iter().map(|(k, v)| (k.clone(), v.clone())));
            merged.syntax.extend(s.syntax.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        build(&merged, depth, emph_alpha).with_context(|| format!("theme `{name}`"))
    }
}

struct Resolver<'a> {
    spec: &'a Spec,
    depth: ColorDepth,
}

impl Resolver<'_> {
    fn palette(&self, key: &str) -> anyhow::Result<Rgb> {
        let v = self.spec.colors.get(&format!("palette.{key}")).ok_or_else(|| anyhow!("missing palette colour `{key}`"))?;
        Rgb::parse(v).ok_or_else(|| anyhow!("palette `{key}`: `{v}` is not a #rrggbb colour"))
    }
    fn palette_or(&self, key: &str, fallback: &str) -> anyhow::Result<Rgb> {
        if self.spec.colors.contains_key(&format!("palette.{key}")) { self.palette(key) } else { self.palette(fallback) }
    }
    /// A colour value: `#hex` or a palette name.
    fn value(&self, what: &str, v: &str) -> anyhow::Result<Rgb> {
        Rgb::parse(v).map(Ok).unwrap_or_else(|| {
            if v.starts_with('#') { Err(anyhow!("{what}: `{v}` is not a colour")) } else { self.palette(v).with_context(|| format!("{what}: `{v}`")) }
        })
    }
    fn get(&self, section: &str, key: &str, default: impl FnOnce() -> anyhow::Result<Rgb>) -> anyhow::Result<Rgb> {
        match self.spec.colors.get(&format!("{section}.{key}")) {
            Some(v) => self.value(&format!("{section}.{key}"), v),
            None => default(),
        }
    }
    fn color(&self, c: Rgb) -> Color {
        match self.depth {
            ColorDepth::True => Color::Rgb(c.0, c.1, c.2),
            ColorDepth::Ansi256 => Color::Indexed(c.to_xterm256()),
        }
    }
}

fn build(spec: &Spec, depth: ColorDepth, emph_override: Option<f32>) -> anyhow::Result<Theme> {
    let r = Resolver { spec, depth };
    let p = |k: &str| r.palette(k);
    let (bg, fg, muted, accent) = (p("bg")?, p("fg")?, p("muted")?, p("accent")?);
    let (red, green, yellow, blue, magenta, cyan) = (p("red")?, p("green")?, p("yellow")?, p("blue")?, p("magenta")?, p("cyan")?);
    let border = p("border")?;
    let panel = r.palette_or("panel", "bg")?;
    let orange = r.palette_or("orange", "yellow")?;
    let is_light = match spec.kind.as_deref() {
        Some(k) => k == "light",
        None => bg.luma() > 0.5,
    };
    let row_alpha = spec.row_alpha.unwrap_or(if is_light { 0.12 } else { 0.15 });
    let emph_alpha = emph_override.or(spec.emph_alpha).unwrap_or(DEFAULT_EMPH_ALPHA);

    let ui_bg = r.get("ui", "bg", || Ok(bg))?;
    let ui_fg = r.get("ui", "fg", || Ok(fg))?;
    let ui_accent = r.get("ui", "accent", || Ok(accent))?;
    let ui_panel = r.get("ui", "panel", || Ok(panel))?;
    let tint = |c: Rgb, a: f32| c.blend(ui_bg, a);
    let u = |k: &str, d: Rgb| r.get("ui", k, || Ok(d));
    let ui = [
        u("border", border)?,
        u("border_focus", ui_accent)?,
        u("muted", muted)?,
        u("selection", tint(ui_accent, 0.22))?,
        u("selection_inactive", tint(ui_fg, 0.08))?,
        u("ahead", yellow)?,
        u("behind", cyan)?,
        u("badge_head_fg", ui_bg)?,
        u("badge_head_bg", ui_accent)?,
        u("badge_local_fg", green)?,
        u("badge_local_bg", tint(green, 0.18))?,
        u("badge_remote_fg", blue)?,
        u("badge_remote_bg", tint(blue, 0.18))?,
        u("badge_tag_fg", yellow)?,
        u("badge_tag_bg", tint(yellow, 0.18))?,
        u("status_bg", ui_panel)?,
        u("status_fg", muted)?,
        u("error", red)?,
        u("warning", yellow)?,
        u("status_added", green)?,
        u("status_modified", yellow)?,
        u("status_deleted", red)?,
        u("status_renamed", blue)?,
        u("pr_open", green)?,
        u("pr_draft", muted)?,
        u("pr_merged", magenta)?,
        u("pr_closed", red)?,
    ];
    let c = |x: Rgb| r.color(x);
    let lanes = lane_palette(&[ui_accent, green, yellow, blue, red, magenta, cyan, orange], &[ui_bg, ui_panel, ui[3], ui[4], ui_fg, ui[2]], c);
    let ui = UiColors {
        lanes,
        bg: c(ui_bg),
        panel: c(ui_panel),
        fg: c(ui_fg),
        accent: c(ui_accent),
        border: c(ui[0]),
        border_focus: c(ui[1]),
        muted: c(ui[2]),
        selection: c(ui[3]),
        selection_inactive: c(ui[4]),
        ahead: c(ui[5]),
        behind: c(ui[6]),
        badge_head_fg: c(ui[7]),
        badge_head_bg: c(ui[8]),
        badge_local_fg: c(ui[9]),
        badge_local_bg: c(ui[10]),
        badge_remote_fg: c(ui[11]),
        badge_remote_bg: c(ui[12]),
        badge_tag_fg: c(ui[13]),
        badge_tag_bg: c(ui[14]),
        status_bg: c(ui[15]),
        status_fg: c(ui[16]),
        error: c(ui[17]),
        warning: c(ui[18]),
        status_added: c(ui[19]),
        status_modified: c(ui[20]),
        status_deleted: c(ui[21]),
        status_renamed: c(ui[22]),
        pr_open: c(ui[23]),
        pr_draft: c(ui[24]),
        pr_merged: c(ui[25]),
        pr_closed: c(ui[26]),
    };

    let d = |k: &str, dflt: Rgb| r.get("diff", k, || Ok(dflt));
    let add_accent = d("add_accent", green)?;
    let del_accent = d("del_accent", red)?;
    let add_bg = d("add_bg", add_accent.blend(ui_bg, row_alpha))?;
    let del_bg = d("del_bg", del_accent.blend(ui_bg, row_alpha))?;
    let hunk_bg = d("hunk_bg", blue.blend(ui_bg, 0.12))?;
    let ui_muted = r.get("ui", "muted", || Ok(muted))?;
    let ours_accent = d("ours_accent", green)?;
    let theirs_accent = d("theirs_accent", blue)?;
    let strong = |a: Rgb, k: f32| a.blend(ui_bg, (row_alpha * k).min(1.0));
    let diff = DiffColors {
        add_bg: c(add_bg),
        del_bg: c(del_bg),
        add_gutter: c(d("add_gutter", add_accent.blend(ui_bg, (row_alpha * 2.0).min(1.0)))?),
        del_gutter: c(d("del_gutter", del_accent.blend(ui_bg, (row_alpha * 2.0).min(1.0)))?),
        add_emph: c(d("add_emph", add_accent.blend(add_bg, emph_alpha))?),
        del_emph: c(d("del_emph", del_accent.blend(del_bg, emph_alpha))?),
        add_fg: c(d("add_fg", ui_fg)?),
        del_fg: c(d("del_fg", ui_fg)?),
        context_fg: c(d("context_fg", ui_fg)?),
        lineno: c(d("lineno", ui_muted)?),
        lineno_add: c(d("lineno_add", add_accent.blend(ui_muted, 0.5))?),
        lineno_del: c(d("lineno_del", del_accent.blend(ui_muted, 0.5))?),
        hunk_bg: c(hunk_bg),
        hunk_fg: c(d("hunk_fg", ui_muted)?),
        expand_bg: c(d("expand_bg", hunk_bg)?),
        expand_fg: c(d("expand_fg", ui_accent)?),
        filler: c(d("filler", ui_fg.blend(ui_bg, 0.04))?),
        cursor: c(d("cursor", ui_accent.blend(ui_bg, 0.30))?),
        ours_bg: c(d("ours_bg", strong(ours_accent, 1.0))?),
        ours_current_bg: c(d("ours_current_bg", strong(ours_accent, 1.8))?),
        ours_head: c(d("ours_head", strong(ours_accent, 3.0))?),
        theirs_bg: c(d("theirs_bg", strong(theirs_accent, 1.0))?),
        theirs_current_bg: c(d("theirs_current_bg", strong(theirs_accent, 1.8))?),
        theirs_head: c(d("theirs_head", strong(theirs_accent, 3.0))?),
        base_bg: c(d("base_bg", ui_fg.blend(ui_bg, 0.04))?),
    };

    let defaults = [red, green, yellow, blue, magenta, cyan, orange, accent];
    let mut avatar = [Color::Reset; 8];
    for (i, slot) in avatar.iter_mut().enumerate() {
        *slot = c(r.get("avatar", &format!("c{i}"), || Ok(defaults[i]))?);
    }

    let mut syntax = HashMap::new();
    for (cap, s) in &spec.syntax {
        let mut style = Style::default();
        if let Some(v) = &s.fg {
            style = style.fg(c(r.value(&format!("syntax.{cap}"), v)?));
        }
        if let Some(v) = &s.bg {
            style = style.bg(c(r.value(&format!("syntax.{cap}"), v)?));
        }
        for (on, m) in [(s.bold, Modifier::BOLD), (s.italic, Modifier::ITALIC), (s.underline, Modifier::UNDERLINED)] {
            if on {
                style = style.add_modifier(m);
            }
        }
        syntax.insert(cap.clone(), style);
    }

    Ok(Theme { name: spec.name.clone(), is_light, ui, diff, avatar, syntax })
}
