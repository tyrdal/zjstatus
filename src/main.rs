use std::collections::BTreeMap;
use zellij_tile::prelude::actions::Action;
use zellij_tile::prelude::actions::SearchDirection;
use zellij_tile::prelude::*;

#[derive(Default)]
struct State {
    initialized: bool,
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

const TO_NORMAL: Action = Action::SwitchToMode(InputMode::Normal);

const PLUGIN_SESSION_MANAGER: &str = "session-manager";
const PLUGIN_CONFIGURATION: &str = "configuration";
const PLUGIN_MANAGER: &str = "plugin-manager";
const PLUGIN_ABOUT: &str = "zellij:about";

const KEY_PATTERNS_NO_SEPARATOR: &[&str] = &["HJKL", "hjkl", "←↓↑→", "←→", "↓↑", "[]"];

const DEFAULT_MAX_LENGTH: usize = 0;
const DEFAULT_OVERFLOW_STR: &str = "...";
const DEFAULT_PIPE_NAME: &str = "zjstatus_hints";
const DEFAULT_KEY_BG: &str = "#394260";
const DEFAULT_KEY_FG: &str = "#ffffff";
const DEFAULT_DESC_FG: &str = "#a0a9cb";
const DEFAULT_CAP_LEFT: &str = "\u{e0ba}"; // Powerline Extra: lower-right slant
const DEFAULT_CAP_RIGHT: &str = "\u{e0bc}"; // Powerline Extra: upper-left slant

type ActionLabel = (Action, &'static str);
type ActionSequenceLabel = (&'static [Action], &'static str);

const NORMAL_MODE_ACTIONS: &[ActionLabel] = &[
    (Action::SwitchToMode(InputMode::Pane), "pane"),
    (Action::SwitchToMode(InputMode::Tab), "tab"),
    (Action::SwitchToMode(InputMode::Resize), "resize"),
    (Action::SwitchToMode(InputMode::Move), "move"),
    (Action::SwitchToMode(InputMode::Scroll), "scroll"),
    (Action::SwitchToMode(InputMode::Search), "search"),
    (Action::SwitchToMode(InputMode::Session), "session"),
    (Action::Quit, "quit"),
];

const PANE_MODE_ACTION_SEQUENCES: &[ActionSequenceLabel] = &[
    (&[Action::NewPane(None, None, false), TO_NORMAL], "new"),
    (&[Action::CloseFocus, TO_NORMAL], "close"),
    (&[Action::ToggleFocusFullscreen, TO_NORMAL], "fullscreen"),
    (&[Action::ToggleFloatingPanes, TO_NORMAL], "float"),
    (&[Action::TogglePaneEmbedOrFloating, TO_NORMAL], "embed"),
    (
        &[
            Action::NewPane(Some(Direction::Right), None, false),
            TO_NORMAL,
        ],
        "split right",
    ),
    (
        &[
            Action::NewPane(Some(Direction::Down), None, false),
            TO_NORMAL,
        ],
        "split down",
    ),
];

const TAB_MODE_ACTION_SEQUENCES: &[ActionSequenceLabel] = &[
    (
        &[
            Action::NewTab(None, vec![], None, None, None, true),
            TO_NORMAL,
        ],
        "new",
    ),
    (&[Action::CloseTab, TO_NORMAL], "close"),
    (&[Action::BreakPane, TO_NORMAL], "break pane"),
    (&[Action::ToggleActiveSyncTab, TO_NORMAL], "sync"),
];

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
        subscribe(&[EventType::ModeUpdate, EventType::SessionUpdate]);
    }

