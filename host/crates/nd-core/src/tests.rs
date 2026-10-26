// SPDX-License-Identifier: GPL-3.0-or-later
//! Unit tests for nd-core.

use crate::error::CoreError;
use crate::mock::{MockInputInjector, MockLauncher};
use crate::model::{
    Action, ActionDefinition, Control, ControlStateVisual, Page, Profile, TriggerKind, CURRENT_SCHEMA_VERSION,
};
use crate::router::{ActionOutcome, ActionRouter, InputContext, NavigationTarget};
use crate::session::{DeckSession, InputReaction};
use nd_platform::{Key, KeyCombo};
use nd_proto::v1::{InputEvent, InputKind};
use std::sync::Arc;

fn make_test_router() -> (ActionRouter, MockInputInjector, MockLauncher) {
    let injector = MockInputInjector::new();
    let launcher = MockLauncher::new();
    let router = ActionRouter::new(Arc::new(injector.clone()), Arc::new(launcher.clone()));
    (router, injector, launcher)
}

#[test]
fn test_profile_serialization_roundtrip() {
    let mut profile = Profile::new("test_profile", "Test Profile");
    profile.cols = 6;
    profile.rows = 4;
    profile.app_bindings = vec!["firefox".to_string(), "code".to_string()];

    let mut page1 = Page::new("page1", "Home");
    let key1 = Control::key("k1", 0, 0, "Mute")
        .with_action(Action::hotkey_with_mods(vec![Key::Ctrl, Key::Alt], Key::M))
        .with_trigger_action(TriggerKind::LongPress, Action::type_text("Long pressed mute"));
    page1.add_control(key1);

    let slider1 = Control::slider("s1", 1, 0, 1, 3, false);
    page1.add_control(slider1);

    let mut toggle_btn = Control::key("t1", 2, 0, "Mic On");
    toggle_btn.states = vec![
        ControlStateVisual {
            title: Some("Mic On".to_string()),
            subtitle: None,
            icon_hash: Some("hash_on".to_string()),
            fg_argb: None,
            bg_argb: Some(0xFF00FF00),
        },
        ControlStateVisual {
            title: Some("Mic Muted".to_string()),
            subtitle: None,
            icon_hash: Some("hash_muted".to_string()),
            fg_argb: None,
            bg_argb: Some(0xFFFF0000),
        },
    ];
    toggle_btn.actions.push(ActionDefinition::new(Action::ToggleState));
    page1.add_control(toggle_btn);

    profile.add_page(page1);

    let mut page2 = Page::new("page2", "Media");
    let launch_btn =
        Control::key("k2", 0, 0, "Spotify").with_action(Action::launch_app("spotify", vec!["--minimized".to_string()]));
    page2.add_control(launch_btn);
    profile.add_page(page2);

    let json = profile.to_json().expect("serialize profile");
    let deserialized = Profile::from_json(&json).expect("deserialize profile");

    assert_eq!(profile, deserialized);
    assert_eq!(deserialized.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(deserialized.cols, 6);
    assert_eq!(deserialized.rows, 4);
    assert_eq!(deserialized.pages.len(), 2);
    assert_eq!(deserialized.pages[0].controls.len(), 3);
}

#[test]
fn test_profile_json_parsing_human_formats() {
    let json = r#"{
        "id": "my_deck",
        "name": "My Stream Deck",
        "pages": [
            {
                "id": "home",
                "name": "Main",
                "controls": [
                    {
                        "id": "btn_url",
                        "kind": "key",
                        "col": 0,
                        "row": 0,
                        "title": "Docs",
                        "action": {
                            "type": "open_url",
                            "url": "https://notideck.dev"
                        }
                    },
                    {
                        "id": "btn_hotkey_flat",
                        "kind": "key",
                        "col": 1,
                        "row": 0,
                        "title": "Copy",
                        "action": {
                            "type": "hotkey",
                            "key": "c",
                            "modifiers": ["ctrl"]
                        }
                    },
                    {
                        "id": "btn_hotkey_combo",
                        "kind": "key",
                        "col": 2,
                        "row": 0,
                        "title": "Paste",
                        "action": {
                            "type": "hotkey",
                            "combo": {
                                "modifiers": ["ctrl"],
                                "key": "v"
                            }
                        }
                    }
                ]
            }
        ]
    }"#;

    let profile = Profile::from_json(json).expect("parse human profile json");
    assert_eq!(profile.id, "my_deck");
    assert_eq!(profile.cols, 5); // default
    assert_eq!(profile.rows, 3); // default
    assert_eq!(profile.schema_version, 1); // default

    let page = profile.get_page("home").expect("page found");
    assert_eq!(page.controls.len(), 3);

    // Check OpenUrl action
    let btn_url = page.get_control("btn_url").expect("btn_url found");
    assert_eq!(btn_url.actions[0].action, Action::OpenUrl { url: "https://notideck.dev".to_string() });
    assert_eq!(btn_url.actions[0].trigger, TriggerKind::KeyDown);

    // Check flat hotkey
    let btn_flat = page.get_control("btn_hotkey_flat").expect("btn_flat found");
    assert_eq!(
        btn_flat.actions[0].action,
        Action::Hotkey { combo: KeyCombo { modifiers: vec![Key::Ctrl], key: Key::C } }
    );

    // Check combo hotkey
    let btn_combo = page.get_control("btn_hotkey_combo").expect("btn_combo found");
    assert_eq!(
        btn_combo.actions[0].action,
        Action::Hotkey { combo: KeyCombo { modifiers: vec![Key::Ctrl], key: Key::V } }
    );
}

#[test]
fn test_profile_schema_migration_error_for_newer_version() {
    let json = r#"{
        "schema_version": 999,
        "id": "future",
        "name": "Future Profile",
        "pages": []
    }"#;

    let result = Profile::from_json(json);
    assert!(result.is_err());
    match result.unwrap_err() {
        CoreError::MigrationError { from, to, .. } => {
            assert_eq!(from, 999);
            assert_eq!(to, CURRENT_SCHEMA_VERSION);
        }
        err => panic!("unexpected error: {:?}", err),
    }
}

#[test]
fn test_profile_page_and_control_management() {
    let mut profile = Profile::new("prof", "Profile");
    assert!(profile.initial_page().is_none());

    let page1 = Page::new("p1", "Page 1");
    profile.add_page(page1);
    assert_eq!(profile.initial_page_id.as_deref(), Some("p1"));
    assert_eq!(profile.initial_page().map(|p| p.id.as_str()), Some("p1"));

    let page2 = Page::new("p2", "Page 2");
    profile.add_page(page2);

    assert_eq!(profile.pages.len(), 2);
    assert!(profile.get_page("p1").is_some());
    assert!(profile.get_page("p2").is_some());
    assert!(profile.get_page("p3").is_none());

    // Add and remove controls
    let p1 = profile.get_page_mut("p1").unwrap();
    let c1 = Control::key("c1", 0, 0, "Button 1");
    p1.add_control(c1);
    assert!(p1.get_control("c1").is_some());

    let removed_ctrl = p1.remove_control("c1");
    assert!(removed_ctrl.is_some());
    assert!(p1.get_control("c1").is_none());

    // Remove page
    let removed_page = profile.remove_page("p1");
    assert!(removed_page.is_some());
    assert_eq!(profile.initial_page_id.as_deref(), Some("p2"));
    assert_eq!(profile.pages.len(), 1);
}

#[test]
fn test_proto_translation_set_layout() {
    let mut page = Page::new("test_page", "Test Layout");
    page.cols = Some(4);
    page.rows = Some(2);

    let k1 = Control::key("k1", 0, 0, "Title 1");
    let mut s1 = Control::slider("s1", 1, 0, 2, 1, true);
    s1.value = 0.85;

    page.add_control(k1);
    page.add_control(s1);

    let set_layout = page.to_proto(5, 3);
    assert_eq!(set_layout.page_id, "test_page");
    assert_eq!(set_layout.cols, 4);
    assert_eq!(set_layout.rows, 2);
    assert_eq!(set_layout.controls.len(), 2);

    assert_eq!(set_layout.controls[0].id, "k1");
    assert_eq!(set_layout.controls[0].kind, nd_proto::v1::ControlKind::Key as i32);
    assert_eq!(set_layout.controls[0].title, "Title 1");

    assert_eq!(set_layout.controls[1].id, "s1");
    assert_eq!(set_layout.controls[1].kind, nd_proto::v1::ControlKind::Slider as i32);
    assert_eq!(set_layout.controls[1].span_w, 2);
    assert_eq!(set_layout.controls[1].value, 0.85);
    assert!(set_layout.controls[1].horizontal);
}

#[test]
fn test_control_multi_state_visuals() {
    let mut ctrl = Control::key("toggle", 0, 0, "Default Title");
    ctrl.states = vec![
        ControlStateVisual {
            title: Some("State 0 Title".to_string()),
            subtitle: Some("State 0 Subtitle".to_string()),
            icon_hash: Some("icon0".to_string()),
            fg_argb: Some(0xFF111111),
            bg_argb: Some(0xFF222222),
        },
        ControlStateVisual {
            title: Some("State 1 Title".to_string()),
            subtitle: Some("State 1 Subtitle".to_string()),
            icon_hash: Some("icon1".to_string()),
            fg_argb: Some(0xFF333333),
            bg_argb: Some(0xFF444444),
        },
    ];

    ctrl.state_index = 0;
    assert_eq!(ctrl.effective_title(), "State 0 Title");
    assert_eq!(ctrl.effective_subtitle(), "State 0 Subtitle");
    assert_eq!(ctrl.effective_icon_hash(), "icon0");
    assert_eq!(ctrl.effective_fg_argb(), 0xFF111111);
    assert_eq!(ctrl.effective_bg_argb(), 0xFF222222);

    let proto0 = ctrl.to_proto();
    assert_eq!(proto0.title, "State 0 Title");
    assert_eq!(proto0.bg_argb, 0xFF222222);

    ctrl.state_index = 1;
    assert_eq!(ctrl.effective_title(), "State 1 Title");
    assert_eq!(ctrl.effective_subtitle(), "State 1 Subtitle");
    assert_eq!(ctrl.effective_icon_hash(), "icon1");
    assert_eq!(ctrl.effective_fg_argb(), 0xFF333333);
    assert_eq!(ctrl.effective_bg_argb(), 0xFF444444);

    let proto1 = ctrl.to_proto();
    assert_eq!(proto1.title, "State 1 Title");
    assert_eq!(proto1.bg_argb, 0xFF444444);
}

