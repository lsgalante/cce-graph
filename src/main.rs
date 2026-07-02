use wayland_client::QueueHandle;
use glyphon::{FontSystem, Buffer, Metrics, Attrs};
use cce_ui::engine::{Application, EngineState, LogicalPosition, LogicalSize, WindowSettings};
use cce_ui::widget::{MouseButton, ElementState, MouseScrollDelta, KeyEvent, TextItem, Element, Graph, GraphNode, MenuBar, MenuController, GraphController, WidgetId};

#[derive(Debug, Clone)]
enum AppMessage {
    New,
    Open,
    Save,
    SaveAs,
    Exit,
    ToggleGrid,
    ToggleUniformBackground,
    SetOpacity95,
    SetOpacity75,
    SetOpacity50,
    AddNode,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct GraphProjectState {
    name: String,
    nodes: Vec<GraphNode>,
    show_grid: bool,
    uniform_background: bool,
    opacity: f32,
}

struct GraphApp {
    menu_bar: MenuBar,
    graph: Graph,
    graph_id: WidgetId,
    text_items: Vec<TextItem>,
    font_system: FontSystem,
    needs_rebuild: bool,
    width: u32,
    height: u32,
    scale_factor: f64,
    show_grid: bool,
    uniform_background: bool,
    opacity: f32,
    ui_context: cce_ui::context::UiContext,
    loaded_project_path: Option<std::path::PathBuf>,
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

    fn new_project(&mut self) {
        self.graph.set_nodes(&[]);
        self.loaded_project_path = None;
        self.needs_rebuild = true;
    }

    fn save_project_to_path(&mut self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let project_dir = path;
        std::fs::create_dir_all(project_dir)?;

        // Create assets and code subdirectories
        std::fs::create_dir_all(project_dir.join("assets"))?;
        std::fs::create_dir_all(project_dir.join("code"))?;

        let state_file_path = project_dir.join("state.json");

        let state = GraphProjectState {
            name: project_dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Graph Project")
                .to_string(),
            nodes: self.graph.get_nodes(),
            show_grid: self.show_grid,
            uniform_background: self.uniform_background,
            opacity: self.opacity,
        };

        let content = serde_json::to_string_pretty(&state)?;
        std::fs::write(&state_file_path, content)?;
        
        self.loaded_project_path = Some(project_dir.to_path_buf());
        self.needs_rebuild = true;
        Ok(())
    }

    fn load_project_from_path(&mut self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let (state_file_path, project_dir) = if path.is_dir() {
            (path.join("state.json"), path.to_path_buf())
        } else {
            if path.file_name().map_or(false, |name| name == "state.json") {
                (path.to_path_buf(), path.parent().unwrap_or(path).to_path_buf())
            } else {
                (path.to_path_buf(), path.parent().unwrap_or(path).to_path_buf())
            }
        };

        let content = std::fs::read_to_string(&state_file_path)?;
        let state: GraphProjectState = serde_json::from_str(&content)?;

        self.graph.set_nodes(&state.nodes);
        self.show_grid = state.show_grid;
        self.uniform_background = state.uniform_background;
        self.opacity = state.opacity;

        // Apply grid/background settings to self.graph
        self.graph.set_show_network_grid(self.show_grid);
        self.graph.set_uniform_background(self.uniform_background);
        self.graph.set_network_opacity(self.opacity);

        // Update menu bar checkboxes
        self.menu_bar.set_item_checked(2, 0, self.show_grid);
        self.menu_bar.set_item_checked(2, 1, self.uniform_background);
        self.menu_bar.set_item_checked(2, 2, (self.opacity - 0.95).abs() < 0.05);
        self.menu_bar.set_item_checked(2, 3, (self.opacity - 0.75).abs() < 0.05);
        self.menu_bar.set_item_checked(2, 4, (self.opacity - 0.50).abs() < 0.05);

        self.loaded_project_path = Some(project_dir);
        self.needs_rebuild = true;
        Ok(())
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
        
        let (show_grid, snap_enabled, uniform_background, opacity, gap_width) = load_config();

        // Configure initial grid settings on the graph
        graph.set_show_network_grid(show_grid);
        graph.set_grid_sizes(140.0, 70.0);
        graph.set_skipped_sizes(gap_width, gap_width);
        graph.set_grid_origin(60.0, 60.0);
        graph.set_grid_snap_enabled(snap_enabled);
        graph.set_uniform_background(uniform_background);
        graph.set_network_opacity(opacity);

        // Build Menu Bar with options to toggle new features
        let mut menu_bar = MenuBar::new(0.0, 0.0, 1024.0, 26.0)
            .with_title("cce-graph")
            .with_item("File", &["New", "Open", "Save", "Save As", "Exit"])
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
        menu_bar.set_item_checked(2, 2, (opacity - 0.95).abs() < 0.05);  // Opacity 95% checked
        menu_bar.set_item_checked(2, 3, (opacity - 0.75).abs() < 0.05);  // Opacity 75% checked
        menu_bar.set_item_checked(2, 4, (opacity - 0.50).abs() < 0.05);  // Opacity 50% checked

        let graph_id = WidgetId(cce_ui::widget::NEXT_WIDGET_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst));