    fn update(&mut self, event: Event) -> bool {
        let mut should_render = !self.initialized;
        if let Event::ModeUpdate(mode_info) = event {
            if self.mode_info != mode_info {
                should_render = true;
            }
            self.mode_info = mode_info;
            self.base_mode_is_locked = self.mode_info.base_mode == Some(InputMode::Locked);
        };
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

fn find_keys_for_actions(
    keymap: &[(KeyWithModifier, Vec<Action>)],
    target_actions: &[Action],
    exact_match: bool,
) -> Vec<KeyWithModifier> {
    keymap
        .iter()
        .filter_map(|(key, key_actions)| {
            if exact_match {
                let matching = key_actions
                    .iter()
                    .zip(target_actions)
                    .filter(|(a, b)| a.shallow_eq(b))
                    .count();
                if matching == key_actions.len() && matching == target_actions.len() {
                    Some(key.clone())
                } else {
                    None
                }
            } else if key_actions.iter().next() == target_actions.iter().next() {
                Some(key.clone())
            } else {
                None
            }
        })
        .collect()
}

fn find_keys_for_action_groups(
    keymap: &[(KeyWithModifier, Vec<Action>)],
    action_groups: &[&[Action]],
) -> Vec<KeyWithModifier> {
    action_groups
        .iter()
        .flat_map(|actions| find_keys_for_actions(keymap, actions, true))
        .collect()
}

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

fn get_select_key(keymap: &[(KeyWithModifier, Vec<Action>)]) -> Vec<KeyWithModifier> {
    let to_normal_keys = find_keys_for_actions(keymap, &[TO_NORMAL], true);
    if to_normal_keys.contains(&KeyWithModifier::new(BareKey::Enter)) {
        vec![KeyWithModifier::new(BareKey::Enter)]
    } else {
        to_normal_keys.into_iter().take(1).collect()
    }
}

fn add_hint(parts: &mut Vec<Hint>, keys: &[KeyWithModifier], description: &str) {
    if !keys.is_empty() {
        parts.push(Hint {
            keys: format_hint_keys(keys),
            description: description.to_string(),
        });
    }
}

fn render_hints_for_mode(mode: InputMode, keymap: &[(KeyWithModifier, Vec<Action>)]) -> Vec<Hint> {
    let mut parts = vec![];
    let select_keys = get_select_key(keymap);

    match mode {
        InputMode::Normal => {
            for (action, label) in NORMAL_MODE_ACTIONS {
                let keys = find_keys_for_actions(keymap, &[action.clone()], true);
                add_hint(&mut parts, &keys, label);
            }
        }
        InputMode::Pane => {
            for (actions, label) in PANE_MODE_ACTION_SEQUENCES {
                let keys = find_keys_for_actions(keymap, actions, false);
                if !keys.is_empty() {
                    add_hint(&mut parts, &keys, label);
                }
            }

            let rename_keys = find_keys_for_actions(
                keymap,
                &[
                    Action::SwitchToMode(InputMode::RenamePane),
                    Action::PaneNameInput(vec![0]),
                ],
                false,
            );
            if !rename_keys.is_empty() {
                add_hint(&mut parts, &rename_keys, "rename");
            }

            let focus_keys = find_keys_for_action_groups(
                keymap,
                &[
                    &[Action::MoveFocus(Direction::Left)],
                    &[Action::MoveFocus(Direction::Down)],
                    &[Action::MoveFocus(Direction::Up)],
                    &[Action::MoveFocus(Direction::Right)],
                ],
            );
            add_hint(&mut parts, &focus_keys, "move");
            add_hint(&mut parts, &select_keys, "select");
        }
        InputMode::Tab => {
            for (actions, label) in TAB_MODE_ACTION_SEQUENCES {
                let keys = find_keys_for_actions(keymap, actions, false);
                if !keys.is_empty() {
                    add_hint(&mut parts, &keys, label);
                }
            }

            let rename_keys = find_keys_for_actions(
                keymap,
                &[
                    Action::SwitchToMode(InputMode::RenameTab),
                    Action::TabNameInput(vec![0]),
                ],
                false,
            );
            if !rename_keys.is_empty() {
                add_hint(&mut parts, &rename_keys, "rename");
            }

            let focus_keys_full = find_keys_for_action_groups(
                keymap,
                &[&[Action::GoToPreviousTab], &[Action::GoToNextTab]],
            );
            let focus_keys = if focus_keys_full.contains(&KeyWithModifier::new(BareKey::Left))
                && focus_keys_full.contains(&KeyWithModifier::new(BareKey::Right))
            {
                vec![
                    KeyWithModifier::new(BareKey::Left),
                    KeyWithModifier::new(BareKey::Right),
                ]
            } else {
                focus_keys_full
            };
            add_hint(&mut parts, &focus_keys, "move");
            add_hint(&mut parts, &select_keys, "select");
        }
        InputMode::Resize => {
            let resize_keys = find_keys_for_action_groups(
                keymap,
                &[
                    &[Action::Resize(Resize::Increase, None)],
                    &[Action::Resize(Resize::Decrease, None)],
                ],
            );
            add_hint(&mut parts, &resize_keys, "resize");

            let increase_keys = find_keys_for_action_groups(
                keymap,
                &[
                    &[Action::Resize(Resize::Increase, Some(Direction::Left))],
                    &[Action::Resize(Resize::Increase, Some(Direction::Down))],
                    &[Action::Resize(Resize::Increase, Some(Direction::Up))],
                    &[Action::Resize(Resize::Increase, Some(Direction::Right))],
                ],
            );
            add_hint(&mut parts, &increase_keys, "increase");

            let decrease_keys = find_keys_for_action_groups(
                keymap,
                &[
                    &[Action::Resize(Resize::Decrease, Some(Direction::Left))],
                    &[Action::Resize(Resize::Decrease, Some(Direction::Down))],
                    &[Action::Resize(Resize::Decrease, Some(Direction::Up))],
                    &[Action::Resize(Resize::Decrease, Some(Direction::Right))],
                ],
            );
            add_hint(&mut parts, &decrease_keys, "decrease");
            add_hint(&mut parts, &select_keys, "select");
        }
        InputMode::Move => {
            let move_keys = find_keys_for_action_groups(
                keymap,
                &[
                    &[Action::MovePane(Some(Direction::Left))],
                    &[Action::MovePane(Some(Direction::Down))],
                    &[Action::MovePane(Some(Direction::Up))],
                    &[Action::MovePane(Some(Direction::Right))],
                ],
            );
            add_hint(&mut parts, &move_keys, "move");
            add_hint(&mut parts, &select_keys, "select");
        }
        InputMode::Scroll => {
            let search_keys = find_keys_for_actions(
                keymap,
                &[
                    Action::SwitchToMode(InputMode::EnterSearch),
                    Action::SearchInput(vec![0]),
                ],
                true,
            );
            add_hint(&mut parts, &search_keys, "search");

            let scroll_keys =
                find_keys_for_action_groups(keymap, &[&[Action::ScrollDown], &[Action::ScrollUp]]);
            add_hint(&mut parts, &scroll_keys, "scroll");

            let page_scroll_keys = find_keys_for_action_groups(
                keymap,
                &[&[Action::PageScrollDown], &[Action::PageScrollUp]],
            );
            add_hint(&mut parts, &page_scroll_keys, "page");

            let half_page_scroll_keys = find_keys_for_action_groups(
                keymap,
                &[&[Action::HalfPageScrollDown], &[Action::HalfPageScrollUp]],
            );
            add_hint(&mut parts, &half_page_scroll_keys, "half page");

            let edit_keys =
                find_keys_for_actions(keymap, &[Action::EditScrollback, TO_NORMAL], false);
            if !edit_keys.is_empty() {
                add_hint(&mut parts, &edit_keys, "edit");
            }
            add_hint(&mut parts, &select_keys, "select");
        }
        InputMode::Search => {
            let search_keys = find_keys_for_actions(
                keymap,
                &[
                    Action::SwitchToMode(InputMode::EnterSearch),
                    Action::SearchInput(vec![0]),
                ],
                true,
            );
            add_hint(&mut parts, &search_keys, "search");

            let scroll_keys =
                find_keys_for_action_groups(keymap, &[&[Action::ScrollDown], &[Action::ScrollUp]]);
            add_hint(&mut parts, &scroll_keys, "scroll");

            let page_scroll_keys = find_keys_for_action_groups(
                keymap,
                &[&[Action::PageScrollDown], &[Action::PageScrollUp]],
            );
            add_hint(&mut parts, &page_scroll_keys, "page");

            let half_page_scroll_keys = find_keys_for_action_groups(
                keymap,
                &[&[Action::HalfPageScrollDown], &[Action::HalfPageScrollUp]],
            );
            add_hint(&mut parts, &half_page_scroll_keys, "half page");

            let down_keys =
                find_keys_for_actions(keymap, &[Action::Search(SearchDirection::Down)], true);
            add_hint(&mut parts, &down_keys, "down");

            let up_keys =
                find_keys_for_actions(keymap, &[Action::Search(SearchDirection::Up)], true);
            add_hint(&mut parts, &up_keys, "up");

            add_hint(&mut parts, &select_keys, "select");
        }
        InputMode::Session => {
            let detach_keys = find_keys_for_actions(keymap, &[Action::Detach], true);
            add_hint(&mut parts, &detach_keys, "detach");

            if let Some(manager_key) = plugin_key(keymap, PLUGIN_SESSION_MANAGER) {
                add_hint(&mut parts, &[manager_key], "manager");
            }

            if let Some(config_key) = plugin_key(keymap, PLUGIN_CONFIGURATION) {
                add_hint(&mut parts, &[config_key], "config");
            }

            if let Some(plugin_key_val) = plugin_key(keymap, PLUGIN_MANAGER) {
                add_hint(&mut parts, &[plugin_key_val], "plugins");
            }

            if let Some(about_key) = plugin_key(keymap, PLUGIN_ABOUT) {
                add_hint(&mut parts, &[about_key], "about");
            }

            add_hint(&mut parts, &select_keys, "select");
        }
        _ => {
            let keys =
                find_keys_for_actions(keymap, &[Action::SwitchToMode(InputMode::Normal)], true);
            add_hint(&mut parts, &keys, "normal");
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