#[test]
fn test_action_router_actions() {
    let (router, injector, launcher) = make_test_router();

    // 1. Hotkey
    let combo = KeyCombo { modifiers: vec![Key::Ctrl, Key::Shift], key: Key::Escape };
    let outcome =
        router.execute(&Action::Hotkey { combo: combo.clone() }, &InputContext::default()).expect("execute hotkey");
    assert_eq!(outcome, ActionOutcome::Executed);
    assert_eq!(injector.last_combo(), Some(combo));

    // 2. TypeText
    let outcome =
        router.execute(&Action::type_text("Hello Notideck!"), &InputContext::default()).expect("execute type_text");
    assert_eq!(outcome, ActionOutcome::Executed);
    assert_eq!(injector.last_text(), Some("Hello Notideck!".to_string()));

    // 3. OpenUrl
    let outcome =
        router.execute(&Action::open_url("https://github.com"), &InputContext::default()).expect("execute open_url");
    assert_eq!(outcome, ActionOutcome::Executed);
    assert_eq!(launcher.last_url(), Some("https://github.com".to_string()));

    // 4. LaunchApp
    let outcome = router
        .execute(&Action::launch_app_confirmed("code", vec![".".to_string()]), &InputContext::default())
        .expect("execute launch_app");
    assert_eq!(outcome, ActionOutcome::Executed);
    assert_eq!(launcher.last_launch(), Some(("code".to_string(), vec![".".to_string()])));

    // 5. Navigation outcomes
    assert_eq!(
        router.execute(&Action::go_to_page("subpage"), &InputContext::default()).expect("goto"),
        ActionOutcome::Navigate(NavigationTarget::Page("subpage".to_string()))
    );
    assert_eq!(
        router.execute(&Action::NextPage, &InputContext::default()).expect("next"),
        ActionOutcome::Navigate(NavigationTarget::Next)
    );
    assert_eq!(
        router.execute(&Action::PrevPage, &InputContext::default()).expect("prev"),
        ActionOutcome::Navigate(NavigationTarget::Prev)
    );
    assert_eq!(
        router.execute(&Action::open_folder("my_folder"), &InputContext::default()).expect("folder"),
        ActionOutcome::Navigate(NavigationTarget::Folder("my_folder".to_string()))
    );
    assert_eq!(
        router.execute(&Action::GoBack, &InputContext::default()).expect("back"),
        ActionOutcome::Navigate(NavigationTarget::Back)
    );
    assert_eq!(
        router.execute(&Action::ToggleState, &InputContext::default()).expect("toggle"),
        ActionOutcome::ToggleState
    );
}

#[test]
fn test_action_router_error_propagation() {
    let (router, injector, launcher) = make_test_router();

    injector.set_failing(true);
    let result = router.execute(&Action::hotkey(Key::Space), &InputContext::default());
    assert!(result.is_err());
    match result.unwrap_err() {
        CoreError::Platform(err) => {
            assert!(err.to_string().contains("mock input failure"));
        }
        err => panic!("unexpected error: {:?}", err),
    }

    launcher.set_failing(true);
    let result = router.execute(&Action::open_url("https://example.com"), &InputContext::default());
    assert!(result.is_err());
    match result.unwrap_err() {
        CoreError::Platform(err) => {
            assert!(err.to_string().contains("mock launch failure"));
        }
        err => panic!("unexpected error: {:?}", err),
    }
}

#[test]
fn test_deck_session_navigation() {
    let mut profile = Profile::new("deck", "Deck");
    profile.add_page(Page::new("page1", "Page 1"));
    profile.add_page(Page::new("page2", "Page 2"));
    profile.add_page(Page::new("folder1", "Folder 1"));

    let mut session = DeckSession::new(profile);
    assert_eq!(session.active_page_id(), "page1");
    assert!(!session.is_in_folder());

    // navigate_to
    let layout = session.navigate_to("page2").expect("nav to page2");
    assert_eq!(layout.page_id, "page2");
    assert_eq!(session.active_page_id(), "page2");

    // next_page
    let layout = session.next_page().expect("next page");
    assert_eq!(layout.page_id, "folder1");

    // next_page cycles
    let layout = session.next_page().expect("next page cycles");
    assert_eq!(layout.page_id, "page1");

    // prev_page cycles
    let layout = session.prev_page().expect("prev page cycles");
    assert_eq!(layout.page_id, "folder1");

    // open_folder pushes onto stack
    let layout = session.open_folder("page2").expect("open folder");
    assert_eq!(layout.page_id, "page2");
    assert!(session.is_in_folder());
    assert_eq!(session.folder_depth(), 1);
    assert_eq!(session.folder_stack(), &["folder1"]);

    // go_back pops from stack
    let layout = session.go_back().expect("go back");
    assert_eq!(layout.page_id, "folder1");
    assert!(!session.is_in_folder());
    assert_eq!(session.folder_depth(), 0);

    // go_back with empty stack stays on current page
    let layout = session.go_back().expect("go back empty");
    assert_eq!(layout.page_id, "folder1");
}

#[test]
fn test_deck_session_control_updates() {
    let mut profile = Profile::new("deck", "Deck");
    let mut page = Page::new("main", "Main");
    page.add_control(Control::slider("vol", 0, 0, 1, 2, false));
    page.add_control(Control::key("mute", 1, 0, "Mute"));
    page.add_control(Control::tile("stats", 2, 0, "CPU", "15%"));
    profile.add_page(page);

    let mut session = DeckSession::new(profile);

    // set_control_value
    let update = session.set_control_value("vol", 0.72).expect("set slider value");
    assert_eq!(update.page_id, "main");
    assert_eq!(update.patches.len(), 1);
    assert_eq!(update.patches[0].id, "vol");
    assert_eq!(update.patches[0].fields, vec!["value"]);
    assert_eq!(update.patches[0].control.as_ref().unwrap().value, 0.72);
    assert_eq!(session.get_control("vol").unwrap().value, 0.72);

    // toggle_control_state
    let update = session.toggle_control_state("mute").expect("toggle mute state");
    assert_eq!(update.patches[0].id, "mute");
    assert_eq!(update.patches[0].control.as_ref().unwrap().state_index, 1);
    assert_eq!(session.get_control("mute").unwrap().state_index, 1);

    // toggle back
    let update = session.toggle_control_state("mute").expect("toggle mute back");
    assert_eq!(update.patches[0].control.as_ref().unwrap().state_index, 0);

    // title / subtitle / icon / color updates
    let update = session.set_control_title("stats", "RAM").expect("set title");
    assert_eq!(update.patches[0].fields, vec!["title"]);
    assert_eq!(update.patches[0].control.as_ref().unwrap().title, "RAM");

    let update = session.set_control_subtitle("stats", "8.2 GB").expect("set subtitle");
    assert_eq!(update.patches[0].fields, vec!["subtitle"]);
    assert_eq!(update.patches[0].control.as_ref().unwrap().subtitle, "8.2 GB");

    let update = session.set_control_icon("stats", "sha256_hash").expect("set icon");
    assert_eq!(update.patches[0].fields, vec!["icon_hash"]);
    assert_eq!(update.patches[0].control.as_ref().unwrap().icon_hash, "sha256_hash");

    let update = session.set_control_colors("stats", 0xFFFFFFFF, 0xFF333333).expect("set colors");
    assert_eq!(update.patches[0].fields, vec!["fg_argb", "bg_argb"]);
    assert_eq!(update.patches[0].control.as_ref().unwrap().fg_argb, 0xFFFFFFFF);
    assert_eq!(update.patches[0].control.as_ref().unwrap().bg_argb, 0xFF333333);
}

#[test]
fn test_deck_session_handle_input_event() {
    let (router, injector, launcher) = make_test_router();

    let mut profile = Profile::new("deck", "Deck");
    let mut page1 = Page::new("p1", "Page 1");

    // Key with Hotkey
    let k_hotkey = Control::key("k_copy", 0, 0, "Copy").with_action(Action::hotkey_with_mods(vec![Key::Ctrl], Key::C));
    page1.add_control(k_hotkey);

    // Key with OpenUrl
    let k_url = Control::key("k_url", 1, 0, "Web").with_action(Action::open_url("https://notideck.dev"));
    page1.add_control(k_url);

    // Key with GoToPage
    let k_nav = Control::key("k_nav", 2, 0, "Page 2").with_action(Action::go_to_page("p2"));
    page1.add_control(k_nav);

    // Key with ToggleState
    let k_toggle = Control::key("k_toggle", 3, 0, "Toggle").with_action(Action::ToggleState);
    page1.add_control(k_toggle);

    // Slider
    let slider = Control::slider("slider1", 4, 0, 1, 1, true);
    page1.add_control(slider);

    profile.add_page(page1);
    profile.add_page(Page::new("p2", "Page 2"));

    let mut session = DeckSession::new(profile);

    // 1. KeyDown on k_copy
    let event = InputEvent {
        page_id: "p1".to_string(),
        control_id: "k_copy".to_string(),
        kind: InputKind::KeyDown as i32,
        delta: 0,
        value: 0.0,
        phone_time_ms: 100,
    };
    let reaction = session.handle_input(&event, &router).expect("handle hotkey");
    assert_eq!(reaction, InputReaction::None);
    assert_eq!(injector.last_combo(), Some(KeyCombo { modifiers: vec![Key::Ctrl], key: Key::C }));

    // 2. KeyDown on k_url
    let event = InputEvent {
        page_id: "p1".to_string(),
        control_id: "k_url".to_string(),
        kind: InputKind::KeyDown as i32,
        delta: 0,
        value: 0.0,
        phone_time_ms: 101,
    };
    let reaction = session.handle_input(&event, &router).expect("handle url");
    assert_eq!(reaction, InputReaction::None);
    assert_eq!(launcher.last_url(), Some("https://notideck.dev".to_string()));

    // 3. KeyDown on k_nav -> triggers SetLayout reaction!
    let event = InputEvent {
        page_id: "p1".to_string(),
        control_id: "k_nav".to_string(),
        kind: InputKind::KeyDown as i32,
        delta: 0,
        value: 0.0,
        phone_time_ms: 102,
    };
    let reaction = session.handle_input(&event, &router).expect("handle nav");
    match reaction {
        InputReaction::SetLayout(layout) => {
            assert_eq!(layout.page_id, "p2");
        }
        other => panic!("expected SetLayout, got {:?}", other),
    }
    assert_eq!(session.active_page_id(), "p2");

    // Navigate back to p1
    session.navigate_to("p1").expect("nav back");

    // 4. KeyDown on k_toggle -> triggers UpdateControls reaction!
    let event = InputEvent {
        page_id: "p1".to_string(),
        control_id: "k_toggle".to_string(),
        kind: InputKind::KeyDown as i32,
        delta: 0,
        value: 0.0,
        phone_time_ms: 103,
    };
    let reaction = session.handle_input(&event, &router).expect("handle toggle");
    match reaction {
        InputReaction::UpdateControls(update) => {
            assert_eq!(update.page_id, "p1");
            assert_eq!(update.patches[0].id, "k_toggle");
            assert_eq!(update.patches[0].control.as_ref().unwrap().state_index, 1);
        }
        other => panic!("expected UpdateControls, got {:?}", other),
    }

    // 5. Slider value changed -> triggers UpdateControls
    let event = InputEvent {
        page_id: "p1".to_string(),
        control_id: "slider1".to_string(),
        kind: InputKind::SliderValue as i32,
        delta: 0,
        value: 0.65,
        phone_time_ms: 104,
    };
    let reaction = session.handle_input(&event, &router).expect("handle slider");
    match reaction {
        InputReaction::UpdateControls(update) => {
            assert_eq!(update.page_id, "p1");
            assert_eq!(update.patches[0].id, "slider1");
            assert_eq!(update.patches[0].control.as_ref().unwrap().value, 0.65);
        }
        other => panic!("expected UpdateControls, got {:?}", other),
    }
    assert_eq!(session.get_control("slider1").unwrap().value, 0.65);
}

