use std::collections::BTreeMap;
use zellij_tile::prelude::actions::Action;
use zellij_tile::prelude::actions::SearchDirection;
use zellij_tile::prelude::*;

#[derive(Default)]
struct State {
    initialized: bool,
    /// Keybindings from the InitialKeybinds event; zellij 0.45 sends ModeUpdate
    /// without them to plugins that subscribe to InitialKeybinds.
    keybinds: KeybindsVec,
    /// Resends still to do, one per second (see `schedule_resends`).
    resends_left: u32,
    timer_running: bool,
    /// Number of tabs at the last TabUpdate, to notice new tabs.
    tab_count: usize,
    pipe_name: String,
    mode_info: ModeInfo,
    base_mode_is_locked: bool,
    max_length: usize,
    overflow_str: String,
    hide_in_base_mode: bool,
    style: HintStyle,
}

/// How one key segment is drawn. Colours are anything zjstatus understands in
/// `#[fg=...]`: `#rrggbb`, a colour name, or a `$alias` from the zjstatus config.
struct HintStyle {
    key_bg: String,
    key_fg: String,
    desc_fg: String,
    cap_left: String,
    cap_right: String,
}

impl Default for HintStyle {
    fn default() -> Self {
        Self {
            key_bg: DEFAULT_KEY_BG.to_string(),
            key_fg: DEFAULT_KEY_FG.to_string(),
            desc_fg: DEFAULT_DESC_FG.to_string(),
            cap_left: DEFAULT_CAP_LEFT.to_string(),
            cap_right: DEFAULT_CAP_RIGHT.to_string(),
        }
    }
}

/// One hint: the key text (e.g. "Ctrl + p" or "h|j|k|l") and what it does.
struct Hint {
    keys: String,
    description: String,
}

register_plugin!(State);

const PLUGIN_SESSION_MANAGER: &str = "session-manager";
const PLUGIN_CONFIGURATION: &str = "configuration";
const PLUGIN_MANAGER: &str = "plugin-manager";
const PLUGIN_ABOUT: &str = "zellij:about";

const KEY_PATTERNS_NO_SEPARATOR: &[&str] = &[
    "HJKL",
    "hjkl",
    "←↓↑→",
    "←→",
    "↓↑",
    "[]",
    "←↓↑→hjkl",
    "←→hl",
    "↓↑jk",
];

/// Display order for direction keys, so "move" reads ←↓↑→hjkl however the
/// bindings happen to be ordered in the config.
const DIRECTION_KEY_ORDER: &[&str] = &["←", "↓", "↑", "→", "h", "j", "k", "l"];

const DEFAULT_MAX_LENGTH: usize = 0;
const DEFAULT_OVERFLOW_STR: &str = "...";
const DEFAULT_PIPE_NAME: &str = "zjstatus_hints";
/// How many times (one per second) the hints are resent after loading.
const STARTUP_RESENDS: u32 = 10;
/// How many times (one per second) the hints are resent when a tab is opened.
const NEW_TAB_RESENDS: u32 = 5;
const DEFAULT_KEY_BG: &str = "#394260";
const DEFAULT_KEY_FG: &str = "#ffffff";
const DEFAULT_DESC_FG: &str = "#a0a9cb";
const DEFAULT_CAP_LEFT: &str = "\u{e0ba}"; // Powerline Extra: lower-right slant
const DEFAULT_CAP_RIGHT: &str = "\u{e0bc}"; // Powerline Extra: upper-left slant

