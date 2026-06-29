use wayland_client::QueueHandle;
use glyphon::{FontSystem, Buffer, Metrics, Attrs};
use cce_ui::engine::{Application, EngineState, LogicalPosition, LogicalSize, WindowSettings};
use cce_ui::widget::{MouseButton, ElementState, MouseScrollDelta, KeyEvent, TextItem, Element, Graph, GraphNode, MenuBar, MenuController, GraphController};

#[derive(Debug, Clone)]
enum AppMessage {
    Exit,
    ToggleGrid,
    ToggleUniformBackground,
    SetOpacity95,
    SetOpacity75,
    SetOpacity50,
    AddNode,
}

struct GraphApp {
    menu_bar: MenuBar,
    graph: Graph,
    text_items: Vec<TextItem>,
    font_system: FontSystem,
    needs_rebuild: bool,
    width: u32,
    height: u32,
    scale_factor: f64,
    show_grid: bool,
    uniform_background: bool,
    cell_opacity: f32,
    gap_opacity: f32,
    ui_context: cce_ui::context::UiContext,
}

impl GraphApp {
    fn rebuild_text_items(&mut self) {
        self.text_items.clear();
        
        // 1. Collect labels from Graph widget
        let mut labels = self.graph.text_labels_with_bounds(&self.ui_context);
        
        // 2. Collect labels from MenuBar widget
        labels.extend(self.menu_bar.text_labels_with_bounds(&self.ui_context));
        
        let scale = cce_ui::scale::scale_factor();
        for (label, bounds) in labels {
            let physical_size = label.font_size * scale;
            let metrics = Metrics::new(physical_size, physical_size * 1.4);
            let mut buf = Buffer::new(&mut self.font_system, metrics);
            buf.set_text(&mut self.font_system, &label.text, Attrs::new(), glyphon::Shaping::Advanced);
            buf.shape_until_scroll(&mut self.font_system, true);
            self.text_items.push(TextItem {
                buffer: buf,
                x: label.x,
                y: label.y,
                color: glyphon::Color::rgb(label.color[0], label.color[1], label.color[2]),
                bounds,
            });
        }
    }
}

impl Application for GraphApp {
    type Message = AppMessage;

    fn ui_context(&self) -> Option<&cce_ui::context::UiContext> {
        Some(&self.ui_context)
    }