#[test]
fn test_multi_action_runs_steps_in_order_with_delay() {
    use crate::model::MultiStep;
    let (router, injector, launcher) = make_test_router();
    let multi = Action::multi(vec![
        MultiStep::new(Action::hotkey(Key::A)),
        MultiStep::after(30, Action::type_text("hi")),
        MultiStep::new(Action::open_url("https://example.com")),
    ]);
    let start = std::time::Instant::now();
    assert_eq!(router.execute(&multi, &InputContext::default()).unwrap(), ActionOutcome::Executed);
    assert!(start.elapsed() >= std::time::Duration::from_millis(30));
    assert_eq!(injector.key_combos().len(), 1);
    assert_eq!(injector.typed_texts(), vec!["hi".to_string()]);
    assert_eq!(launcher.opened_urls(), vec!["https://example.com".to_string()]);
}

#[test]
fn test_multi_action_propagates_navigation_and_aborts_on_error() {
    use crate::model::MultiStep;
    let (router, injector, _l) = make_test_router();
    let nav = Action::multi(vec![MultiStep::new(Action::type_text("x")), MultiStep::new(Action::open_folder("f"))]);
    assert_eq!(
        router.execute(&nav, &InputContext::default()).unwrap(),
        ActionOutcome::Navigate(NavigationTarget::Folder("f".into()))
    );
    injector.set_failing(true);
    let bad = Action::multi(vec![
        MultiStep::new(Action::type_text("a")),
        MultiStep::new(Action::open_url("https://never.example")),
    ]);
    assert!(router.execute(&bad, &InputContext::default()).is_err());
}

#[test]
fn test_multi_action_rejects_nested_and_long_delays() {
    use crate::model::MultiStep;
    let (router, _i, _l) = make_test_router();
    let nested = Action::multi(vec![MultiStep::new(Action::multi(vec![]))]);
    assert!(router.execute(&nested, &InputContext::default()).is_err());
    let slow = Action::multi(vec![MultiStep::after(60_000, Action::GoBack)]);
    assert!(router.execute(&slow, &InputContext::default()).is_err());
}

#[test]
fn test_multi_action_json_roundtrip() {
    use crate::model::MultiStep;
    let a = Action::multi(vec![MultiStep::new(Action::hotkey(Key::A)), MultiStep::after(100, Action::ToggleState)]);
    let json = serde_json::to_string(&a).unwrap();
    assert_eq!(serde_json::from_str::<Action>(&json).unwrap(), a);
    let parsed: Action = serde_json::from_str(
        r#"{"type":"multi","steps":[{"action":{"type":"go_back"}},{"action":{"type":"toggle_state"},"delay_ms":5}]}"#,
    )
    .unwrap();
    assert!(matches!(parsed, Action::Multi { ref steps } if steps.len() == 2 && steps[1].delay_ms == 5));
}

#[test]
fn test_toggle_via_session_with_multi() {
    use crate::model::MultiStep;
    let (router, _i, _l) = make_test_router();
    let mut profile = Profile::new("p", "P");
    let mut page = Page::new("home", "Home");
    page.add_control(
        Control::key("k", 0, 0, "K").with_action(Action::multi(vec![
            MultiStep::new(Action::type_text("x")),
            MultiStep::new(Action::ToggleState),
        ])),
    );
    profile.add_page(page);
    let mut session = DeckSession::new(profile);
    let ev = InputEvent {
        page_id: "home".into(),
        control_id: "k".into(),
        kind: InputKind::KeyDown as i32,
        ..Default::default()
    };
    match session.handle_input(&ev, &router).unwrap() {
        InputReaction::UpdateControls(u) => assert_eq!(u.patches[0].id, "k"),
        other => panic!("unexpected {other:?}"),
    }
}

// ---------------------------------------------------------------- Round 3

mod r3 {
    use super::*;
    use crate::live::{civil_from_unix, clock_sample, format_rate, sample_all, LiveProviders, LiveSample};
    use crate::mock::MockAudio;
    use crate::model::DataSource;
    use nd_platform::{AudioControl, AudioTarget, MediaInfo, NowPlaying, NullMedia, SystemSnapshot, SystemStats};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn router_with_audio() -> (ActionRouter, MockAudio) {
        let audio = MockAudio::new();
        let router = ActionRouter::new(Arc::new(MockInputInjector::new()), Arc::new(MockLauncher::new()))
            .with_audio(Arc::new(audio.clone()));
        (router, audio)
    }

    fn dial_event(delta: i32) -> InputEvent {
        InputEvent {
            page_id: "p".into(),
            control_id: "c".into(),
            kind: InputKind::DialRotate as i32,
            delta,
            value: 0.0,
            phone_time_ms: 0,
        }
    }

    fn session_with(control: Control) -> DeckSession {
        let mut page = Page::new("p", "P");
        page.add_control(control);
        let mut profile = Profile::new("t", "T");
        profile.add_page(page);
        DeckSession::new(profile)
    }

    #[test]
    fn adjust_volume_by_dial_delta() {
        let (router, audio) = router_with_audio();
        let act = Action::AdjustVolume { target: AudioTarget::Output, step: 0.1 };
        let up = router.execute(&act, &InputContext { delta: 3, value: 0.0, ..Default::default() }).unwrap();
        match up {
            ActionOutcome::SetValue(v) => assert!((v - 0.8).abs() < 1e-5),
            o => panic!("{o:?}"),
        }
        let down = router.execute(&act, &InputContext { delta: -3, value: 0.0, ..Default::default() }).unwrap();
        match down {
            ActionOutcome::SetValue(v) => assert!((v - 0.5).abs() < 1e-5),
            o => panic!("{o:?}"),
        }
        // delta 0 counts as one step; clamped at 1.0
        router.execute(&act, &InputContext::default()).unwrap();
        audio.set_volume(AudioTarget::Output, 0.95).unwrap();
        let capped = router.execute(&act, &InputContext { delta: 5, value: 0.0, ..Default::default() }).unwrap();
        assert_eq!(capped, ActionOutcome::SetValue(1.0));
    }

    #[test]
    fn set_volume_uses_level_or_context_value() {
        let (router, audio) = router_with_audio();
        let ctx = InputContext { delta: 0, value: 0.3, ..Default::default() };
        let o = router.execute(&Action::SetVolume { target: AudioTarget::Input, level: None }, &ctx).unwrap();
        assert_eq!(o, ActionOutcome::SetValue(0.3));
        assert_eq!(audio.volume(AudioTarget::Input).unwrap(), 0.3);
        let o = router.execute(&Action::SetVolume { target: AudioTarget::Input, level: Some(2.0) }, &ctx).unwrap();
        assert_eq!(o, ActionOutcome::SetValue(1.0));
    }

    #[test]
    fn toggle_mute_flips_state() {
        let (router, audio) = router_with_audio();
        let act = Action::ToggleMute { target: AudioTarget::Output };
        let ctx = InputContext::default();
        assert_eq!(router.execute(&act, &ctx).unwrap(), ActionOutcome::SetState(1));
        assert!(audio.muted(AudioTarget::Output).unwrap());
        assert_eq!(router.execute(&act, &ctx).unwrap(), ActionOutcome::SetState(0));
    }

    #[test]
    fn audio_errors_without_backend() {
        let (router, _, _) = make_test_router();
        let act = Action::ToggleMute { target: AudioTarget::Output };
        assert!(router.execute(&act, &InputContext::default()).is_err());
    }

    #[test]
    fn dial_rotate_session_patches_value() {
        let (router, audio) = router_with_audio();
        let mut c = Control::dial("c", 0, 0, "Vol");
        c.actions.push(ActionDefinition::with_trigger(
            Action::AdjustVolume { target: AudioTarget::Output, step: 0.1 },
            TriggerKind::DialRotate,
        ));
        let mut session = session_with(c);
        let r = session.handle_input(&dial_event(3), &router).unwrap();
        let InputReaction::UpdateControls(u) = r else { panic!("{r:?}") };
        assert_eq!(u.patches.len(), 1);
        assert!((u.patches[0].control.as_ref().unwrap().value - 0.8).abs() < 1e-5);
        assert!((audio.volume(AudioTarget::Output).unwrap() - 0.8).abs() < 1e-5);
    }

    #[test]
    fn dial_without_value_action_moves_dial_by_step() {
        let (router, _) = router_with_audio();
        let mut c = Control::dial("c", 0, 0, "D");
        c.value = 0.5;
        let mut session = session_with(c);
        let r = session.handle_input(&dial_event(-5), &router).unwrap();
        assert!(matches!(r, InputReaction::UpdateControls(_)));
        assert!((session.get_control("c").unwrap().value - 0.4).abs() < 1e-5);
    }

    #[test]
    fn dial_cw_ccw_repeat_and_cap() {
        let mut c = Control::dial("c", 0, 0, "D");
        c.actions.push(ActionDefinition::with_trigger(Action::hotkey(Key::VolumeUp), TriggerKind::DialRotateCw));
        c.actions.push(ActionDefinition::with_trigger(Action::hotkey(Key::VolumeDown), TriggerKind::DialRotateCcw));
        let injector = MockInputInjector::new();
        let router = ActionRouter::new(Arc::new(injector.clone()), Arc::new(MockLauncher::new()));
        let mut session = session_with(c);
        session.handle_input(&dial_event(3), &router).unwrap();
        assert_eq!(injector.key_combos().len(), 3);
        assert!(injector.key_combos().iter().all(|k| k.key == Key::VolumeUp));
        injector.clear();
        session.handle_input(&dial_event(-2), &router).unwrap();
        assert_eq!(injector.key_combos().len(), 2);
        assert!(injector.key_combos().iter().all(|k| k.key == Key::VolumeDown));
        injector.clear();
        session.handle_input(&dial_event(100), &router).unwrap();
        assert_eq!(injector.key_combos().len(), 20);
    }

    #[test]
    fn slider_set_volume_merges_value_patch() {
        let (router, audio) = router_with_audio();
        let mut c = Control::slider("c", 0, 0, 2, 1, true);
        c.actions.push(ActionDefinition::with_trigger(
            Action::SetVolume { target: AudioTarget::Input, level: None },
            TriggerKind::SliderValue,
        ));
        let mut session = session_with(c);
        let ev = InputEvent {
            page_id: "p".into(),
            control_id: "c".into(),
            kind: InputKind::SliderValue as i32,
            delta: 0,
            value: 0.42,
            phone_time_ms: 0,
        };
        let r = session.handle_input(&ev, &router).unwrap();
        let InputReaction::UpdateControls(u) = r else { panic!("{r:?}") };
        assert_eq!(u.patches.len(), 1);
        assert_eq!(u.patches[0].fields, vec!["value".to_string()]);
        assert_eq!(audio.volume(AudioTarget::Input).unwrap(), 0.42);
        assert_eq!(session.get_control("c").unwrap().value, 0.42);
    }