fn get_common_modifiers(mut key_bindings: Vec<&KeyWithModifier>) -> Vec<KeyModifier> {
    if key_bindings.is_empty() {
        return vec![];
    }
    let mut common_modifiers = key_bindings.pop().unwrap().key_modifiers.clone();
    for key in key_bindings {
        common_modifiers = common_modifiers
            .intersection(&key.key_modifiers)
            .cloned()
            .collect();
    }
    common_modifiers.into_iter().collect()
}

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.initialized = false;

        // TODO: configuration validation
        self.max_length = configuration
            .get("max_length")
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_MAX_LENGTH);
        self.overflow_str = configuration
            .get("overflow_str")
            .cloned()
            .unwrap_or_else(|| DEFAULT_OVERFLOW_STR.to_string());
        self.pipe_name = configuration
            .get("pipe_name")
            .cloned()
            .unwrap_or_else(|| DEFAULT_PIPE_NAME.to_string());
        self.hide_in_base_mode = configuration
            .get("hide_in_base_mode")
            .map(|s| s.to_lowercase().parse::<bool>().unwrap_or(false))
            .unwrap_or(false);

        let get = |key: &str, default: &str| {
            configuration
                .get(key)
                .cloned()
                .unwrap_or_else(|| default.to_string())
        };
        self.style = HintStyle {
            key_bg: get("key_bg", DEFAULT_KEY_BG),
            key_fg: get("key_fg", DEFAULT_KEY_FG),
            desc_fg: get("desc_fg", DEFAULT_DESC_FG),
            cap_left: get("cap_left", DEFAULT_CAP_LEFT),
            cap_right: get("cap_right", DEFAULT_CAP_RIGHT),
        };

        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::MessageAndLaunchOtherPlugins,
        ]);

        set_selectable(false);
        subscribe(&[
            EventType::ModeUpdate,
            EventType::SessionUpdate,
            EventType::Timer,
            EventType::InitialKeybinds,
            EventType::TabUpdate,
        ]);
        // zjstatus may not be listening yet when the first hints are sent, and
        // nothing triggers a resend until the mode changes - so resend once a
        // second for the first few seconds.
        self.schedule_resends(STARTUP_RESENDS);
    }

    fn update(&mut self, event: Event) -> bool {
        let mut should_render = !self.initialized;
        match event {
            Event::Timer(_) => {
                self.timer_running = false;
                if self.resends_left > 0 {
                    self.resends_left -= 1;
                    should_render = true;
                    if self.resends_left > 0 {
                        set_timeout(1.0);
                        self.timer_running = true;
                    }
                }
            }
            // A new tab brings a new zjstatus instance with no hints yet, and it
            // may still be starting - resend now and for a few more seconds.
            Event::TabUpdate(tabs) => {
                if tabs.len() != self.tab_count {
                    if tabs.len() > self.tab_count && self.tab_count > 0 {
                        self.schedule_resends(NEW_TAB_RESENDS);
                        should_render = true;
                    }
                    self.tab_count = tabs.len();
                }
            }
            // Sent right after subscribing, so the hints work before the first
            // mode change - a background plugin gets no ModeUpdate until then.
            Event::InitialKeybinds(keybinds) => {
                self.keybinds = keybinds;
                self.mode_info.keybinds = self.keybinds.clone();
                should_render = true;
            }
            Event::ModeUpdate(mut mode_info) => {
                if mode_info.keybinds.is_empty() {
                    mode_info.keybinds = self.keybinds.clone();
                } else {
                    self.keybinds = mode_info.keybinds.clone();
                }
                if self.mode_info != mode_info {
                    should_render = true;
                }
                self.mode_info = mode_info;
                self.base_mode_is_locked = self.mode_info.base_mode == Some(InputMode::Locked);
            }
            _ => {}
        }
        should_render
    }

    fn render(&mut self, _rows: usize, _cols: usize) {
        let mode_info = &self.mode_info;
        let hints = if !(self.hide_in_base_mode && Some(mode_info.mode) == mode_info.base_mode) {
            let keymap = get_keymap_for_mode(mode_info);
            fit_hints(
                render_hints_for_mode(mode_info.mode, &keymap),
                self.max_length,
            )
        } else {
            vec![]
        };
        let truncated = hints.len() < self.hint_count();
        let mut markup = hints_to_markup(&hints, &self.style);
        let mut plain = hints_to_plain(&hints);
        if truncated {
            markup.push_str(&format!(
                "#[fg={}]{}",
                self.style.desc_fg, self.overflow_str
            ));
            plain.push_str(&self.overflow_str);
        }

        // HACK: Because we're not sure when zjstatus will be ready to receive messages,
        // we'll repeatedly send messages until the user has switched to a different mode,
        // at which point we'll assume that zjstatus has been initialized. The render function
        // does not seem to be called too frequently, so this should be fine.
        if !markup.is_empty() && Some(mode_info.mode) != mode_info.base_mode {
            self.initialized = true;
        }

        pipe_message_to_plugin(MessageToPlugin::new("pipe").with_payload(format!(
            "zjstatus::pipe::pipe_{}::{}",
            self.pipe_name, markup
        )));
        print!("{}", plain);
    }
}