    fn new(_qh: &QueueHandle<EngineState<Self>>, _sender: calloop::channel::Sender<Self::Message>) -> Self {
        let mut graph = Graph::new();
        
        // Define some initial graph nodes
        let nodes = vec![
            GraphNode {
                id: String::new(),
                name: "Data Source".to_string(),
                position: (1.0, 1.0),
                parameters: vec![],
                geom_visible: true,
                node_type: String::new(),
                inputs: 0,
                outputs: 1,
            },
            GraphNode {
                id: String::new(),
                name: "Filter".to_string(),
                position: (3.0, 1.0),
                parameters: vec![("input".to_string(), "Data Source".to_string(), "string".to_string())],
                geom_visible: true,
                node_type: String::new(),
                inputs: 1,
                outputs: 1,
            },
            GraphNode {
                id: String::new(),
                name: "Render Output".to_string(),
                position: (5.0, 2.0),
                parameters: vec![("input".to_string(), "Filter".to_string(), "string".to_string())],
                geom_visible: true,
                node_type: String::new(),
                inputs: 1,
                outputs: 1,
            },
        ];
        graph.set_nodes(&nodes);
        
        let (show_grid, snap_enabled, uniform_background, cell_opacity, gap_opacity, gap_width) = load_config();

        // Configure initial grid settings on the graph
        graph.set_show_network_grid(show_grid);
        graph.set_grid_sizes(140.0, 70.0);
        graph.set_skipped_sizes(gap_width, gap_width);
        graph.set_grid_origin(60.0, 60.0);
        graph.set_grid_snap_enabled(snap_enabled);
        graph.set_uniform_background(uniform_background);
        graph.set_cell_opacity(cell_opacity);
        graph.set_gap_opacity(gap_opacity);

        // Build Menu Bar with options to toggle new features
        let mut menu_bar = MenuBar::new(0.0, 0.0, 1024.0, 26.0)
            .with_title("cce-graph")
            .with_item("File", &["Exit"])
            .with_item("Edit", &["Add Node"])
            .with_item("View", &[
                "Show Grid",
                "Uniform Background",
                "Opacity: 95%",
                "Opacity: 75%",
                "Opacity: 50%"
            ]);
            
        menu_bar.set_item_checked(2, 0, show_grid);  // Show Grid checked
        menu_bar.set_item_checked(2, 1, uniform_background); // Uniform Background unchecked
        menu_bar.set_item_checked(2, 2, (cell_opacity - 0.95).abs() < 0.05);  // Opacity 95% checked
        menu_bar.set_item_checked(2, 3, (cell_opacity - 0.75).abs() < 0.05);  // Opacity 75% checked
        menu_bar.set_item_checked(2, 4, (cell_opacity - 0.50).abs() < 0.05);  // Opacity 50% checked

        let mut app = Self {
            menu_bar,
            graph,
            text_items: Vec::new(),
            font_system: FontSystem::new(),
            needs_rebuild: true,
            width: 1024,
            height: 768,
            scale_factor: 1.0,
            show_grid,
            uniform_background,
            cell_opacity,
            gap_opacity,
            ui_context: cce_ui::context::UiContext::new(),
        };
        
        app.menu_bar.set_rect(0.0, 0.0, 1024.0, 26.0);
        app.graph.set_rect(0.0, 26.0, 1024.0, 768.0 - 26.0);
        app.rebuild_text_items();
        app
    }

    fn settings(&self) -> WindowSettings {
        WindowSettings {
            title: "CCE Graph".to_string(),
            app_id: "cce-graph".to_string(),
            width: 1024,
            height: 768,
            fullscreen: false,
            min_size: Some((800, 600)),
        }
    }