    #[test]
    fn toggle_mute_session_state_patch() {
        let (router, _) = router_with_audio();
        let mut c = Control::key("c", 0, 0, "Mute").with_action(Action::ToggleMute { target: AudioTarget::Output });
        c.states = vec![
            ControlStateVisual::default(),
            ControlStateVisual { title: Some("Muted".into()), ..Default::default() },
        ];
        let mut session = session_with(c);
        let ev = InputEvent {
            page_id: "p".into(),
            control_id: "c".into(),
            kind: InputKind::KeyDown as i32,
            delta: 0,
            value: 0.0,
            phone_time_ms: 0,
        };
        let r = session.handle_input(&ev, &router).unwrap();
        let InputReaction::UpdateControls(u) = r else { panic!("{r:?}") };
        let ctrl = u.patches[0].control.as_ref().unwrap();
        assert_eq!(ctrl.state_index, 1);
        assert_eq!(ctrl.title, "Muted");
        assert!(u.patches[0].fields.contains(&"state_index".to_string()));
    }

    #[test]
    fn apply_live_detects_changes() {
        let mut c = Control::tile("c", 0, 0, "CPU", "");
        c.source = Some(DataSource::Cpu);
        let mut session = session_with(c);
        assert_eq!(session.live_sources(), vec![("c".to_string(), DataSource::Cpu)]);
        let s = LiveSample { value: Some(0.37), title: Some("37%".into()), ..Default::default() };
        let u = session.apply_live(&[("c".into(), s.clone())]).expect("changed");
        assert_eq!(u.patches[0].fields, vec!["value".to_string(), "title".to_string()]);
        assert!(session.apply_live(&[("c".into(), s)]).is_none());
        // within tolerance: no patch
        let s2 = LiveSample { value: Some(0.372), ..Default::default() };
        assert!(session.apply_live(&[("c".into(), s2)]).is_none());
        let s3 = LiveSample { value: Some(0.5), ..Default::default() };
        assert!(session.apply_live(&[("c".into(), s3)]).is_some());
        assert!(session.apply_live(&[("c".into(), LiveSample::default())]).is_none());
    }

