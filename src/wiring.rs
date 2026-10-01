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
    fn a_later_port_pads_the_ones_before_it() {
        let mut nodes = vec![node("n1", "x", vec![])];
        assert!(connect(&mut nodes, "n1", "y", 1));
        assert_eq!(node_wires(&nodes[0]), ["", "y"]);
    }
}