impl State {
    /// Resend the hints once a second, `count` times (restarts any countdown).
    fn schedule_resends(&mut self, count: u32) {
        self.resends_left = self.resends_left.max(count);
        if !self.timer_running {
            set_timeout(1.0);
            self.timer_running = true;
        }
    }

    /// Number of hints for the current mode before any truncation.
    fn hint_count(&self) -> usize {
        if self.hide_in_base_mode && Some(self.mode_info.mode) == self.mode_info.base_mode {
            return 0;
        }
        let keymap = get_keymap_for_mode(&self.mode_info);
        render_hints_for_mode(self.mode_info.mode, &keymap).len()
    }
}

/// Visible width of one hint as drawn: " " cap " keys " cap " description".
fn hint_width(hint: &Hint) -> usize {
    1 + 1 + hint.keys.chars().count() + 2 + 1 + 1 + hint.description.chars().count()
}

/// Keep whole hints while they fit in `max_length` columns (0 = no limit).
fn fit_hints(hints: Vec<Hint>, max_length: usize) -> Vec<Hint> {
    if max_length == 0 {
        return hints;
    }
    let mut used = 0;
    hints
        .into_iter()
        .take_while(|hint| {
            used += hint_width(hint);
            used <= max_length
        })
        .collect()
}

/// zjstatus markup for the pipe; render it with `pipe_<name>_rendermode "dynamic"`.
fn hints_to_markup(hints: &[Hint], style: &HintStyle) -> String {
    hints
        .iter()
        .map(|hint| {
            format!(
                " #[fg={bg}]{cl}#[bg={bg},fg={fg},bold] {keys} #[fg={bg}]{cr}#[fg={desc}] {d}",
                bg = style.key_bg,
                fg = style.key_fg,
                desc = style.desc_fg,
                cl = style.cap_left,
                cr = style.cap_right,
                keys = hint.keys,
                d = hint.description,
            )
        })
        .collect()
}

/// Plain text for the plugin's own (normally hidden) pane.
fn hints_to_plain(hints: &[Hint]) -> String {
    hints
        .iter()
        .map(|hint| format!(" <{}> {}", hint.keys, hint.description))
        .collect()
}

type Keymap = [(KeyWithModifier, Vec<Action>)];

/// Keys whose list of bound actions satisfies `matches`.
fn keys_where(keymap: &Keymap, matches: impl Fn(&[Action]) -> bool) -> Vec<KeyWithModifier> {
    keymap
        .iter()
        .filter(|(_, actions)| matches(actions))
        .map(|(key, _)| key.clone())
        .collect()
}

/// Keys bound to exactly one action, which satisfies `pred`.
fn keys_for(keymap: &Keymap, pred: impl Fn(&Action) -> bool) -> Vec<KeyWithModifier> {
    keys_where(keymap, |actions| actions.len() == 1 && pred(&actions[0]))
}

/// Keys whose first action satisfies `pred`, e.g. `NewPane; SwitchToMode "normal"`.
fn keys_starting_with(keymap: &Keymap, pred: impl Fn(&Action) -> bool) -> Vec<KeyWithModifier> {
    keys_where(keymap, |actions| actions.first().is_some_and(&pred))
}

fn is_switch_to(action: &Action, mode: InputMode) -> bool {
    matches!(action, Action::SwitchToMode { input_mode } if *input_mode == mode)
}

const DIRECTIONS: [Direction; 4] = [
    Direction::Left,
    Direction::Down,
    Direction::Up,
    Direction::Right,
];

fn format_modifier_string(modifiers: &[KeyModifier]) -> String {
    if modifiers.is_empty() {
        String::new()
    } else {
        modifiers
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join("-")
    }
}

fn format_key_display(
    key_bindings: &[KeyWithModifier],
    common_modifiers: &[KeyModifier],
) -> Vec<String> {
    key_bindings
        .iter()
        .map(|key| {
            if common_modifiers.is_empty() {
                format!("{}", key)
            } else {
                let unique_modifiers = key
                    .key_modifiers
                    .iter()
                    .filter(|m| !common_modifiers.contains(m))
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                if unique_modifiers.is_empty() {
                    format!("{}", key.bare_key)
                } else {
                    format!("{} {}", unique_modifiers, key.bare_key)
                }
            }
        })
        .collect()
}