    struct CountingStats(AtomicUsize);
    impl SystemStats for CountingStats {
        fn snapshot(&self) -> nd_platform::Result<SystemSnapshot> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(SystemSnapshot {
                cpu_percent: 37.2,
                mem_used_bytes: 6_100_000_000,
                mem_total_bytes: 16_000_000_000,
                net_rx_bytes_per_sec: 1_200_000,
                net_tx_bytes_per_sec: 340_000,
            })
        }
    }

    struct FakeMedia;
    impl MediaInfo for FakeMedia {
        fn now_playing(&self) -> nd_platform::Result<Option<NowPlaying>> {
            Ok(Some(NowPlaying {
                title: "Song".into(),
                artist: "Artist".into(),
                playing: true,
                progress: Some(0.25),
                ..Default::default()
            }))
        }
    }

    #[test]
    fn sample_all_formats_and_samples_stats_once() {
        let stats = Arc::new(CountingStats(AtomicUsize::new(0)));
        let audio = MockAudio::new();
        audio.set_muted(AudioTarget::Output, true).unwrap();
        let p = LiveProviders {
            audio: Arc::new(audio),
            media: Arc::new(FakeMedia),
            stats: stats.clone(),
            camera: Arc::new(crate::NullCamera),
            mic: Arc::new(crate::NullMic),
            profile_actions: Arc::new(crate::autoswitch::NullProfileActions),
            mixer: Arc::new(crate::mixer::Mixer::new(
                Arc::new(nd_platform::NullAppAudio),
                Arc::new(nd_platform::NullAudio),
            )),
            obs: std::sync::Arc::new(crate::obs::NullObs),
            icons: None,
        };
        let src = vec![
            ("cpu".to_string(), DataSource::Cpu),
            ("mem".to_string(), DataSource::Memory),
            ("net".to_string(), DataSource::Network),
            ("vol".to_string(), DataSource::Volume { target: AudioTarget::Output }),
            ("mute".to_string(), DataSource::Mute { target: AudioTarget::Output }),
            ("np".to_string(), DataSource::NowPlaying { art: false }),
            ("clock".to_string(), DataSource::Clock),
        ];
        let out = sample_all(&src, &p);
        assert_eq!(stats.0.load(Ordering::SeqCst), 1);
        let get = |id: &str| out.iter().find(|(i, _)| i == id).unwrap().1.clone();
        assert_eq!(get("cpu").title.as_deref(), Some("37%"));
        assert!((get("cpu").value.unwrap() - 0.372).abs() < 1e-5);
        assert_eq!(get("mem").title.as_deref(), Some("6.1 GB"));
        assert_eq!(get("net").title.as_deref(), Some("1.2 MB/s"));
        assert_eq!(get("net").subtitle.as_deref(), Some("340 kB/s"));
        assert_eq!(get("vol").subtitle.as_deref(), Some("50%"));
        assert_eq!(get("mute").state_index, Some(1));
        let np = get("np");
        assert_eq!((np.title.as_deref(), np.subtitle.as_deref()), (Some("Song"), Some("Artist")));
        assert_eq!((np.value, np.state_index), (Some(0.25), Some(1)));
        assert_eq!(get("clock").title.as_ref().unwrap().len(), 5);
    }

    struct ScriptedMedia {
        calls: AtomicUsize,
        np: Option<NowPlaying>,
    }
    impl MediaInfo for ScriptedMedia {
        fn now_playing(&self) -> nd_platform::Result<Option<NowPlaying>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.np.clone())
        }
    }

    fn media_providers(media: Arc<dyn MediaInfo>, icons: Option<crate::mixer::IconImporter>) -> LiveProviders {
        LiveProviders {
            audio: Arc::new(nd_platform::NullAudio),
            media,
            stats: Arc::new(nd_platform::NullStats),
            camera: Arc::new(crate::NullCamera),
            mic: Arc::new(crate::NullMic),
            profile_actions: Arc::new(crate::autoswitch::NullProfileActions),
            mixer: Arc::new(crate::mixer::Mixer::new(
                Arc::new(nd_platform::NullAppAudio),
                Arc::new(nd_platform::NullAudio),
            )),
            obs: std::sync::Arc::new(crate::obs::NullObs),
            icons,
        }
    }

    #[test]
    fn media_detail_formatting() {
        use crate::live::format_times;
        assert_eq!(format_times(Some(0.0), Some(225.0)), "0:00 / 3:45");
        assert_eq!(format_times(Some(83.4), Some(225.0)), "1:23 / 3:45");
        assert_eq!(format_times(Some(3723.0), Some(7200.0)), "1:02:03 / 2:00:00");
        assert_eq!(format_times(Some(65.0), None), "1:05");
        assert_eq!(format_times(None, Some(225.0)), "");
        assert_eq!(format_times(None, None), "");
    }

    #[test]
    fn media_sources_call_player_once_and_fill_samples() {
        let np = NowPlaying {
            title: "T".into(),
            artist: "A".into(),
            playing: true,
            progress: None,
            position_s: Some(10.0),
            length_s: Some(100.0),
            ..Default::default()
        };
        let media = Arc::new(ScriptedMedia { calls: AtomicUsize::new(0), np: Some(np) });
        let p = media_providers(media.clone(), None);
        let src = vec![
            ("a".to_string(), DataSource::NowPlaying { art: true }),
            ("b".to_string(), DataSource::NowPlaying { art: false }),
            ("c".to_string(), DataSource::MediaPlaying),
        ];
        let out = sample_all(&src, &p);
        assert_eq!(media.calls.load(Ordering::SeqCst), 1);
        let a = &out[0].1;
        assert_eq!(a.detail.as_deref(), Some("0:10 / 1:40"));
        assert_eq!((a.value, a.state_index, a.dimmed), (Some(0.0), Some(1), Some(false)));
        assert_eq!(a.icon_hash.as_deref(), Some(""));
        assert_eq!(out[1].1.icon_hash, None);
        assert_eq!(out[2].1, LiveSample { state_index: Some(1), dimmed: Some(false), ..Default::default() });
    }

    #[test]
    fn media_no_player_is_dimmed_and_clears() {
        let media = Arc::new(ScriptedMedia { calls: AtomicUsize::new(0), np: None });
        let p = media_providers(media, None);
        let src =
            vec![("a".to_string(), DataSource::NowPlaying { art: true }), ("c".to_string(), DataSource::MediaPlaying)];
        let out = sample_all(&src, &p);
        let a = &out[0].1;
        assert_eq!(a.title.as_deref(), Some(""));
        assert_eq!(a.subtitle.as_deref(), Some(""));
        assert_eq!(a.detail.as_deref(), Some(""));
        assert_eq!((a.value, a.state_index, a.dimmed), (Some(0.0), Some(0), Some(true)));
        assert_eq!(a.icon_hash.as_deref(), Some(""));
        assert_eq!(out[1].1, LiveSample { state_index: Some(0), dimmed: Some(true), ..Default::default() });
    }

    #[test]
    fn media_art_is_imported_once_per_track() {
        let art: Arc<[u8]> = Arc::from(&b"png-bytes"[..]);
        let np = NowPlaying { title: "T".into(), art: Some(art.clone()), ..Default::default() };
        let media = Arc::new(ScriptedMedia { calls: AtomicUsize::new(0), np: Some(np) });
        let imports = Arc::new(AtomicUsize::new(0));
        let n = imports.clone();
        let importer: crate::mixer::IconImporter = Arc::new(move |b| {
            n.fetch_add(1, Ordering::SeqCst);
            Some(format!("h{}", b.len()))
        });
        let p = media_providers(media, Some(importer));
        let src = vec![("a".to_string(), DataSource::NowPlaying { art: true })];
        for _ in 0..3 {
            let out = sample_all(&src, &p);
            assert_eq!(out[0].1.icon_hash.as_deref(), Some("h9"));
        }
        assert_eq!(imports.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn media_source_serde_and_face_roundtrip() {
        let a: DataSource = serde_json::from_str(r#"{"type":"now_playing","art":true}"#).unwrap();
        assert_eq!(a, DataSource::NowPlaying { art: true });
        assert_eq!(serde_json::to_string(&a).unwrap(), r#"{"type":"now_playing","art":true}"#);
        let n: DataSource = serde_json::from_str(r#"{"type":"now_playing"}"#).unwrap();
        assert_eq!(serde_json::to_string(&n).unwrap(), r#"{"type":"now_playing"}"#);
        let m: DataSource = serde_json::from_str(r#"{"type":"media_playing"}"#).unwrap();
        assert_eq!(m, DataSource::MediaPlaying);
        assert_eq!(serde_json::to_string(&m).unwrap(), r#"{"type":"media_playing"}"#);

        let mut c = Control::tile("c", 0, 0, "x", "");
        assert!(!serde_json::to_string(&c).unwrap().contains("face"));
        c.face = crate::model::ControlFace::Media;
        let j = serde_json::to_string(&c).unwrap();
        assert!(j.contains(r#""face":"media""#), "{j}");
        let back: Control = serde_json::from_str(&j).unwrap();
        assert_eq!(back.face, crate::model::ControlFace::Media);
    }

    #[test]
    fn camera_face_roundtrip_layout_and_input_noop() {
        let mut c = Control::tile("cam", 0, 0, "Camera", "");
        c.face = crate::model::ControlFace::Camera;
        let j = serde_json::to_string(&c).unwrap();
        assert!(j.contains(r#""face":"camera""#), "{j}");
        let back: Control = serde_json::from_str(&j).unwrap();
        assert_eq!(back.face, crate::model::ControlFace::Camera);

        let mut page = Page::new("p", "P");
        page.add_control(c);
        let mut profile = Profile::new("t", "T");
        profile.add_page(page);
        let pj = serde_json::to_string(&profile).unwrap();
        let p2: Profile = serde_json::from_str(&pj).unwrap();
        assert_eq!(p2.pages[0].controls[0].face, crate::model::ControlFace::Camera);

        let mut session = DeckSession::new(p2);
        let layout = session.current_layout().unwrap();
        assert_eq!(layout.controls[0].face, nd_proto::v1::ControlFace::Camera as i32);

        // Older phones still send KEY_DOWN/UP: no actions bound, so nothing happens.
        let (router, _audio) = router_with_audio();
        for kind in [InputKind::KeyDown, InputKind::KeyUp] {
            let ev =
                InputEvent { page_id: "p".into(), control_id: "cam".into(), kind: kind as i32, ..Default::default() };
            assert!(matches!(session.handle_input(&ev, &router).unwrap(), InputReaction::None));
        }
    }

    #[test]
    fn apply_live_emits_detail_only_on_change() {
        let mut c = Control::tile("c", 0, 0, "x", "");
        c.face = crate::model::ControlFace::Media;
        let mut session = session_with(c);
        let s = LiveSample { detail: Some("0:01 / 3:00".into()), ..Default::default() };
        let u = session.apply_live(&[("c".into(), s.clone())]).expect("changed");
        assert_eq!(u.patches[0].fields, vec!["detail".to_string()]);
        let pc = u.patches[0].control.as_ref().unwrap();
        assert_eq!(pc.detail, "0:01 / 3:00");
        assert_eq!(pc.face, nd_proto::v1::ControlFace::Media as i32);
        assert!(session.apply_live(&[("c".into(), s)]).is_none());
        let s2 = LiveSample { detail: Some("0:02 / 3:00".into()), ..Default::default() };
        assert!(session.apply_live(&[("c".into(), s2)]).is_some());
    }

    #[test]
    fn provider_errors_give_empty_samples() {
        let p = LiveProviders {
            audio: Arc::new(nd_platform::NullAudio),
            media: Arc::new(NullMedia),
            stats: Arc::new(nd_platform::NullStats),
            camera: Arc::new(crate::NullCamera),
            mic: Arc::new(crate::NullMic),
            profile_actions: Arc::new(crate::autoswitch::NullProfileActions),
            mixer: Arc::new(crate::mixer::Mixer::new(
                Arc::new(nd_platform::NullAppAudio),
                Arc::new(nd_platform::NullAudio),
            )),
            obs: std::sync::Arc::new(crate::obs::NullObs),
            icons: None,
        };
        let src = vec![
            ("a".to_string(), DataSource::Cpu),
            ("b".to_string(), DataSource::Volume { target: AudioTarget::Output }),
            ("c".to_string(), DataSource::Mute { target: AudioTarget::Input }),
        ];
        for (_, s) in sample_all(&src, &p) {
            assert_eq!(s, LiveSample::default());
        }
    }

    #[test]
    fn clock_and_rate_formatting() {
        // 2026-10-11 14:05 UTC is a Sunday.
        let c = civil_from_unix(1_791_727_500);
        assert_eq!((c.year, c.month, c.day, c.hour, c.minute, c.weekday), (2026, 10, 11, 14, 5, 0));
        let s = clock_sample(c);
        assert_eq!(s.title.as_deref(), Some("14:05"));
        assert_eq!(s.subtitle.as_deref(), Some("Sun 11 Oct"));
        assert_eq!(format_rate(512), "512 B/s");
        assert_eq!(format_rate(340_000), "340 kB/s");
        assert_eq!(format_rate(1_200_000), "1.2 MB/s");
    }

    #[test]
    fn json_shapes_roundtrip() {
        let j = r#"{"trigger":"dial_rotate","action":{"type":"adjust_volume","target":"output","step":0.02}}"#;
        let d: ActionDefinition = serde_json::from_str(j).unwrap();
        assert_eq!(d.trigger, TriggerKind::DialRotate);
        assert_eq!(d.action, Action::AdjustVolume { target: AudioTarget::Output, step: 0.02 });
        let d2: ActionDefinition = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(d, d2);

        let d: ActionDefinition =
            serde_json::from_str(r#"{"trigger":"slider_value","action":{"type":"set_volume","target":"input"}}"#)
                .unwrap();
        assert_eq!(d.action, Action::SetVolume { target: AudioTarget::Input, level: None });
        let d: ActionDefinition =
            serde_json::from_str(r#"{"trigger":"key_down","action":{"type":"toggle_mute","target":"output"}}"#)
                .unwrap();
        assert_eq!(d.action, Action::ToggleMute { target: AudioTarget::Output });
        let d: ActionDefinition = serde_json::from_str(
            r#"{"trigger":"dial_rotate_cw","action":{"type":"hotkey","combo":{"modifiers":["ctrl"],"key":"equal"}}}"#,
        )
        .unwrap();
        assert_eq!(d.trigger, TriggerKind::DialRotateCw);

        for (j, want) in [
            (r#"{"type":"volume","target":"output"}"#, DataSource::Volume { target: AudioTarget::Output }),
            (r#"{"type":"cpu"}"#, DataSource::Cpu),
            (r#"{"type":"now_playing"}"#, DataSource::NowPlaying { art: false }),
            (r#"{"type":"mute","target":"input"}"#, DataSource::Mute { target: AudioTarget::Input }),
        ] {
            let got: DataSource = serde_json::from_str(j).unwrap();
            assert_eq!(got, want);
        }
    }

    #[test]
    fn default_profile_has_no_overlaps_and_roundtrips() {
        let p = crate::store::default_profile();
        for page in &p.pages {
            let (cols, rows) = (page.cols.unwrap_or(p.cols), page.rows.unwrap_or(p.rows));
            let mut grid = std::collections::HashSet::new();
            for c in &page.controls {
                for dc in 0..c.span_w {
                    for dr in 0..c.span_h {
                        let cell = (c.col + dc, c.row + dr);
                        assert!(cell.0 < cols && cell.1 < rows, "{} out of grid", c.id);
                        assert!(grid.insert(cell), "{} overlaps at {cell:?}", c.id);
                    }
                }
            }
        }
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Profile>(&json).unwrap(), p);
    }
}

#[test]
fn test_set_injector_swaps_for_all_clones() {
    let (router, first, _l) = make_test_router();
    let clone = router.clone();
    let second = MockInputInjector::new();
    router.set_injector(Arc::new(second.clone()));
    clone.execute(&Action::type_text("swapped"), &InputContext::default()).unwrap();
    assert!(first.typed_texts().is_empty());
    assert_eq!(second.typed_texts(), vec!["swapped".to_string()]);
}

mod camera_actions {
    use super::*;
    use crate::camera::{CameraActions, CameraSnapshot};
    use crate::live::{sample_all, LiveProviders};
    use crate::model::DataSource;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeCam(Mutex<CameraSnapshot>);

    impl CameraActions for FakeCam {
        fn toggle(&self) -> Result<bool, String> {
            let mut s = self.0.lock().unwrap();
            s.active = !s.active;
            Ok(s.active)
        }
        fn switch_lens(&self) -> Result<bool, String> {
            let mut s = self.0.lock().unwrap();
            s.front_lens = !s.front_lens;
            Ok(s.front_lens)
        }
        fn toggle_torch(&self) -> Result<bool, String> {
            let mut s = self.0.lock().unwrap();
            s.torch = !s.torch;
            Ok(s.torch)
        }
        fn zoom_by(&self, delta: f32) -> Result<f32, String> {
            let mut s = self.0.lock().unwrap();
            s.zoom = (s.zoom + delta).clamp(1.0, 8.0);
            Ok(s.zoom)
        }
        fn snapshot(&self) -> CameraSnapshot {
            *self.0.lock().unwrap()
        }
    }

    fn router() -> (ActionRouter, Arc<FakeCam>) {
        let cam = Arc::new(FakeCam(Mutex::new(CameraSnapshot::default())));
        let (r, _, _) = make_test_router();
        (r.with_camera(cam.clone()), cam)
    }

    #[test]
    fn camera_actions_route_to_backend() {
        let (r, cam) = router();
        let ctx = InputContext::default();
        assert_eq!(r.execute(&Action::CameraToggle, &ctx).unwrap(), ActionOutcome::SetState(1));
        assert_eq!(r.execute(&Action::CameraToggle, &ctx).unwrap(), ActionOutcome::SetState(0));
        assert_eq!(r.execute(&Action::CameraTorch, &ctx).unwrap(), ActionOutcome::SetState(1));
        assert!(cam.snapshot().torch);
        assert_eq!(r.execute(&Action::CameraSwitchLens, &ctx).unwrap(), ActionOutcome::Executed);
        assert!(cam.snapshot().front_lens);
    }

    #[test]
    fn zoom_scales_with_dial_delta_and_clamps() {
        let (r, cam) = router();
        let act = Action::CameraZoom { step: 0.5 };
        let o = r.execute(&act, &InputContext { delta: 4, value: 0.0, ..Default::default() }).unwrap();
        assert_eq!(cam.snapshot().zoom, 3.0);
        assert_eq!(o, ActionOutcome::SetValue((3.0 - 1.0) / 7.0));
        r.execute(&act, &InputContext { delta: -100, value: 0.0, ..Default::default() }).unwrap();
        assert_eq!(cam.snapshot().zoom, 1.0);
        r.execute(&act, &InputContext { delta: 100, value: 0.0, ..Default::default() }).unwrap();
        assert_eq!(cam.snapshot().zoom, 8.0);
    }

    #[test]
    fn camera_actions_fail_without_backend() {
        let (r, _, _) = make_test_router();
        let err = r.execute(&Action::CameraToggle, &InputContext::default()).unwrap_err();
        assert!(matches!(err, CoreError::Camera(_)));
    }

    #[test]
    fn serde_round_trip_and_defaults() {
        for a in [Action::CameraToggle, Action::CameraSwitchLens, Action::CameraTorch, Action::CameraZoom { step: 0.5 }]
        {
            let json = serde_json::to_string(&a).unwrap();
            assert_eq!(serde_json::from_str::<Action>(&json).unwrap(), a);
        }
        assert_eq!(
            serde_json::from_str::<Action>(r#"{"type":"camera_zoom"}"#).unwrap(),
            Action::CameraZoom { step: 0.25 }
        );
        assert_eq!(serde_json::from_str::<DataSource>(r#"{"type":"camera_torch"}"#).unwrap(), DataSource::CameraTorch);
    }

    #[test]
    fn live_sources_read_the_snapshot() {
        let (r, cam) = router();
        r.execute(&Action::CameraTorch, &InputContext::default()).unwrap();
        r.execute(&Action::CameraZoom { step: 1.0 }, &InputContext::default()).unwrap();
        cam.toggle().unwrap();
        let p = LiveProviders {
            audio: Arc::new(nd_platform::NullAudio),
            media: Arc::new(nd_platform::NullMedia),
            stats: Arc::new(nd_platform::NullStats),
            camera: cam.clone(),
            mic: Arc::new(crate::NullMic),
            profile_actions: Arc::new(crate::autoswitch::NullProfileActions),
            mixer: Arc::new(crate::mixer::Mixer::new(
                Arc::new(nd_platform::NullAppAudio),
                Arc::new(nd_platform::NullAudio),
            )),
            obs: std::sync::Arc::new(crate::obs::NullObs),
            icons: None,
        };
        let src = vec![
            ("t".to_string(), DataSource::CameraTorch),
            ("s".to_string(), DataSource::CameraState),
            ("z".to_string(), DataSource::CameraZoom),
        ];
        let out = sample_all(&src, &p);
        assert_eq!(out[0].1.state_index, Some(1));
        assert_eq!(out[1].1.state_index, Some(1));
        assert_eq!(out[2].1.subtitle.as_deref(), Some("2.0x"));
        assert!((out[2].1.value.unwrap() - 1.0 / 7.0).abs() < 1e-6);
    }

    // ---- round 9: main menu / auto-switch pause
    use crate::autoswitch::ProfileActions;
    use crate::PauseMode;

    #[derive(Default)]
    struct MockProfiles {
        paused: Mutex<bool>,
        homes: Mutex<Vec<bool>>,
    }
    impl ProfileActions for MockProfiles {
        fn go_home(&self, pause: bool) -> std::result::Result<(), String> {
            self.homes.lock().unwrap().push(pause);
            if pause {
                *self.paused.lock().unwrap() = true;
            }
            Ok(())
        }
        fn set_paused(&self, mode: PauseMode) -> std::result::Result<bool, String> {
            let mut p = self.paused.lock().unwrap();
            *p = match mode {
                PauseMode::Toggle => !*p,
                PauseMode::Pause => true,
                PauseMode::Resume => false,
            };
            Ok(*p)
        }
        fn paused(&self) -> bool {
            *self.paused.lock().unwrap()
        }
    }

    #[test]
    fn profile_actions_route_and_report_state() {
        let mock = Arc::new(MockProfiles::default());
        let (r, _, _) = make_test_router();
        let r = r.with_profile_actions(mock.clone());
        let ctx = InputContext::default();
        assert_eq!(r.execute(&Action::GoHome { pause_auto_switch: false }, &ctx).unwrap(), ActionOutcome::Executed);
        assert_eq!(r.execute(&Action::GoHome { pause_auto_switch: true }, &ctx).unwrap(), ActionOutcome::Executed);
        assert_eq!(*mock.homes.lock().unwrap(), vec![false, true]);
        let toggle = Action::AutoSwitchPause { mode: PauseMode::Toggle };
        assert_eq!(r.execute(&toggle, &ctx).unwrap(), ActionOutcome::SetState(0)); // was paused by go_home
        assert_eq!(r.execute(&toggle, &ctx).unwrap(), ActionOutcome::SetState(1));
        assert_eq!(
            r.execute(&Action::AutoSwitchPause { mode: PauseMode::Resume }, &ctx).unwrap(),
            ActionOutcome::SetState(0)
        );
        let p = LiveProviders {
            audio: Arc::new(nd_platform::NullAudio),
            media: Arc::new(nd_platform::NullMedia),
            stats: Arc::new(nd_platform::NullStats),
            camera: Arc::new(crate::NullCamera),
            mic: Arc::new(crate::NullMic),
            profile_actions: mock.clone(),
            mixer: Arc::new(crate::mixer::Mixer::new(
                Arc::new(nd_platform::NullAppAudio),
                Arc::new(nd_platform::NullAudio),
            )),
            obs: std::sync::Arc::new(crate::obs::NullObs),
            icons: None,
        };
        let src = vec![("a".to_string(), DataSource::AutoSwitchPaused)];
        assert_eq!(sample_all(&src, &p)[0].1.state_index, Some(0));
        mock.set_paused(PauseMode::Pause).unwrap();
        assert_eq!(sample_all(&src, &p)[0].1.state_index, Some(1));
    }

    #[test]
    fn profile_actions_fail_without_service_and_serde() {
        let (r, _, _) = make_test_router();
        assert!(r.execute(&Action::GoHome { pause_auto_switch: false }, &InputContext::default()).is_err());
        assert_eq!(
            serde_json::from_str::<Action>(r#"{"type":"go_home"}"#).unwrap(),
            Action::GoHome { pause_auto_switch: false }
        );
        assert_eq!(
            serde_json::from_str::<Action>(r#"{"type":"auto_switch_pause","mode":"resume"}"#).unwrap(),
            Action::AutoSwitchPause { mode: PauseMode::Resume }
        );
        assert_eq!(
            serde_json::from_str::<Action>(r#"{"type":"auto_switch_pause"}"#).unwrap(),
            Action::AutoSwitchPause { mode: PauseMode::Toggle }
        );
        assert_eq!(
            serde_json::from_str::<DataSource>(r#"{"type":"auto_switch_paused"}"#).unwrap(),
            DataSource::AutoSwitchPaused
        );
        let json = serde_json::to_string(&Action::GoHome { pause_auto_switch: true }).unwrap();
        assert!(json.contains("go_home") && json.contains("true"));
    }

    // ---- round 22: switch profile
    use crate::model::ProfileTarget;

    #[derive(Default)]
    struct SwitchMock {
        calls: Mutex<Vec<ProfileTarget>>,
    }
    impl ProfileActions for SwitchMock {
        fn go_home(&self, _: bool) -> std::result::Result<(), String> {
            Ok(())
        }
        fn set_paused(&self, _: PauseMode) -> std::result::Result<bool, String> {
            Ok(false)
        }
        fn paused(&self) -> bool {
            false
        }
        fn switch_profile(&self, to: &ProfileTarget) -> std::result::Result<(), String> {
            self.calls.lock().unwrap().push(to.clone());
            Ok(())
        }
        fn destination(&self, to: &ProfileTarget) -> Option<(String, bool)> {
            match to {
                ProfileTarget::Profile { id } => Some((format!("Name {id}"), id == "here")),
                ProfileTarget::Next => Some(("Next one".into(), false)),
                _ => None,
            }
        }
        fn current_profile_name(&self) -> Option<String> {
            Some("Current".into())
        }
    }

    #[test]
    fn switch_profile_json_router_and_sampling() {
        let targets = [
            (r#"{"by":"profile","id":"p_1"}"#, ProfileTarget::Profile { id: "p_1".into() }),
            (r#"{"by":"next"}"#, ProfileTarget::Next),
            (r#"{"by":"previous"}"#, ProfileTarget::Previous),
            (r#"{"by":"back"}"#, ProfileTarget::Back),
        ];
        for (json, t) in &targets {
            assert_eq!(&serde_json::from_str::<ProfileTarget>(json).unwrap(), t);
            let a = Action::SwitchProfile { to: t.clone() };
            let s = serde_json::to_string(&a).unwrap();
            assert!(s.contains("switch_profile"), "{s}");
            assert_eq!(serde_json::from_str::<Action>(&s).unwrap(), a);
            let src = DataSource::ProfileDestination { to: t.clone() };
            let s = serde_json::to_string(&src).unwrap();
            assert!(s.contains("profile_destination"), "{s}");
            assert_eq!(serde_json::from_str::<DataSource>(&s).unwrap(), src);
            let from_text = format!(r#"{{"type":"switch_profile","to":{json}}}"#);
            assert_eq!(serde_json::from_str::<Action>(&from_text).unwrap(), a);
        }
        assert_eq!(
            serde_json::from_str::<DataSource>(r#"{"type":"current_profile"}"#).unwrap(),
            DataSource::CurrentProfile
        );

        // router calls the service; without one it fails
        let (plain, _, _) = make_test_router();
        let a = Action::SwitchProfile { to: ProfileTarget::Next };
        assert!(plain.execute(&a, &InputContext::default()).is_err());
        let mock = Arc::new(SwitchMock::default());
        let (r, _, _) = make_test_router();
        let r = r.with_profile_actions(mock.clone());
        assert_eq!(r.execute(&a, &InputContext::default()).unwrap(), ActionOutcome::Executed);
        assert_eq!(*mock.calls.lock().unwrap(), vec![ProfileTarget::Next]);

        // sampling
        let p = LiveProviders {
            audio: Arc::new(nd_platform::NullAudio),
            media: Arc::new(nd_platform::NullMedia),
            stats: Arc::new(nd_platform::NullStats),
            camera: Arc::new(crate::NullCamera),
            mic: Arc::new(crate::NullMic),
            profile_actions: mock.clone(),
            mixer: Arc::new(crate::mixer::Mixer::new(
                Arc::new(nd_platform::NullAppAudio),
                Arc::new(nd_platform::NullAudio),
            )),
            obs: std::sync::Arc::new(crate::obs::NullObs),
            icons: None,
        };
        let src = vec![
            ("here".to_string(), DataSource::ProfileDestination { to: ProfileTarget::Profile { id: "here".into() } }),
            ("other".to_string(), DataSource::ProfileDestination { to: ProfileTarget::Profile { id: "x".into() } }),
            ("next".to_string(), DataSource::ProfileDestination { to: ProfileTarget::Next }),
            ("back".to_string(), DataSource::ProfileDestination { to: ProfileTarget::Back }),
            ("cur".to_string(), DataSource::CurrentProfile),
        ];
        let out = sample_all(&src, &p);
        let get = |id: &str| out.iter().find(|(i, _)| i == id).unwrap().1.clone();
        assert_eq!(get("here").subtitle.as_deref(), Some("Name here"));
        assert_eq!(get("here").state_index, Some(1));
        assert_eq!(get("here").dimmed, Some(false));
        assert_eq!(get("other").state_index, Some(0));
        assert_eq!(get("next").subtitle.as_deref(), Some("Next one"));
        assert_eq!(get("back").dimmed, Some(true));
        assert_eq!(get("back").subtitle.as_deref(), Some(""));
        assert_eq!(get("cur").title.as_deref(), Some("Current"));
    }
}

// ---------------------------------------------------------------- Round 12 (per-app mixer)

mod r12 {
    use super::*;
    use crate::live::{meter_targets, sample_all, sample_levels, LiveProviders, LiveSample, MeterTarget};
    use crate::mixer::{Mixer, MixerConfig};
    use crate::mock::{AppAudioCall, MockAppAudio, MockAudio};
    use crate::model::{AudioApp, DataSource};
    use nd_platform::{AudioTarget, NullMedia};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    fn name(n: &str) -> AudioApp {
        AudioApp::Name { name: n.into() }
    }

    fn slot(i: u32) -> AudioApp {
        AudioApp::Slot { index: i }
    }

    /// Mixer with a controllable clock (ms offset).
    fn mixer(backend: &MockAppAudio) -> (Arc<Mixer>, Arc<AtomicU64>) {
        let base = Instant::now();
        let ms = Arc::new(AtomicU64::new(0));
        let m2 = ms.clone();
        let m = Mixer::new(Arc::new(backend.clone()), Arc::new(MockAudio::new()))
            .with_clock(Arc::new(move || base + Duration::from_millis(m2.load(Ordering::SeqCst))));
        (Arc::new(m), ms)
    }

    fn basic() -> MockAppAudio {
        let b = MockAppAudio::new();
        b.set_sessions(vec![
            MockAppAudio::session("1", "chrome.exe", "Chrome", 0.4),
            MockAppAudio::session("2", "C:\\x\\Spotify.exe", "Spotify", 0.7),
            MockAppAudio::session("3", "chrome.exe", "", 0.9),
        ]);
        b
    }

    #[test]
    fn grouping_and_set() {
        let b = basic();
        let (m, _) = mixer(&b);
        assert!(!m.available());
        let apps = m.refresh().unwrap();
        assert!(m.available());
        assert_eq!(apps.len(), 2);
        let chrome = apps.iter().find(|a| a.key == "chrome").unwrap();
        assert_eq!(chrome.session_ids, vec!["1", "3"]);
        assert!((chrome.volume - 0.9).abs() < 1e-6);
        assert_eq!(chrome.display_name, "Chrome");
        assert!(!chrome.muted);
        // set applies to both
        assert_eq!(m.set_volume(&name("Chrome.EXE"), 0.25).unwrap(), 0.25);
        assert_eq!(b.get("1").unwrap().volume, 0.25);
        assert_eq!(b.get("3").unwrap().volume, 0.25);
        // mute: not all muted -> mute all; then unmute
        b.set_sessions(vec![
            MockAppAudio::session("1", "chrome.exe", "Chrome", 0.4),
            MockAppAudio::session("3", "chrome.exe", "", 0.9),
        ]);
        m.refresh().unwrap();
        b.clear_calls();
        assert!(m.toggle_mute(&name("chrome")).unwrap());
        assert_eq!(b.calls(), vec![AppAudioCall::Muted("1".into(), true), AppAudioCall::Muted("3".into(), true)]);
        m.refresh().unwrap();
        assert!(m.resolve(&name("chrome")).unwrap().muted);
        assert!(!m.toggle_mute(&name("chrome")).unwrap());
        // one of two muted -> not muted -> toggling mutes all
        let mut s = b.get("1").unwrap();
        s.muted = true;
        b.set_sessions(vec![s, MockAppAudio::session("3", "chrome.exe", "", 0.9)]);
        m.refresh().unwrap();
        assert!(!m.resolve(&name("chrome")).unwrap().muted);
        assert!(m.toggle_mute(&name("chrome")).unwrap());
    }

    #[test]
    fn adjust_clamps() {
        let b = basic();
        let (m, _) = mixer(&b);
        assert!((m.adjust(&name("spotify"), 0.1).unwrap() - 0.8).abs() < 1e-6);
        assert_eq!(m.adjust(&name("spotify"), 0.9).unwrap(), 1.0);
        assert_eq!(m.set_volume(&name("spotify"), -3.0).unwrap(), 0.0);
    }

    #[test]
    fn rapid_sets_build_on_each_other_within_cache_age() {
        let b = basic();
        let (m, _) = mixer(&b); // clock frozen: every call is within CACHE_MAX_AGE
        m.refresh().unwrap();
        for _ in 0..3 {
            m.adjust(&name("spotify"), -0.1).unwrap();
        }
        assert!((b.get("2").unwrap().volume - 0.4).abs() < 1e-5);
        assert!(m.toggle_mute(&name("spotify")).unwrap());
        assert!(!m.toggle_mute(&name("spotify")).unwrap());
        assert_eq!(b.list_calls(), 1);
    }

    #[test]
    fn hidden_system_and_order() {
        let b = MockAppAudio::new();
        let mut sys = MockAppAudio::session("s", "", "System sounds", 0.5);
        sys.is_system = true;
        b.set_sessions(vec![
            MockAppAudio::session("1", "a.exe", "Alpha", 0.1),
            sys,
            MockAppAudio::session("2", "b.exe", "Bravo", 0.2),
            MockAppAudio::session("3", "c.exe", "Charlie", 0.3),
        ]);
        let (m, _) = mixer(&b);
        let apps = m.refresh().unwrap();
        assert_eq!(apps.iter().map(|a| a.key.as_str()).collect::<Vec<_>>(), ["a", "b", "c", "system"]);
        assert!(apps[3].hidden && apps[3].slot.is_none() && apps[3].is_system);
        m.set_config(MixerConfig { order: vec!["c".into()], hidden: vec!["b".into()], show_system: true });
        let apps = m.apps();
        let keys: Vec<_> = apps.iter().map(|a| (a.key.as_str(), a.slot)).collect();
        assert_eq!(keys, [("c", Some(0)), ("a", Some(1)), ("system", Some(2)), ("b", None)]);
        // hidden still resolvable by name, but no slot 3
        assert!(m.resolve(&name("b")).is_some());
        assert!(m.resolve(&slot(3)).is_none());
        assert_eq!(m.resolve(&slot(2)).unwrap().key, "system");
    }

    #[test]
    fn first_seen_order_survives_absence() {
        let b = MockAppAudio::new();
        let s = |id: &str, p: &str| MockAppAudio::session(id, p, p, 0.5);
        b.set_sessions(vec![s("1", "a"), s("2", "b"), s("3", "c")]);
        let (m, ms) = mixer(&b);
        m.refresh().unwrap();
        b.set_sessions(vec![s("1", "a"), s("3", "c")]);
        m.refresh().unwrap();
        let slots: Vec<_> = m.apps().iter().map(|a| (a.key.clone(), a.slot)).collect();
        assert_eq!(slots, [("a".to_string(), Some(0)), ("c".to_string(), Some(1))]);
        // b returns, listed last by the backend, still keeps its place
        b.set_sessions(vec![s("1", "a"), s("3", "c"), s("2", "b")]);
        ms.fetch_add(1000, Ordering::SeqCst);
        let keys: Vec<_> = m.apps().iter().map(|a| a.key.clone()).collect();
        assert_eq!(keys, ["a", "b", "c"]);
        assert_eq!(m.resolve(&slot(1)).unwrap().key, "b");
    }

    #[test]
    fn focused_resolution() {
        let b = basic();
        let focus = Arc::new(std::sync::Mutex::new(Some("SPOTIFY.exe".to_string())));
        let f2 = focus.clone();
        let m = Mixer::new(Arc::new(b.clone()), Arc::new(MockAudio::new()))
            .with_focus(Arc::new(move || f2.lock().unwrap().clone()));
        assert_eq!(m.resolve(&AudioApp::Focused).unwrap().key, "spotify");
        *focus.lock().unwrap() = Some("notepad.exe".into());
        assert!(m.resolve(&AudioApp::Focused).is_none());
        let err = m.set_volume(&AudioApp::Focused, 0.5).unwrap_err().to_string();
        assert!(err.contains("no audio from the focused app"), "{err}");
        *focus.lock().unwrap() = None;
        assert!(m.resolve(&AudioApp::Focused).is_none());
    }

    #[test]
    fn cache_age_and_retry() {
        let b = basic();
        let (m, ms) = mixer(&b);
        m.apps();
        m.apps();
        m.resolve(&slot(0));
        assert_eq!(b.list_calls(), 1);
        ms.store(499, Ordering::SeqCst);
        m.apps();
        assert_eq!(b.list_calls(), 1);
        ms.store(500, Ordering::SeqCst);
        m.apps();
        assert_eq!(b.list_calls(), 2);

        // vanished session: cached snapshot knows session 2, the backend does not
        b.set_sessions(vec![MockAppAudio::session("9", "spotify.exe", "Spotify", 0.2)]);
        let before = b.list_calls();
        let v = m.set_volume(&name("spotify"), 0.6).unwrap();
        assert_eq!(v, 0.6);
        assert_eq!(b.list_calls(), before + 1);
        assert_eq!(b.get("9").unwrap().volume, 0.6);
        // unresolvable after refresh -> error
        b.set_sessions(vec![]);
        ms.fetch_add(1000, Ordering::SeqCst);
        let err = m.set_volume(&name("spotify"), 0.6).unwrap_err().to_string();
        assert!(err.contains("no audio from spotify"), "{err}");
        let err = m.toggle_mute(&slot(2)).unwrap_err().to_string();
        assert!(err.contains("mixer slot 3"), "{err}");
    }

    #[test]
    fn unavailable_backend() {
        let b = MockAppAudio::new();
        b.set_fail_list(true);
        let (m, _) = mixer(&b);
        assert!(m.refresh().is_err());
        assert!(!m.available());
        assert!(m.last_error().unwrap().contains("mock list failure"));
        assert!(m.apps().is_empty());
        b.set_fail_list(false);
        m.refresh().unwrap();
        assert!(m.available() && m.last_error().is_none());
    }

    #[test]
    fn set_app_by_key() {
        let b = basic();
        let (m, _) = mixer(&b);
        m.set_app("chrome", Some(0.5), Some(true)).unwrap();
        assert_eq!((b.get("1").unwrap().volume, b.get("3").unwrap().muted), (0.5, true));
        assert!(m.set_app("nope", None, None).is_err());
    }

    #[test]
    fn icon_cache_asks_once_including_negative() {
        let b = basic();
        b.set_icon("chrome.exe", b"png");
        let imports = Arc::new(AtomicU64::new(0));
        let i2 = imports.clone();
        let m = Mixer::new(Arc::new(b.clone()), Arc::new(MockAudio::new())).with_icons(Arc::new(move |bytes| {
            i2.fetch_add(1, Ordering::SeqCst);
            Some(format!("h{}", bytes.len()))
        }));
        let apps = m.refresh().unwrap();
        let chrome = apps.iter().find(|a| a.key == "chrome").unwrap();
        let spot = apps.iter().find(|a| a.key == "spotify").unwrap();
        assert_eq!(m.icon_hash(chrome).as_deref(), Some("h3"));
        assert_eq!(m.icon_hash(chrome).as_deref(), Some("h3"));
        assert_eq!(b.icon_calls(), 1);
        assert_eq!(m.icon_hash(spot), None);
        assert_eq!(m.icon_hash(spot), None);
        assert_eq!(b.icon_calls(), 2);
        assert_eq!(imports.load(Ordering::SeqCst), 1);
        let mut sys = chrome.clone();
        sys.is_system = true;
        sys.key = "system".into();
        assert_eq!(m.icon_hash(&sys), None);
        assert_eq!(b.icon_calls(), 2);
    }

    #[test]
    fn levels_batch_once() {
        let b = basic();
        b.set_peak("1", 0.2);
        b.set_peak("3", 0.6);
        let (m, _) = mixer(&b);
        m.refresh().unwrap();
        let lv = m.levels(&[name("chrome"), slot(1), name("zzz"), slot(0)]);
        assert_eq!(lv, vec![Some(0.6), None, None, Some(0.6)]);
        // spotify (id 2) has no peak entry -> None
        assert_eq!(b.peak_calls().len(), 1);
        assert_eq!(b.peak_calls()[0], vec!["1", "3", "2"]);
        // peaks unsupported
        let b2 = basic();
        let (m2, _) = mixer(&b2);
        m2.refresh().unwrap();
        assert_eq!(m2.levels(&[slot(0)]), vec![None]);
        assert!(b2.peak_calls().is_empty());
    }

    #[test]
    fn router_actions() {
        let b = basic();
        let (m, _) = mixer(&b);
        let router = ActionRouter::new(Arc::new(MockInputInjector::new()), Arc::new(MockLauncher::new())).with_mixer(m);
        let ctx = InputContext { delta: 2, value: 0.3, ..Default::default() };
        let out = router.execute(&Action::AdjustAppVolume { app: name("spotify"), step: 0.05 }, &ctx).unwrap();
        assert!(matches!(out, ActionOutcome::SetValue(v) if (v - 0.8).abs() < 1e-6));
        let out = router.execute(&Action::SetAppVolume { app: name("spotify"), level: None }, &ctx).unwrap();
        assert!(matches!(out, ActionOutcome::SetValue(v) if (v - 0.3).abs() < 1e-6));
        let out = router.execute(&Action::ToggleAppMute { app: name("spotify") }, &ctx).unwrap();
        assert!(matches!(out, ActionOutcome::SetState(1)));
        assert!(router.execute(&Action::ToggleAppMute { app: name("nope") }, &ctx).is_err());
    }

    fn providers(b: &MockAppAudio, audio: MockAudio) -> (LiveProviders, Arc<Mixer>) {
        let m = Arc::new(
            Mixer::new(Arc::new(b.clone()), Arc::new(audio.clone()))
                .with_icons(Arc::new(|bytes| Some(format!("ic{}", bytes.len())))),
        );
        let p = LiveProviders {
            audio: Arc::new(audio),
            media: Arc::new(NullMedia),
            stats: Arc::new(nd_platform::NullStats),
            camera: Arc::new(crate::NullCamera),
            mic: Arc::new(crate::NullMic),
            profile_actions: Arc::new(crate::autoswitch::NullProfileActions),
            mixer: m.clone(),
            obs: std::sync::Arc::new(crate::obs::NullObs),
            icons: None,
        };
        (p, m)
    }

    #[test]
    fn live_samples() {
        let b = basic();
        b.set_icon("chrome.exe", b"xx");
        b.set_has_peaks(true);
        let audio = MockAudio::new();
        audio.set_peak(Some(0.3));
        let (p, _) = providers(&b, audio);
        let src = vec![
            ("n".to_string(), DataSource::AppAudio { app: name("chrome") }),
            ("s".to_string(), DataSource::AppAudio { app: slot(0) }),
            ("u".to_string(), DataSource::AppAudio { app: name("zzz") }),
            ("us".to_string(), DataSource::AppAudio { app: slot(7) }),
            ("v".to_string(), DataSource::Volume { target: AudioTarget::Output }),
        ];
        let out = sample_all(&src, &p);
        let get = |id: &str| out.iter().find(|(i, _)| i == id).unwrap().1.clone();
        let n = get("n");
        assert_eq!((n.value, n.subtitle.as_deref(), n.state_index), (Some(0.9), Some("90%"), Some(0)));
        assert_eq!((n.dimmed, n.meter, n.title, n.icon_hash), (Some(false), Some(true), None, None));
        let s = get("s");
        assert_eq!((s.title.as_deref(), s.icon_hash.as_deref()), (Some("Chrome"), Some("ic2")));
        let u = get("u");
        assert_eq!((u.value, u.subtitle.as_deref(), u.dimmed, u.meter), (Some(0.0), Some(""), Some(true), Some(false)));
        assert_eq!((u.title, u.icon_hash), (None, None));
        let us = get("us");
        assert_eq!((us.title.as_deref(), us.icon_hash.as_deref(), us.state_index), (Some(""), Some(""), Some(0)));
        assert_eq!(get("v").meter, Some(true));
        // spotify slot has no icon -> empty string
        let (p2, _) = providers(&b, MockAudio::new());
        let o = sample_all(&[("x".into(), DataSource::AppAudio { app: slot(1) })], &p2);
        assert_eq!(o[0].1.icon_hash.as_deref(), Some(""));
        assert_eq!(
            sample_all(&[("v".into(), DataSource::Volume { target: AudioTarget::Output })], &p2)[0].1.meter,
            Some(false)
        );
    }

    #[test]
    fn meter_targets_and_levels() {
        let b = basic();
        let audio = MockAudio::new();
        let (p, m) = providers(&b, audio.clone());
        let src = vec![
            ("a".to_string(), DataSource::AppAudio { app: name("chrome") }),
            ("b".to_string(), DataSource::AppAudio { app: slot(1) }),
            ("v".to_string(), DataSource::Volume { target: AudioTarget::Output }),
            ("c".to_string(), DataSource::Cpu),
        ];
        assert!(meter_targets(&src, &p).is_empty());
        b.set_peak("1", 0.5);
        b.set_peak("2", 0.333);
        audio.set_peak(Some(0.81));
        let t = meter_targets(&src, &p);
        assert_eq!(
            t,
            vec![
                ("a".to_string(), MeterTarget::App(name("chrome"))),
                ("b".to_string(), MeterTarget::App(slot(1))),
                ("v".to_string(), MeterTarget::Endpoint(AudioTarget::Output)),
            ]
        );
        m.refresh().unwrap();
        let mut t = t;
        t.push(("z".into(), MeterTarget::App(name("zzz"))));
        let lv = sample_levels(&t, &p);
        assert_eq!(lv, vec![("a".into(), 0.5), ("b".into(), 0.34), ("v".into(), 0.82), ("z".into(), 0.0)]);
        assert_eq!(b.peak_calls().len(), 1);
    }

    fn meter_session() -> DeckSession {
        let mut k = Control::slider("m", 0, 0, 1, 3, false);
        k.meter = true;
        let plain = Control::slider("p", 1, 0, 1, 3, false);
        let mut page = Page::new("p", "P");
        page.add_control(k);
        page.add_control(plain);
        let mut profile = Profile::new("t", "T");
        profile.add_page(page);
        DeckSession::new(profile)
    }

    #[test]
    fn apply_levels_thresholds() {
        let mut s = meter_session();
        let lv = |v: f32| vec![("m".to_string(), v), ("p".to_string(), v)];
        let u = s.apply_levels(&lv(0.5)).unwrap();
        assert_eq!(u.patches.len(), 1);
        assert_eq!(u.patches[0].fields, vec!["level"]);
        assert_eq!(u.patches[0].control.as_ref().unwrap().level, 0.5);
        assert!(s.apply_levels(&lv(0.51)).is_none());
        assert!(s.apply_levels(&lv(0.52)).is_some());
        assert!(s.apply_levels(&lv(0.0)).is_some());
        assert!(s.apply_levels(&lv(0.0)).is_none());
        // tiny rise from 0 is below the threshold
        assert!(s.apply_levels(&lv(0.01)).is_none());
        assert!(s.apply_levels_rail(&lv(0.5)).is_none());
    }

    #[test]
    fn live_apply_new_fields_and_meter_off() {
        let mut s = meter_session();
        let sample =
            LiveSample { icon_hash: Some("abc".into()), meter: Some(true), dimmed: Some(true), ..Default::default() };
        let u = s.apply_live(&[("p".into(), sample.clone())]).unwrap();
        assert_eq!(u.patches[0].fields, vec!["icon_hash", "meter", "dimmed"]);
        let c = u.patches[0].control.as_ref().unwrap();
        assert_eq!((c.icon_hash.as_str(), c.meter, c.dimmed), ("abc", true, true));
        assert!(s.apply_live(&[("p".into(), sample)]).is_none());
        // meter off zeroes level
        s.apply_levels(&[("p".into(), 0.7)]).unwrap();
        let u = s.apply_live(&[("p".into(), LiveSample { meter: Some(false), ..Default::default() })]).unwrap();
        assert_eq!(u.patches[0].fields, vec!["meter", "level"]);
        assert_eq!(u.patches[0].control.as_ref().unwrap().level, 0.0);
        // state visuals override only where set
        let mut c = Control::key("k", 0, 0, "x");
        c.states = vec![ControlStateVisual { title: Some("M".into()), ..Default::default() }];
        let mut page = Page::new("p", "P");
        page.add_control(c);
        let mut profile = Profile::new("t", "T");
        profile.add_page(page);
        let mut s = DeckSession::new(profile);
        let u = s
            .apply_live(&[(
                "k".into(),
                LiveSample { icon_hash: Some("i".into()), title: Some("T".into()), ..Default::default() },
            )])
            .unwrap();
        let c = u.patches[0].control.as_ref().unwrap();
        assert_eq!((c.icon_hash.as_str(), c.title.as_str()), ("i", "M"));
    }

    #[test]
    fn serde_roundtrip() {
        let a: Action = serde_json::from_str(r#"{"type":"set_app_volume","app":{"by":"slot","index":0}}"#).unwrap();
        assert_eq!(a, Action::SetAppVolume { app: slot(0), level: None });
        for a in [
            Action::AdjustAppVolume { app: AudioApp::Focused, step: 0.05 },
            Action::SetAppVolume { app: name("spotify"), level: Some(0.5) },
            Action::ToggleAppMute { app: slot(2) },
        ] {
            let j = serde_json::to_string(&a).unwrap();
            assert_eq!(serde_json::from_str::<Action>(&j).unwrap(), a);
        }
        let s = DataSource::AppAudio { app: name("discord") };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("app_audio"), "{j}");
        assert_eq!(serde_json::from_str::<DataSource>(&j).unwrap(), s);
    }
}
