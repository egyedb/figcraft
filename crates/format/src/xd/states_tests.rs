//! `.xd` component states: a component with states becomes a component set of `State=` variants,
//! instances name the variant of the state they show, and state-change links change to it.

use figcraft_doc::NodeKind;
use figcraft_doc::geom::Point;
use figcraft_doc::proto::Action;
use serde_json::{Value, json};

use super::fixture::*;
use super::*;

/// A checkbox component `S`: its default state an empty box, its state `on` (id `ON`) the box
/// with a tick, and a state named `Default, again` (id `D2`) to test names.
fn checkbox_def() -> Value {
    let boxed = |id: &str| json!({"type": "shape", "id": id, "name": "Box", "style": {"fill": rgb(255, 255, 255)}, "shape": {"type": "rect", "x": 0, "y": 0, "width": 20, "height": 20}});
    let tick = json!({"type": "group", "id": "TICKG", "name": "Tick", "transform": translate(4.0, 4.0), "group": {"children": [
        {"type": "shape", "id": "TICK", "name": "Mark", "style": {"fill": rgb(0, 0, 0)}, "shape": {"type": "rect", "x": 2, "y": 2, "width": 8, "height": 8}}
    ]}});
    json!({"type": "group", "id": "DEF", "name": "Checkbox", "meta": {"ux": {"symbolId": "S", "stateId": "S", "states": [
        {"type": "group", "id": "ON", "name": "on", "meta": {"ux": {"symbolId": "S", "stateId": "ON"}}, "group": {"children": [boxed("BOX-ON"), tick]}},
        {"type": "group", "id": "D2", "name": "Default, again", "meta": {"ux": {"symbolId": "S", "stateId": "D2"}}, "group": {"children": [boxed("BOX-D2")]}}
    ]}}, "group": {"children": [boxed("BOX")]}})
}

/// A group of component `S` at `x`, 10 showing state `state`; its main when `master`.
fn checkbox(id: &str, x: f64, state: &str, master: bool, child: Value) -> Value {
    json!({"type": "group", "id": id, "name": id, "transform": translate(x, 10.0),
        "meta": {"ux": {"symbolId": "S", "isMaster": master, "stateId": state}}, "group": {"children": [child]}})
}

fn import_board(layers: Vec<Value>, interactions: Option<Value>) -> (Document, Vec<String>) {
    let z = xd_zip(&[board("Board", (0.0, 0.0, 800.0, 600.0), layers)], vec![], vec![checkbox_def()], &[]);
    let z = match interactions {
        Some(i) => with_member(&z, "interactions/interactions.json", i.to_string().as_bytes()),
        None => z,
    };
    import(&z, "f").unwrap()
}

fn node<'d>(doc: &'d Document, name: &str) -> &'d figcraft_doc::Node {
    doc.nodes().find(|n| n.name == name).unwrap_or_else(|| panic!("no layer {name}: {:?}", doc.nodes().map(|n| &n.name).collect::<Vec<_>>()))
}

fn at(doc: &Document, id: figcraft_doc::NodeId) -> Point {
    doc.world_transform(id) * Point::ZERO
}

#[test]
fn a_main_with_states_becomes_a_set_of_state_variants() {
    let reference = |guid: &str| json!({"type": "syncRef", "syncSourceGuid": "BOX", "guid": guid});
    let on = json!({"type": "syncRef", "syncSourceGuid": "BOX-ON", "guid": "g2"});
    let layers = vec![
        checkbox("Main", 100.0, "S", true, reference("g0")),
        checkbox("Off", 0.0, "S", false, reference("g1")),
        checkbox("Checked", 50.0, "ON", false, on),
    ];
    let (doc, warnings) = import_board(layers, None);
    let set = doc.nodes().find(|n| matches!(n.kind, NodeKind::ComponentSet { .. })).unwrap_or_else(|| panic!("no set: {warnings:?}"));
    // On the Components page, where the variants have room.
    assert_eq!(doc.page_of(set.id).and_then(|p| doc.node(p)).map(|p| p.name.as_str()), Some("Components"));
    assert_eq!(set.name, "Checkbox");
    let names: Vec<&str> = set.children.iter().map(|c| doc.node(*c).unwrap().name.as_str()).collect();
    assert_eq!(names, ["State=Default", "State=on", "State=Default again"]);
    // In a row, 40 px apart.
    let (d, o) = (node(&doc, "State=Default"), node(&doc, "State=on"));
    assert!((at(&doc, o.id).x - (at(&doc, d.id).x + d.width + 40.0)).abs() < 1e-6);
    // The main on the canvas stays where it was, as an instance of the default variant.
    assert_eq!(at(&doc, node(&doc, "Main").id), Point::new(100.0, 10.0));
    // Instances name the variant of the state they show.
    let comp = |n: &str| match node(&doc, n).kind {
        NodeKind::Instance { component, .. } => doc.node(component).unwrap().name.clone(),
        ref k => panic!("{n} is {k:?}"),
    };
    assert_eq!(comp("Main"), "State=Default");
    assert_eq!(comp("Off"), "State=Default");
    assert_eq!(comp("Checked"), "State=on");
    assert!(!warnings.iter().any(|w| w.contains("states are not supported")), "{warnings:?}");
}

#[test]
fn state_change_links_change_to_the_variant() {
    let reference = json!({"type": "syncRef", "syncSourceGuid": "BOX", "guid": "g1"});
    let interactions = json!({"version": "0.24", "interactions": {"Inst": [
        {"id": "a", "triggerEvent": "tap", "action": "state-transition", "properties": {"transition": "auto-animate", "duration": 0.3, "easing": "ease-out", "destination": "ON"}},
        {"id": "b", "triggerEvent": "hover", "action": "state-transition", "properties": {"transition": "dissolve", "duration": 0.2, "easing": "ease-out", "destination": "S"}},
    ]}});
    let (doc, warnings) = import_board(vec![checkbox("Inst", 0.0, "S", false, reference)], Some(interactions));
    let its = &node(&doc, "Inst").interactions;
    assert_eq!(its.len(), 2, "{warnings:?}");
    assert_eq!(its[0].actions, [Action::ChangeTo { dest: node(&doc, "State=on").id }]);
    // A component's own id is its default state.
    assert_eq!(its[1].actions, [Action::ChangeTo { dest: node(&doc, "State=Default").id }]);
}

#[test]
fn library_components_with_states_get_a_set_on_the_components_page() {
    let on = json!({"type": "syncRef", "syncSourceGuid": "BOX-ON", "guid": "g2"});
    let (doc, _) = import_board(vec![checkbox("Checked", 0.0, "ON", false, on)], None);
    let set = doc.nodes().find(|n| matches!(n.kind, NodeKind::ComponentSet { .. })).unwrap();
    let page = doc.page_of(set.id).and_then(|p| doc.node(p)).map(|p| p.name.clone());
    assert_eq!(page.as_deref(), Some("Components"));
    assert_eq!(set.name, "Checkbox");
    // The tick sits where the state draws it inside its variant, its group moved with the row.
    let on = node(&doc, "State=on");
    let mark = doc.nodes().find(|n| n.name == "Mark" && doc.is_ancestor(on.id, n.id)).unwrap();
    assert_eq!(at(&doc, mark.id) - at(&doc, on.id), figcraft_doc::geom::Vec2::new(6.0, 6.0));
    let NodeKind::Instance { component, .. } = node(&doc, "Checked").kind else { panic!() };
    assert_eq!(component, on.id);
}