fn get_key_separator(key_display: &[String]) -> &'static str {
    let key_string = key_display.join("");
    if KEY_PATTERNS_NO_SEPARATOR.contains(&&key_string[..]) {
        ""
    } else {
        "|"
    }
}

/// "Ctrl + p", "h|j|k|l", "Alt + ←↓↑→": shared modifiers once, then the keys.
fn format_hint_keys(key_bindings: &[KeyWithModifier]) -> String {
    let common_modifiers = get_common_modifiers(key_bindings.iter().collect());
    let modifier_str = format_modifier_string(&common_modifiers);
    let key_display = format_key_display(key_bindings, &common_modifiers);
    let keys = key_display.join(get_key_separator(&key_display));
    if modifier_str.is_empty() {
        keys
    } else {
        format!("{} + {}", modifier_str, keys)
    }
}

fn plugin_key(
    keymap: &[(KeyWithModifier, Vec<Action>)],
    plugin_name: &str,
) -> Option<KeyWithModifier> {
    keymap.iter().find_map(|(key, key_actions)| {
        if key_actions
            .iter()
            .any(|action| action.launches_plugin(plugin_name))
        {
            Some(key.clone())
        } else {
            None
        }
    })
}

fn get_select_key(keymap: &Keymap) -> Vec<KeyWithModifier> {
    let to_normal_keys = keys_for(keymap, |a| is_switch_to(a, InputMode::Normal));
    if to_normal_keys.contains(&KeyWithModifier::new(BareKey::Enter)) {
        vec![KeyWithModifier::new(BareKey::Enter)]
    } else {
        to_normal_keys.into_iter().take(1).collect()
    }
}

fn add_hint(parts: &mut Vec<Hint>, keys: &[KeyWithModifier], description: &str) {
    let keys = compact_keys(keys);
    if !keys.is_empty() {
        parts.push(Hint {
            keys: format_hint_keys(&keys),
            description: description.to_string(),
        });
    }
}

/// "1-9 go to" instead of "1|2|3|4|5|6|7|8|9 go to": plain digit keys are
/// shown as a range, anything else falls back to the normal hint.
fn add_digit_range_hint(parts: &mut Vec<Hint>, keys: &[KeyWithModifier], description: &str) {
    let mut digits: Vec<u32> = keys
        .iter()
        .filter(|k| k.key_modifiers.is_empty())
        .filter_map(|k| match k.bare_key {
            BareKey::Char(c) => c.to_digit(10),
            _ => None,
        })
        .collect();
    digits.sort_unstable();
    digits.dedup();
    let contiguous = digits.windows(2).all(|w| w[1] == w[0] + 1);
    match (digits.first(), digits.last()) {
        (Some(first), Some(last)) if digits.len() > 2 && contiguous => parts.push(Hint {
            keys: format!("{}-{}", first, last),
            description: description.to_string(),
        }),
        _ => add_hint(parts, keys, description),
    }
}

/// Keep a hint short: when an action has plain keys (no modifier), show only
/// those - in a mode like "pane", `Alt n` next to `n` is just noise. Direction
/// keys are put in ←↓↑→hjkl order so they collapse into one compact group.
fn compact_keys(keys: &[KeyWithModifier]) -> Vec<KeyWithModifier> {
    let plain: Vec<KeyWithModifier> = keys
        .iter()
        .filter(|k| k.key_modifiers.is_empty())
        .cloned()
        .collect();
    let mut keys = if plain.is_empty() {
        keys.to_vec()
    } else {
        plain
    };
    let rank = |k: &KeyWithModifier| {
        DIRECTION_KEY_ORDER
            .iter()
            .position(|d| *d == k.bare_key.to_string())
            .unwrap_or(DIRECTION_KEY_ORDER.len())
    };
    keys.sort_by_key(rank); // stable: non-direction keys keep their order
    keys.dedup();
    keys
}

