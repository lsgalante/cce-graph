//! Node identity and mouse-drawn wires for the project editor.
//!
//! The `Graph` widget reports a wire drawn with the mouse as a pending
//! connection naming the downstream node by `id` and the upstream one by
//! `name`, and draws wires from parameters (`cce_ui::widget::node_wires`):
//! a node's `node`-typed parameters, one per input port in order, else its
//! parameter named `input`. Two things were missing, so every drawn wire
//! was dropped: nodes loaded and added with an empty id (the pending
//! connection could not say which node), and nothing took the pending
//! connection and wrote it into a parameter.

use cce_ui::widget::GraphNode;

/// Give every node without one a unique session id. Ids are not saved:
/// wires reference names, so a fresh id per load is enough.
pub fn ensure_ids(nodes: &mut [GraphNode]) {
    let mut taken: std::collections::HashSet<String> =
        nodes.iter().filter(|n| !n.id.is_empty()).map(|n| n.id.clone()).collect();
    let mut next = 1usize;
    for node in nodes.iter_mut().filter(|n| n.id.is_empty()) {
        while taken.contains(&format!("n{next}")) {
            next += 1;
        }
        node.id = format!("n{next}");
        taken.insert(node.id.clone());
    }
}

/// A name no node has yet: `Node {n}`, counting up from one past the node
/// count. Wires reference names, so two nodes sharing one share every wire
/// to either — and deleting one cut the other's.
pub fn fresh_name(nodes: &[GraphNode]) -> String {
    let taken: std::collections::HashSet<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    (nodes.len() + 1..).map(|n| format!("Node {n}")).find(|name| !taken.contains(name.as_str())).unwrap()
}

/// Cut every wire from the node named `name`: blank the parameters
/// `node_wires` reads as wires (the `node`-typed ones, else the one named
/// `input`) where they name it. Other parameters are left alone, even when
/// their value happens to equal the name.
pub fn disconnect(nodes: &mut [GraphNode], name: &str) {
    for node in nodes {
        let any_typed = node.parameters.iter().any(|p| p.2 == "node");
        let untyped_input = node.parameters.iter().position(|p| p.0.eq_ignore_ascii_case("input"));
        for (i, p) in node.parameters.iter_mut().enumerate() {
            let is_wire = if any_typed { p.2 == "node" } else { Some(i) == untyped_input };
            if is_wire && p.1.trim() == name {
                p.1 = String::new();
            }
        }
    }
}