    fn update(&mut self, msg: Self::Message, needs_rebuild: &mut bool, exit: &mut bool) {
        match msg {
            AppMessage::Exit => {
                *exit = true;
            }
            AppMessage::ToggleGrid => {
                self.show_grid = !self.show_grid;
                self.graph.set_show_network_grid(self.show_grid);
                self.menu_bar.set_item_checked(2, 0, self.show_grid);
                write_config_value("graph_show_grid", &self.show_grid.to_string());
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::ToggleUniformBackground => {
                self.uniform_background = !self.uniform_background;
                self.graph.set_uniform_background(self.uniform_background);
                self.menu_bar.set_item_checked(2, 1, self.uniform_background);
                write_config_value("graph_uniform_background", &self.uniform_background.to_string());
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SetOpacity95 => {
                self.cell_opacity = 0.95;
                self.gap_opacity = 0.95;
                self.graph.set_cell_opacity(0.95);
                self.graph.set_gap_opacity(0.95);
                self.menu_bar.set_item_checked(2, 2, true);
                self.menu_bar.set_item_checked(2, 3, false);
                self.menu_bar.set_item_checked(2, 4, false);
                write_config_value("graph_cell_opacity", "0.95");
                write_config_value("graph_gap_opacity", "0.95");
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SetOpacity75 => {
                self.cell_opacity = 0.75;
                self.gap_opacity = 0.75;
                self.graph.set_cell_opacity(0.75);
                self.graph.set_gap_opacity(0.75);
                self.menu_bar.set_item_checked(2, 2, false);
                self.menu_bar.set_item_checked(2, 3, true);
                self.menu_bar.set_item_checked(2, 4, false);
                write_config_value("graph_cell_opacity", "0.75");
                write_config_value("graph_gap_opacity", "0.75");
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SetOpacity50 => {
                self.cell_opacity = 0.50;
                self.gap_opacity = 0.50;
                self.graph.set_cell_opacity(0.50);
                self.graph.set_gap_opacity(0.50);
                self.menu_bar.set_item_checked(2, 2, false);
                self.menu_bar.set_item_checked(2, 3, false);
                self.menu_bar.set_item_checked(2, 4, true);
                write_config_value("graph_cell_opacity", "0.50");
                write_config_value("graph_gap_opacity", "0.50");
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::AddNode => {
                let mut nodes = self.graph.get_nodes();
                let next_id = nodes.len() + 1;
                nodes.push(GraphNode {
                    id: String::new(),
                    name: format!("Node {}", next_id),
                    position: (2.0 + (next_id % 3) as f32, 2.0 + (next_id / 3) as f32),
                    parameters: vec![],
                    geom_visible: true,
                    node_type: String::new(),
                    inputs: 1,
                    outputs: 1,
                });
                self.graph.set_nodes(&nodes);
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
        }
    }

    fn tick(&mut self, _dt: f32, _needs_rebuild: &mut bool) {}

    fn view(&mut self, quads: &mut Vec<(f32, f32, f32, f32, [f32; 4])>, size: LogicalSize, scale: f64) {
        let size_changed = self.width != size.width as u32 || self.height != size.height as u32 || self.scale_factor != scale;
        if self.needs_rebuild || size_changed {
            self.width = size.width as u32;
            self.height = size.height as u32;
            self.scale_factor = scale;
            
            // Layout MenuBar at the top
            self.menu_bar.set_rect(0.0, 0.0, size.width, 26.0);
            
            // Layout Graph below MenuBar
            self.graph.set_rect(0.0, 26.0, size.width, size.height - 26.0);
            
            self.rebuild_text_items();
            self.needs_rebuild = false;
        }

        // 1. Background quad (outer window color)
        quads.push((0.0, 0.0, self.width as f32, self.height as f32, [0.08, 0.08, 0.10, 1.0]));

        // 2. Add Graph extra quads (includes graph background color if uniform background is active)
        let graph_color = self.graph.color();
        if graph_color[3] > 0.0 {
            let (gx, gy, gw, gh) = self.graph.rect();
            quads.push((gx, gy, gw, gh, graph_color));
        }
        quads.extend(self.graph.extra_quads());

        // 3. Add MenuBar background and highlights/dropdowns
        let mb_color = self.menu_bar.color();
        let (mb_x, mb_y, mb_w, mb_h) = self.menu_bar.rect();
        quads.push((mb_x, mb_y, mb_w, mb_h, mb_color));



        // Add MenuBar extra quads (dropdown boxes)
        quads.extend(self.menu_bar.extra_quads());
    }

    fn text_items(&self) -> &[TextItem] {
        &self.text_items
    }

    fn handle_pointer_move(&mut self, pos: LogicalPosition, needs_rebuild: &mut bool) {
        let mut changed = false;

        // If MenuBar has an open menu, or cursor is over MenuBar, feed it first
        if self.menu_bar.is_menu_open() || self.menu_bar.hit_test(pos.x, pos.y, &self.ui_context) {
            if self.menu_bar.on_cursor_moved(pos.x, pos.y, &mut self.ui_context) {
                changed = true;
            }
        } else {
            // Otherwise feed it to Graph
            if self.graph.is_dragging() {
                if self.graph.drag_update(pos.x, pos.y) {
                    changed = true;
                }
            } else {
                if self.graph.on_cursor_moved(pos.x, pos.y, &mut self.ui_context) {
                    changed = true;
                }
            }
            // Clear menu bar hover if cursor moved away
            if self.menu_bar.on_cursor_moved(pos.x, pos.y, &mut self.ui_context) {
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
        let mut msg_out = None;

        if self.menu_bar.is_menu_open() || self.menu_bar.hit_test(pos.x, pos.y, &self.ui_context) {
            if self.menu_bar.mouse_input(button, state, pos.x, pos.y, &mut self.ui_context) {
                changed = true;
                
                // Check if a dropdown menu item was clicked
                if let Some((menu_idx, item_idx)) = self.menu_bar.menu_click() {
                    if menu_idx == 0 { // File
                        if item_idx == 0 { // Exit
                            msg_out = Some(AppMessage::Exit);
                        }
                    } else if menu_idx == 1 { // Edit
                        if item_idx == 0 { // Add Node
                            msg_out = Some(AppMessage::AddNode);
                        }
                    } else if menu_idx == 2 { // View
                        match item_idx {
                            0 => msg_out = Some(AppMessage::ToggleGrid),
                            1 => msg_out = Some(AppMessage::ToggleUniformBackground),
                            2 => msg_out = Some(AppMessage::SetOpacity95),
                            3 => msg_out = Some(AppMessage::SetOpacity75),
                            4 => msg_out = Some(AppMessage::SetOpacity50),
                            _ => {}
                        }
                    }
                }
            }
            
            // If the user clicked outside the open menu, close it
            if state == ElementState::Pressed && !self.menu_bar.hit_test(pos.x, pos.y, &self.ui_context) {
                self.menu_bar.unfocus();
                changed = true;
            }
        } else {
            // Otherwise route to Graph
            if button == MouseButton::Left {
                if state == ElementState::Pressed {
                    if self.graph.mouse_input(button, state, pos.x, pos.y, &mut self.ui_context) {
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
                        if self.graph.mouse_input(button, state, pos.x, pos.y, &mut self.ui_context) {
                            changed = true;
                        }
                    }
                }
            }
        }

        if changed {
            *needs_rebuild = true;
            self.needs_rebuild = true;
        }
        
        msg_out
    }

    fn handle_mouse_wheel(&mut self, delta: &MouseScrollDelta, pos: LogicalPosition, needs_rebuild: &mut bool) {
        if self.graph.mouse_wheel(delta, pos.x as f32, pos.y as f32, &mut self.ui_context) {
            *needs_rebuild = true;
            self.needs_rebuild = true;
        }
    }

    fn handle_key_input(&mut self, _event: &KeyEvent, _needs_rebuild: &mut bool) -> Option<Self::Message> {
        None
    }
}

fn load_config() -> (bool, bool, bool, f32, f32, f32) {
    let path = cce_ui::config::get_config_path();
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let val = cce_ui::config::parse_kdl_to_json(&content);
    
    let show_grid = val.pointer("/layout/graph_show_grid").and_then(|v| v.as_bool()).unwrap_or(true);
    let snap_enabled = val.pointer("/layout/graph_snap_enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let uniform_background = val.pointer("/layout/graph_uniform_background").and_then(|v| v.as_bool()).unwrap_or(false);
    let legacy_opacity = val.pointer("/layout/graph_network_opacity").and_then(|v| v.as_f64()).map(|n| n as f32).unwrap_or(0.95);
    let cell_opacity = val.pointer("/layout/graph_cell_opacity").and_then(|v| v.as_f64()).map(|n| n as f32).unwrap_or(legacy_opacity);
    let gap_opacity = val.pointer("/layout/graph_gap_opacity").and_then(|v| v.as_f64()).map(|n| n as f32).unwrap_or(legacy_opacity);
    let gap_width = val.pointer("/layout/graph_gap_width").and_then(|v| v.as_f64()).map(|n| n as f32).unwrap_or(35.0);
    
    (show_grid, snap_enabled, uniform_background, cell_opacity, gap_opacity, gap_width)
}

fn write_config_value(key: &str, value: &str) -> bool {
    let path = cce_ui::config::get_config_path();
    let path_str = path.to_string_lossy();
    cce_ui::config::write_config_value(&path_str, key, value, "layout")
}

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _guard = rt.enter();

    cce_ui::engine::run::<GraphApp>();
}
