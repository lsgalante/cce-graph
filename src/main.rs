use wayland_client::QueueHandle;
use glyphon::{FontSystem, Buffer, Metrics, Attrs};
use clear_ui::engine::{Application, EngineState, LogicalPosition, LogicalSize, WindowSettings};
use clear_ui::widget::{MouseButton, ElementState, MouseScrollDelta, KeyEvent, TextItem, Widget, Graph, GraphNode};

struct GraphApp {
    graph: Graph,
    text_items: Vec<TextItem>,
    font_system: FontSystem,
    needs_rebuild: bool,
    width: u32,
    height: u32,
    scale_factor: f64,
}

impl GraphApp {
    fn rebuild_text_items(&mut self) {
        self.text_items.clear();
        let labels = self.graph.text_labels();
        for label in labels {
            let metrics = Metrics::new(label.font_size, label.font_size * 1.4);
            let mut buf = Buffer::new(&mut self.font_system, metrics);
            buf.set_text(&mut self.font_system, &label.text, Attrs::new(), glyphon::Shaping::Advanced);
            buf.shape_until_scroll(&mut self.font_system, true);
            self.text_items.push(TextItem {
                buffer: buf,
                x: label.x,
                y: label.y,
                color: glyphon::Color::rgb(label.color[0], label.color[1], label.color[2]),
            });
        }
    }
}

impl Application for GraphApp {
    type Message = ();

    fn new(_qh: &QueueHandle<EngineState<Self>>, _sender: calloop::channel::Sender<Self::Message>) -> Self {
        let mut graph = Graph::new();
        
        // Define some graph nodes
        let nodes = vec![
            GraphNode {
                name: "Data Source".to_string(),
                position: (1.0, 1.0),
                parameters: vec![],
                geom_visible: true,
            },
            GraphNode {
                name: "Filter".to_string(),
                position: (3.0, 1.0),
                parameters: vec![("input".to_string(), "Data Source".to_string(), "string".to_string())],
                geom_visible: true,
            },
            GraphNode {
                name: "Render Output".to_string(),
                position: (5.0, 2.0),
                parameters: vec![("input".to_string(), "Filter".to_string(), "string".to_string())],
                geom_visible: true,
            },
        ];
        graph.set_nodes(&nodes);
        
        // Configure grid settings
        graph.set_show_network_grid(true);
        graph.set_grid_sizes(140.0, 70.0);
        graph.set_skipped_sizes(35.0, 35.0);
        graph.set_grid_origin(60.0, 60.0);
        graph.set_grid_snap_enabled(true);

        let mut app = Self {
            graph,
            text_items: Vec::new(),
            font_system: FontSystem::new(),
            needs_rebuild: true,
            width: 1024,
            height: 768,
            scale_factor: 1.0,
        };
        app.graph.set_rect(0.0, 0.0, 1024.0, 768.0);
        app.rebuild_text_items();
        app
    }

    fn settings(&self) -> WindowSettings {
        WindowSettings {
            title: "Clear Graph".to_string(),
            app_id: "clear-graph".to_string(),
            width: 1024,
            height: 768,
            fullscreen: false,
            min_size: Some((800, 600)),
        }
    }

    fn update(&mut self, _msg: Self::Message, _needs_rebuild: &mut bool, _exit: &mut bool) {}

    fn tick(&mut self, _dt: f32, _needs_rebuild: &mut bool) {}

    fn view(&mut self, quads: &mut Vec<(f32, f32, f32, f32, [f32; 4])>, size: LogicalSize, scale: f64) {
        let size_changed = self.width != size.width as u32 || self.height != size.height as u32 || self.scale_factor != scale;
        if self.needs_rebuild || size_changed {
            self.width = size.width as u32;
            self.height = size.height as u32;
            self.scale_factor = scale;
            self.graph.set_rect(0.0, 0.0, size.width, size.height);
            self.rebuild_text_items();
            self.needs_rebuild = false;
        }

        // 1. Background quad
        quads.push((0.0, 0.0, self.width as f32, self.height as f32, [0.08, 0.08, 0.10, 1.0]));

        // 2. Add extra quads from the Graph widget (nodes, wires, toggles, grid)
        quads.extend(self.graph.extra_quads());
    }

    fn text_items(&self) -> &[TextItem] {
        &self.text_items
    }

    fn handle_pointer_move(&mut self, pos: LogicalPosition, needs_rebuild: &mut bool) {
        let mut changed = false;
        if self.graph.is_dragging() {
            if self.graph.drag_update(pos.x, pos.y) {
                changed = true;
            }
        } else {
            if self.graph.on_cursor_moved(pos.x, pos.y) {
                changed = true;
            }
        }
        if changed {
            *needs_rebuild = true;
            self.needs_rebuild = true;
        }
    }

    fn handle_mouse_input(&mut self, button: MouseButton, state: ElementState, pos: LogicalPosition, needs_rebuild: &mut bool) -> Option<Self::Message> {
        let mut changed = false;
        if button == MouseButton::Left {
            if state == ElementState::Pressed {
                if self.graph.mouse_input(button, state, pos.x, pos.y) {
                    if self.graph.is_dragging() {
                        self.graph.drag_begin(pos.x, pos.y);
                    }
                    changed = true;
                }
            } else if state == ElementState::Released {
                if self.graph.is_dragging() {
                    self.graph.drag_end();
                    changed = true;
                } else {
                    if self.graph.mouse_input(button, state, pos.x, pos.y) {
                        changed = true;
                    }
                }
            }
        }
        if changed {
            *needs_rebuild = true;
            self.needs_rebuild = true;
        }
        None
    }

    fn handle_mouse_wheel(&mut self, _delta: &MouseScrollDelta, _pos: LogicalPosition, _needs_rebuild: &mut bool) {}

    fn handle_key_input(&mut self, _event: &KeyEvent, _needs_rebuild: &mut bool) -> Option<Self::Message> {
        None
    }
}

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _guard = rt.enter();

    clear_ui::engine::run::<GraphApp>();
}