/// Wire `from` (a node name) into input `port` of the node with id `to`.
/// Writes the value of that port's parameter — the port-th `node`-typed
/// one, or the `input` parameter for port 0 of a node that has no typed
/// ones — and adds the parameter when the node has none for that port.
/// False when no node has that id, or the wire would loop onto itself.
pub fn connect(nodes: &mut [GraphNode], to: &str, from: &str, port: usize) -> bool {
    let Some(node) = nodes.iter_mut().find(|n| n.id == to) else { return false };
    if node.name == from {
        return false;
    }
    let is_typed = |p: &(String, String, String)| p.2 == "node";
    let untyped_input = node.parameters.iter().position(|p| p.0.eq_ignore_ascii_case("input"));
    if !node.parameters.iter().any(is_typed) {
        if let Some(i) = untyped_input {
            if port == 0 {
                node.parameters[i].1 = from.to_string();
                return true;
            }
            // A later port on a node whose only wire is an untyped
            // `input`: type that one, so it stays port 0.
            node.parameters[i].2 = "node".into();
        }
    }
    let typed: Vec<usize> =
        node.parameters.iter().enumerate().filter(|(_, p)| is_typed(p)).map(|(i, _)| i).collect();
    if let Some(&i) = typed.get(port) {
        node.parameters[i].1 = from.to_string();
        return true;
    }
    // Typed parameters are read in order, so pad up to this port.
    for k in typed.len()..=port {
        let name = if k == 0 { "input".to_string() } else { format!("input{}", k + 1) };
        let value = if k == port { from.to_string() } else { String::new() };
        node.parameters.push((name, value, "node".into()));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use cce_ui::widget::node_wires;

    fn node(id: &str, name: &str, params: Vec<(&str, &str, &str)>) -> GraphNode {
        GraphNode {
            id: id.into(),
            name: name.into(),
            position: (0.0, 0.0),
            parameters: params.into_iter().map(|(a, b, c)| (a.into(), b.into(), c.into())).collect(),
            geom_visible: true,
            node_type: String::new(),
            inputs: 2,
            outputs: 1,
        }
    }

    #[test]
    fn ids_are_unique_and_kept() {
        let mut nodes = vec![node("", "a", vec![]), node("n1", "b", vec![]), node("", "c", vec![])];
        ensure_ids(&mut nodes);
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["n2", "n1", "n3"]);
    }

    #[test]
    fn a_drawn_wire_becomes_the_port_parameter() {
        let mut nodes = vec![node("n1", "a", vec![]), node("n2", "b", vec![])];
        assert!(connect(&mut nodes, "n2", "a", 0));
        assert_eq!(node_wires(&nodes[1]), ["a"]);
        // Port 1 pads nothing (port 0 exists) and adds the second input.
        assert!(connect(&mut nodes, "n2", "a", 1));
        assert_eq!(node_wires(&nodes[1]), ["a", "a"]);
        // Rewiring port 0 replaces it.
        nodes.push(node("n3", "c", vec![]));
        assert!(connect(&mut nodes, "n2", "c", 0));
        assert_eq!(node_wires(&nodes[1]), ["c", "a"]);
        assert!(!connect(&mut nodes, "n9", "a", 0));
        assert!(!connect(&mut nodes, "n1", "a", 0), "no self-loop");
    }

    #[test]
    fn an_untyped_input_parameter_keeps_working() {
        let mut nodes = vec![node("n1", "x", vec![("Input", "old", "string")])];
        assert!(connect(&mut nodes, "n1", "y", 0));
        assert_eq!(node_wires(&nodes[0]), ["y"]);
        assert!(connect(&mut nodes, "n1", "z", 1));
        assert_eq!(node_wires(&nodes[0]), ["y", "z"]);
    }

    #[test]
    fn fresh_names_skip_ones_in_use() {
        // After "Node 2" of three was deleted, len + 1 is "Node 3" — taken.
        let nodes = vec![node("n1", "Node 1", vec![]), node("n3", "Node 3", vec![])];
        assert_eq!(fresh_name(&nodes), "Node 4");
        assert_eq!(fresh_name(&[]), "Node 1");
    }

    #[test]
    fn disconnect_cuts_wires_only() {
        let mut nodes = vec![
            // Typed wires: both cut; a string that equals the name is kept.
            node("n1", "x", vec![("input", "a", "node"), ("input2", " a ", "node"), ("label", "a", "string")]),
            // Untyped: the `input` parameter is the wire.
            node("n2", "y", vec![("Input", "a", "string"), ("note", "a", "string")]),
            node("n3", "z", vec![("input", "b", "node")]),
        ];
        disconnect(&mut nodes, "a");
        assert_eq!(node_wires(&nodes[0]), ["", ""]);
        assert_eq!(nodes[0].parameters[2].1, "a");
        assert_eq!(node_wires(&nodes[1]), [""]);
        assert_eq!(nodes[1].parameters[1].1, "a");
        assert_eq!(node_wires(&nodes[2]), ["b"]);
    }

    #[test]
    fn a_later_port_pads_the_ones_before_it() {
        let mut nodes = vec![node("n1", "x", vec![])];
        assert!(connect(&mut nodes, "n1", "y", 1));
        assert_eq!(node_wires(&nodes[0]), ["", "y"]);
    }
}