fn render_hints_for_mode(mode: InputMode, keymap: &Keymap) -> Vec<Hint> {
    let mut parts = vec![];
    let select_keys = get_select_key(keymap);
    let hint = |parts: &mut Vec<Hint>, keys: Vec<KeyWithModifier>, label: &str| {
        add_hint(parts, &keys, label)
    };

    match mode {
        InputMode::Normal => {
            for (target, label) in [
                (InputMode::Pane, "pane"),
                (InputMode::Tab, "tab"),
                (InputMode::Resize, "resize"),
                (InputMode::Move, "move"),
                (InputMode::Scroll, "scroll"),
                (InputMode::Search, "search"),
                (InputMode::Session, "session"),
            ] {
                hint(
                    &mut parts,
                    keys_for(keymap, |a| is_switch_to(a, target)),
                    label,
                );
            }
            hint(
                &mut parts,
                keys_for(keymap, |a| matches!(a, Action::Quit)),
                "quit",
            );
        }
        InputMode::Pane => {
            let new_pane = |dir: Option<Direction>| {
                keys_starting_with(
                    keymap,
                    move |a| matches!(a, Action::NewPane { direction, .. } if *direction == dir),
                )
            };
            hint(&mut parts, new_pane(None), "new");
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::CloseFocus)),
                "close",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::ToggleFocusFullscreen)),
                "fullscreen",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::ToggleFloatingPanes)),
                "float",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::TogglePaneEmbedOrFloating)),
                "embed",
            );
            hint(&mut parts, new_pane(Some(Direction::Right)), "split right");
            hint(&mut parts, new_pane(Some(Direction::Down)), "split down");
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| is_switch_to(a, InputMode::RenamePane)),
                "rename",
            );
            let focus_keys = DIRECTIONS
                .iter()
                .flat_map(|d| {
                    keys_for(
                        keymap,
                        |a| matches!(a, Action::MoveFocus { direction } if direction == d),
                    )
                })
                .collect();
            hint(&mut parts, focus_keys, "move");
            hint(&mut parts, select_keys, "select");
        }
        InputMode::Tab => {
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::NewTab { .. })),
                "new",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::CloseTab)),
                "close",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::BreakPane)),
                "break pane",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::ToggleActiveSyncTab)),
                "sync",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| is_switch_to(a, InputMode::RenameTab)),
                "rename",
            );
            let mut focus_keys = keys_for(keymap, |a| matches!(a, Action::GoToPreviousTab));
            focus_keys.extend(keys_for(keymap, |a| matches!(a, Action::GoToNextTab)));
            let left = KeyWithModifier::new(BareKey::Left);
            let right = KeyWithModifier::new(BareKey::Right);
            if focus_keys.contains(&left) && focus_keys.contains(&right) {
                focus_keys = vec![left, right];
            }
            hint(&mut parts, focus_keys, "move");
            add_digit_range_hint(
                &mut parts,
                &keys_starting_with(keymap, |a| matches!(a, Action::GoToTab { .. })),
                "go to",
            );
            let mut break_keys = keys_starting_with(keymap, |a| matches!(a, Action::BreakPaneLeft));
            break_keys.extend(keys_starting_with(keymap, |a| matches!(a, Action::BreakPaneRight)));
            hint(&mut parts, break_keys, "break ←→");
            hint(
                &mut parts,
                keys_for(keymap, |a| matches!(a, Action::ToggleTab)),
                "last",
            );
            hint(&mut parts, select_keys, "select");
        }
        InputMode::Resize => {
            let resize = |r: Resize, dir: Option<Direction>| {
                keys_for(
                    keymap,
                    move |a| matches!(a, Action::Resize { resize, direction } if *resize == r && *direction == dir),
                )
            };
            let mut both = resize(Resize::Increase, None);
            both.extend(resize(Resize::Decrease, None));
            hint(&mut parts, both, "resize");
            for (r, label) in [
                (Resize::Increase, "increase"),
                (Resize::Decrease, "decrease"),
            ] {
                let keys = DIRECTIONS
                    .iter()
                    .flat_map(|d| resize(r, Some(*d)))
                    .collect();
                hint(&mut parts, keys, label);
            }
            hint(&mut parts, select_keys, "select");
        }
        InputMode::Move => {
            let keys = DIRECTIONS
                .iter()
                .flat_map(|d| {
                    keys_for(
                        keymap,
                        |a| matches!(a, Action::MovePane { direction: Some(x) } if x == d),
                    )
                })
                .collect();
            hint(&mut parts, keys, "move");
            hint(&mut parts, select_keys, "select");
        }
        InputMode::Scroll | InputMode::Search => {
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| is_switch_to(a, InputMode::EnterSearch)),
                "search",
            );
            let pair = |a1: fn(&Action) -> bool, a2: fn(&Action) -> bool| {
                let mut keys = keys_for(keymap, a1);
                keys.extend(keys_for(keymap, a2));
                keys
            };
            hint(
                &mut parts,
                pair(
                    |a| matches!(a, Action::ScrollDown),
                    |a| matches!(a, Action::ScrollUp),
                ),
                "scroll",
            );
            hint(
                &mut parts,
                pair(
                    |a| matches!(a, Action::PageScrollDown),
                    |a| matches!(a, Action::PageScrollUp),
                ),
                "page",
            );
            hint(
                &mut parts,
                pair(
                    |a| matches!(a, Action::HalfPageScrollDown),
                    |a| matches!(a, Action::HalfPageScrollUp),
                ),
                "half page",
            );
            if mode == InputMode::Scroll {
                hint(
                    &mut parts,
                    keys_starting_with(keymap, |a| matches!(a, Action::EditScrollback { .. })),
                    "edit",
                );
            } else {
                for (dir, label) in [(SearchDirection::Down, "down"), (SearchDirection::Up, "up")] {
                    let keys = keys_for(
                        keymap,
                        move |a| matches!(a, Action::Search { direction, .. } if *direction == dir),
                    );
                    hint(&mut parts, keys, label);
                }
            }
            hint(&mut parts, select_keys, "select");
        }
        InputMode::Session => {
            hint(
                &mut parts,
                keys_for(keymap, |a| matches!(a, Action::Detach)),
                "detach",
            );
            for (plugin, label) in [
                (PLUGIN_SESSION_MANAGER, "manager"),
                (PLUGIN_CONFIGURATION, "config"),
                (PLUGIN_MANAGER, "plugins"),
                (PLUGIN_ABOUT, "about"),
            ] {
                if let Some(key) = plugin_key(keymap, plugin) {
                    hint(&mut parts, vec![key], label);
                }
            }
            hint(&mut parts, select_keys, "select");
        }
        InputMode::Tmux => {
            // Most useful first: max_length drops hints from the end.
            let new_pane = |dir: Direction| {
                keys_starting_with(
                    keymap,
                    move |a| matches!(a, Action::NewPane { direction, .. } if *direction == Some(dir)),
                )
            };
            hint(&mut parts, new_pane(Direction::Down), "split ↓");
            hint(&mut parts, new_pane(Direction::Right), "split →");
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::FocusLastPane)),
                "last",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::ToggleFocusFullscreen)),
                "zoom",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::NewTab { .. })),
                "new tab",
            );
            let mut tab_keys = keys_starting_with(keymap, |a| matches!(a, Action::GoToPreviousTab));
            tab_keys.extend(keys_starting_with(keymap, |a| matches!(a, Action::GoToNextTab)));
            hint(&mut parts, tab_keys, "tabs");
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| is_switch_to(a, InputMode::Scroll)),
                "copy",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| is_switch_to(a, InputMode::RenameTab)),
                "name tab",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| is_switch_to(a, InputMode::RenamePane)),
                "name pane",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| {
                    matches!(a, Action::Run { command, .. }
                        if command.args.iter().any(|arg| arg.contains("rename-session")))
                }),
                "name session",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::FocusNextPane)),
                "cycle",
            );
            hint(
                &mut parts,
                keys_starting_with(keymap, |a| matches!(a, Action::NextSwapLayout)),
                "layout",
            );
        }
        _ => {
            let keys = keys_for(keymap, |a| is_switch_to(a, InputMode::Normal));
            hint(&mut parts, keys, "normal");
        }
    }

    parts
}

fn get_keymap_for_mode(mode_info: &ModeInfo) -> Vec<(KeyWithModifier, Vec<Action>)> {
    match mode_info.mode {
        InputMode::Normal => mode_info.get_keybinds_for_mode(InputMode::Normal),
        InputMode::Pane => mode_info.get_keybinds_for_mode(InputMode::Pane),
        InputMode::Tab => mode_info.get_keybinds_for_mode(InputMode::Tab),
        InputMode::Resize => mode_info.get_keybinds_for_mode(InputMode::Resize),
        InputMode::Move => mode_info.get_keybinds_for_mode(InputMode::Move),
        InputMode::Scroll => mode_info.get_keybinds_for_mode(InputMode::Scroll),
        InputMode::Search => mode_info.get_keybinds_for_mode(InputMode::Search),
        InputMode::Session => mode_info.get_keybinds_for_mode(InputMode::Session),
        _ => mode_info.get_mode_keybinds(),
    }
}