        let mut app = Self {
            menu_bar,
            graph,
            graph_id,
            text_items: Vec::new(),
            font_system: FontSystem::new(),
            needs_rebuild: true,
            width: 1024,
            height: 768,
            scale_factor: 1.0,
            show_grid,
            uniform_background,
            opacity,
            ui_context: cce_ui::context::UiContext::new(),
            loaded_project_path: None,
        };
        
        app.menu_bar.set_rect(0.0, 0.0, 1024.0, 26.0);
        app.graph.set_rect(0.0, 26.0, 1024.0, 768.0 - 26.0);
        app.rebuild_text_items();
        app
    }

    fn settings(&self) -> WindowSettings {
        let mut title = "CCE Graph".to_string();
        if let Some(ref path) = self.loaded_project_path {
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                title.push_str(" - ");
                title.push_str(filename);
            }
        }
        WindowSettings {
            title,
            app_id: "cce-graph".to_string(),
            width: 1024,
            height: 768,
            fullscreen: false,
            min_size: Some((800, 600)),
        }
    }

    fn update(&mut self, msg: Self::Message, needs_rebuild: &mut bool, exit: &mut bool) {
        match msg {
            AppMessage::New => {
                self.new_project();
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::Open => {
                if let Some(path) = cce_ui::file_dialog::pick_file("Open CCE Graph Project", &[]) {
                    if let Err(e) = self.load_project_from_path(&path) {
                        eprintln!("Failed to load project: {:?}", e);
                    }
                }
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::Save => {
                if let Some(path) = self.loaded_project_path.clone() {
                    if let Err(e) = self.save_project_to_path(&path) {
                        eprintln!("Failed to save project: {:?}", e);
                    }
                } else {
                    if let Some(path) = cce_ui::file_dialog::save_file("Save CCE Graph Project", &[]) {
                        if let Err(e) = self.save_project_to_path(&path) {
                            eprintln!("Failed to save project: {:?}", e);
                        }
                    }
                }
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SaveAs => {
                if let Some(path) = cce_ui::file_dialog::save_file("Save CCE Graph Project As", &[]) {
                    if let Err(e) = self.save_project_to_path(&path) {
                        eprintln!("Failed to save project: {:?}", e);
                    }
                }
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
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
                self.opacity = 0.95;
                self.graph.set_network_opacity(0.95);
                self.menu_bar.set_item_checked(2, 2, true);
                self.menu_bar.set_item_checked(2, 3, false);
                self.menu_bar.set_item_checked(2, 4, false);
                write_config_value("graph_network_opacity", "0.95");
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SetOpacity75 => {
                self.opacity = 0.75;
                self.graph.set_network_opacity(0.75);
                self.menu_bar.set_item_checked(2, 2, false);
                self.menu_bar.set_item_checked(2, 3, true);
                self.menu_bar.set_item_checked(2, 4, false);
                write_config_value("graph_network_opacity", "0.75");
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SetOpacity50 => {
                self.opacity = 0.50;
                self.graph.set_network_opacity(0.50);
                self.menu_bar.set_item_checked(2, 2, false);
                self.menu_bar.set_item_checked(2, 3, false);
                self.menu_bar.set_item_checked(2, 4, true);
                write_config_value("graph_network_opacity", "0.50");
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
        self.ui_context.clear_popovers();
        cce_ui::widget::popovers::clear();

        // Register widgets with correct, stable self addresses
        let menu_ptr = &mut self.menu_bar as *mut MenuBar as *mut (dyn Element + 'static);
        let graph_ptr = &mut self.graph as *mut Graph as *mut (dyn Element + 'static);
        self.ui_context.register_widget(self.menu_bar.base.id(), menu_ptr);
        self.ui_context.register_widget(self.graph_id, graph_ptr);

        if self.menu_bar.popover_rect().is_some() {
            self.ui_context.register_popover(&self.menu_bar);
            cce_ui::widget::popovers::register(&self.menu_bar);
        }

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

            self.ui_context.rebuild_spatial_grid();
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

    fn render_popovers(&self, pc: &mut dyn cce_ui::layout::RenderTarget) {
        cce_ui::layout::render_popovers(pc, &self.ui_context);
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
                        match item_idx {
                            0 => msg_out = Some(AppMessage::New),
                            1 => msg_out = Some(AppMessage::Open),
                            2 => msg_out = Some(AppMessage::Save),
                            3 => msg_out = Some(AppMessage::SaveAs),
                            4 => msg_out = Some(AppMessage::Exit),
                            _ => {}
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

fn load_config() -> (bool, bool, bool, f32, f32) {
    let path = cce_ui::config::get_config_path();
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let val = cce_ui::config::parse_kdl_to_json(&content);
    
    let show_grid = val.pointer("/layout/graph_show_grid").and_then(|v| v.as_bool()).unwrap_or(true);
    let snap_enabled = val.pointer("/layout/graph_snap_enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let uniform_background = val.pointer("/layout/graph_uniform_background").and_then(|v| v.as_bool()).unwrap_or(false);
    let opacity = val.pointer("/layout/graph_network_opacity").and_then(|v| v.as_f64()).map(|n| n as f32).unwrap_or(0.95);
    let gap_width = val.pointer("/layout/graph_gap_width").and_then(|v| v.as_f64()).map(|n| n as f32).unwrap_or(35.0);
    
    (show_grid, snap_enabled, uniform_background, opacity, gap_width)
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
