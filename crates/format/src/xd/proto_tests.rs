//! `.xd` prototype links: triggers, actions, transitions, overlays and where prototypes start.

use figcraft_doc::NodeKind;
use figcraft_doc::proto::{Action, Direction, Easing, OverlayPosition, Transition, Trigger};
use serde_json::{Value, json};

use super::fixture::*;
use super::*;

/// Two artboards, `Home` (with a button `Btn` at 20, 30) and `Next`, and `interactions`.
fn file(interactions: Value) -> (Document, Vec<String>) {
    let boards = [
        Board { id: "HOME", name: "Home", rect: (0.0, 0.0, 400.0, 300.0), layers: vec![rect("Btn", 20.0, 30.0, 100.0, 40.0)] },
        Board { id: "NEXT", name: "Next", rect: (500.0, 0.0, 400.0, 300.0), layers: vec![rect("Target", 520.0, 200.0, 10.0, 10.0)] },
    ];
    let z = with_member(&xd_zip(&boards, vec![], vec![], &[]), "interactions/interactions.json", interactions.to_string().as_bytes());
    import(&z, "f").unwrap()
}

fn node<'d>(doc: &'d Document, name: &str) -> &'d figcraft_doc::Node {
    doc.nodes().find(|n| n.name == name).unwrap_or_else(|| panic!("no layer {name}"))
}

fn on_button(action: &str, props: Value, trigger: &str) -> Value {
    json!({"version": "0.24", "interactions": {"Btn": [{"id": "i1", "triggerEvent": trigger, "action": action, "properties": props}]}})
}

#[test]
fn taps_navigate_with_their_transition() {
    let (doc, warnings) = file(on_button(
        "artboard-transition",
        json!({"transition": "slide-left", "duration": 0.4, "easing": "linear", "destination": "NEXT", "preserveScrollPosition": true}),
        "tap",
    ));
    let btn = node(&doc, "Btn");
    let it = btn.interactions.first().unwrap_or_else(|| panic!("{warnings:?}"));
    assert_eq!(it.trigger, Trigger::OnClick);
    assert_eq!(it.actions, [Action::Navigate { dest: node(&doc, "Next").id, preserve_scroll: true }]);
    assert_eq!(it.transition, Transition::SlideIn { direction: Direction::Left });
    assert_eq!((it.duration_ms, it.easing), (400, Easing::Linear));
}

#[test]
fn auto_animate_back_scroll_and_links() {
    let (doc, _) = file(json!({"version": "0.24", "interactions": {
        "Btn": [
            {"id": "a", "triggerEvent": "hover", "action": "auto-animate", "properties": {"transition": "dissolve", "duration": 0.3, "easing": "ease-out", "destination": "NEXT"}},
            {"id": "b", "triggerEvent": "tap", "action": "previous-artboard", "properties": {}},
            {"id": "c", "triggerEvent": "drag", "action": "scroll-transition", "properties": {"transition": null, "duration": 0.3, "easing": "ease-out", "destination": "Target"}},
            {"id": "d", "triggerEvent": "tap", "action": "hyperlink", "properties": {"hyperlinkUrl": "https://example.com"}},
        ]
    }}));
    let its = &node(&doc, "Btn").interactions;
    assert_eq!(its.len(), 4);
    assert_eq!((its[0].trigger.clone(), its[0].transition), (Trigger::OnHover, Transition::SmartAnimate));
    assert_eq!(its[1].actions, [Action::Back]);
    assert!(matches!(its[2].actions[0], Action::ScrollTo { dest, .. } if dest == node(&doc, "Target").id));
    assert_eq!(its[2].trigger, Trigger::OnDrag);
    assert!(matches!(&its[3].actions[0], Action::OpenUrl { url, .. } if url == "https://example.com"));
}

#[test]
fn timers_and_keys() {
    let (doc, _) = file(json!({"version": "0.24", "interactions": {"Btn": [
        {"id": "t", "triggerEvent": "time", "action": "artboard-transition", "properties": {"delay": 1.5, "transition": "none", "destination": "NEXT"}},
        {"id": "k", "triggerEvent": "keys-gamepad", "action": "artboard-transition",
            "properties": {"keysGamepadDevice": "keyboard", "keysGamepad": "\u{21a9}", "transition": "dissolve", "duration": 0.3, "destination": "NEXT"}},
    ]}}));
    let its = &node(&doc, "Btn").interactions;
    assert_eq!(its[0].trigger, Trigger::AfterDelay { ms: 1500 });
    assert_eq!(its[0].transition, Transition::Instant);
    assert!(matches!(&its[1].trigger, Trigger::KeyPress { key } if key == "Enter"), "{:?}", its[1].trigger);
}

#[test]
fn overlays_sit_where_xd_put_them() {
    // XD opens the overlay at 50, 60 on the Home artboard; the button is at 20, 30.
    let (doc, _) = file(on_button(
        "overlay-transition",
        json!({"transition": "dissolve", "duration": 0.3, "easing": "ease-out", "destination": "NEXT", "overlayPosition": {"x": 50, "y": 60}}),
        "tap",
    ));
    let it = &node(&doc, "Btn").interactions[0];
    let Action::OpenOverlay { dest, position, offset, .. } = &it.actions[0] else { panic!("{:?}", it.actions) };
    assert_eq!(*dest, node(&doc, "Next").id);
    assert_eq!(*position, OverlayPosition::Manual);
    assert_eq!(*offset, Some([30.0, 30.0]));
}

#[test]
fn state_changes_and_sounds_are_noted() {
    let (doc, warnings) = file(json!({"version": "0.24", "interactions": {"Btn": [
        {"id": "s", "triggerEvent": "tap", "action": "state-transition", "properties": {"destination": "STATE"}},
        {"id": "a", "triggerEvent": "tap", "action": "audio-file-playback", "properties": {}},
    ]}}));
    assert!(node(&doc, "Btn").interactions.is_empty());
    assert!(warnings.iter().any(|w| w.contains("state")), "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("sounds")), "{warnings:?}");
}

#[test]
fn the_home_artboard_and_named_flows_start_prototypes() {
    let (doc, _) = file(json!({"version": "0.24", "homeArtboard": "NEXT", "interactions": {}}));
    let page = doc.node(doc.first_page().unwrap()).unwrap();
    let NodeKind::Page { flows, .. } = &page.kind else { panic!() };
    assert_eq!(flows.len(), 1);
    assert_eq!((flows[0].name.as_str(), flows[0].start), ("Next", node(&doc, "Next").id));
}

#[test]
fn junk_interactions_are_skipped() {
    let (doc, _) = file(json!({"version": "0.24", "homeArtboard": "../x", "interactions": {
        "Btn": [1, null, {"triggerEvent": 5}, {"triggerEvent": "tap", "action": "artboard-transition"},
            {"triggerEvent": "tap", "action": "artboard-transition", "properties": {"destination": "NOWHERE", "duration": 1e300}},
            {"triggerEvent": "keys-gamepad", "action": "previous-artboard", "properties": {"keysGamepad": "\u{2318}"}}],
        "Nobody": "x"
    }}));
    assert!(node(&doc, "Btn").interactions.is_empty());
}
