use wayland_client::QueueHandle;
use cce_ui::engine::{Application, EngineState, LogicalPosition, LogicalSize, WindowSettings};
use cce_ui::widget::{Adapted, MouseButton, ElementState, MouseScrollDelta, KeyEvent, WidgetHost, Event, Graph, GraphNode, MenuBar, GraphController, Dropdown, Label};
use image::GenericImageView;

#[derive(Debug, Clone)]
enum AppMessage {
    New,
    Open,
    OpenRecent(std::path::PathBuf),
    Save,
    SaveAs,
    SaveToPath(std::path::PathBuf),
    Exit,
    ToggleGrid,
    ToggleUniformBackground,
    SetOpacity95,
    SetOpacity75,
    SetOpacity50,
    ToggleControlPanel,
    AddNode,
    AddImage,
    AddImageFromPath(std::path::PathBuf),
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct GraphProjectImage {
    path: String,
    position: (f32, f32), // (column, row)
    size: (f32, f32), // (w_cols, h_rows)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct GraphProjectState {
    name: String,
    nodes: Vec<GraphNode>,
    images: Vec<GraphProjectImage>,
    show_grid: bool,
    uniform_background: bool,
    opacity: f32,
}

struct LoadedImage {
    path: String,
    position: (f32, f32),
    size: (f32, f32),
    pixels: Vec<[u8; 4]>,
    pixel_width: u32,
    pixel_height: u32,
}

struct GraphApp {
    menu_bar: Adapted<MenuBar>,
    dropdown_file: Adapted<Dropdown>,
    dropdown_edit: Adapted<Dropdown>,
    dropdown_view: Adapted<Dropdown>,

    graph: Adapted<Graph>,
    needs_rebuild: bool,
    width: u32,
    height: u32,
    scale_factor: f64,
    show_grid: bool,
    uniform_background: bool,
    opacity: f32,
    ui_context: cce_ui::context::UiContext,
    loaded_project_path: Option<std::path::PathBuf>,
    loaded_images: Vec<LoadedImage>,
    widgets_registered: bool,
    message_sender: calloop::channel::Sender<AppMessage>,
    dragging_image_idx: Option<usize>,
    drag_image_ox: f32,
    drag_image_oy: f32,
    selected_image_idx: Option<usize>,
    // Dissolved control panel (was a draggable Plate): position + drag state live here;
    // its plate is emitted as prims and the label is a standalone walked widget.
    panel_x: f32,
    panel_y: f32,
    panel_dragging: bool,
    panel_drag_ox: f32,
    panel_drag_oy: f32,
    show_control_panel: bool,
    control_panel_label: cce_ui::widget::Adapted<cce_ui::widget::Label>,
}

fn get_default_project_path() -> std::path::PathBuf {
    cce_ui::config::cce_config_dir().join("cce-graph").join("default.kdl")
}

fn ensure_default_project_file(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Seeded empty: an empty KDL document parses to exactly the loader's defaults
    // (`name "default"`, `show_grid true`, `uniform_background false`, `opacity 0.95` — see
    // `load_project_from_kdl_path`), so the app opens on a blank canvas. The file itself still has
    // to exist, since loading reads it directly.
    std::fs::write(path, "")?;
    Ok(())
}

fn load_project_from_kdl_path(path: &std::path::Path) -> Result<GraphProjectState, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let doc = content.parse::<kdl::KdlDocument>()?;
    
    let mut name = "default".to_string();
    let mut show_grid = true;
    let mut uniform_background = false;
    let mut opacity = 0.95f32;
    let mut nodes = Vec::new();
    let mut images = Vec::new();

    for node in doc.nodes() {
        let name_val = node.name().value();
        match name_val {
            "name" => {
                if let Some(entry) = node.entries().first() {
                    if let kdl::KdlValue::String(s) = entry.value() {
                        name = s.to_string();
                    }
                }
            }
            "show_grid" => {
                if let Some(entry) = node.entries().first() {
                    if let kdl::KdlValue::Bool(b) = entry.value() {
                        show_grid = *b;
                    }
                }
            }
            "uniform_background" => {
                if let Some(entry) = node.entries().first() {
                    if let kdl::KdlValue::Bool(b) = entry.value() {
                        uniform_background = *b;
                    }
                }
            }
            "opacity" => {
                if let Some(entry) = node.entries().first() {
                    match entry.value() {
                        kdl::KdlValue::Base10Float(f) => opacity = *f as f32,
                        kdl::KdlValue::Base10(i) => opacity = *i as f32,
                        _ => {}
                    }
                }
            }
            "node" => {
                let node_name = if let Some(entry) = node.entries().first() {
                    if let kdl::KdlValue::String(s) = entry.value() {
                        s.to_string()
                    } else {
                        "".to_string()
                    }
                } else {
                    "".to_string()
                };

                let mut position = (0.0f32, 0.0f32);
                let mut inputs = 0;
                let mut outputs = 0;
                let mut geom_visible = true;
                let mut node_type = "".to_string();
                let mut parameters = Vec::new();

                if let Some(children) = node.children() {
                    for child in children.nodes() {
                        let child_name = child.name().value();
                        match child_name {
                            "position" => {
                                let coords: Vec<f32> = child.entries().iter().filter_map(|e| {
                                    match e.value() {
                                        kdl::KdlValue::Base10Float(f) => Some(*f as f32),
                                        kdl::KdlValue::Base10(i) => Some(*i as f32),
                                        _ => None
                                    }
                                }).collect();
                                if coords.len() >= 2 {
                                    position = (coords[0], coords[1]);
                                }
                            }
                            "inputs" => {
                                if let Some(e) = child.entries().first() {
                                    if let kdl::KdlValue::Base10(i) = e.value() {
                                        inputs = *i as usize;
                                    }
                                }
                            }
                            "outputs" => {
                                if let Some(e) = child.entries().first() {
                                    if let kdl::KdlValue::Base10(i) = e.value() {
                                        outputs = *i as usize;
                                    }
                                }
                            }
                            "geom_visible" => {
                                if let Some(e) = child.entries().first() {
                                    if let kdl::KdlValue::Bool(b) = e.value() {
                                        geom_visible = *b;
                                    }
                                }
                            }
                            "node_type" => {
                                if let Some(e) = child.entries().first() {
                                    if let kdl::KdlValue::String(s) = e.value() {
                                        node_type = s.to_string();
                                    }
                                }
                            }
                            "parameter" => {
                                let mut param_name = "".to_string();
                                let mut param_val = "".to_string();
                                let mut param_type = "".to_string();
                                if let Some(e) = child.entries().first() {
                                    if let kdl::KdlValue::String(s) = e.value() {
                                        param_name = s.to_string();
                                    }
                                }
                                for entry in child.entries().iter().skip(1) {
                                    if let Some(prop) = entry.name() {
                                        let prop_str = prop.value();
                                        match prop_str {
                                            "value" => {
                                                if let kdl::KdlValue::String(s) = entry.value() {
                                                    param_val = s.to_string();
                                                }
                                            }
                                            "type" => {
                                                if let kdl::KdlValue::String(s) = entry.value() {
                                                    param_type = s.to_string();
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                parameters.push((param_name, param_val, param_type));
                            }
                            _ => {}
                        }
                    }
                }

                nodes.push(GraphNode {
                    id: "".to_string(),
                    name: node_name,
                    position,
                    parameters,
                    geom_visible,
                    node_type,
                    inputs,
                    outputs,
                });
            }
            "image" => {
                let img_path = if let Some(entry) = node.entries().first() {
                    if let kdl::KdlValue::String(s) = entry.value() {
                        s.to_string()
                    } else {
                        "".to_string()
                    }
                } else {
                    "".to_string()
                };

                let mut position = (0.0f32, 0.0f32);
                let mut size = (0.0f32, 0.0f32);

                if let Some(children) = node.children() {
                    for child in children.nodes() {
                        let child_name = child.name().value();
                        match child_name {
                            "position" => {
                                let coords: Vec<f32> = child.entries().iter().filter_map(|e| {
                                    match e.value() {
                                        kdl::KdlValue::Base10Float(f) => Some(*f as f32),
                                        kdl::KdlValue::Base10(i) => Some(*i as f32),
                                        _ => None
                                    }
                                }).collect();
                                if coords.len() >= 2 {
                                    position = (coords[0], coords[1]);
                                }
                            }
                            "size" => {
                                let sz: Vec<f32> = child.entries().iter().filter_map(|e| {
                                    match e.value() {
                                        kdl::KdlValue::Base10Float(f) => Some(*f as f32),
                                        kdl::KdlValue::Base10(i) => Some(*i as f32),
                                        _ => None
                                    }
                                }).collect();
                                if sz.len() >= 2 {
                                    size = (sz[0], sz[1]);
                                }
                            }
                            _ => {}
                        }
                    }
                }

                images.push(GraphProjectImage {
                    path: img_path,
                    position,
                    size,
                });
            }
            _ => {}
        }
    }

    Ok(GraphProjectState {
        name,
        nodes,
        images,
        show_grid,
        uniform_background,
        opacity,
    })
}

fn save_project_to_kdl_path(path: &std::path::Path, state: &GraphProjectState) -> Result<(), Box<dyn std::error::Error>> {
    let mut kdl = String::new();
    kdl.push_str(&format!("name {:?}\n", state.name));
    kdl.push_str(&format!("show_grid {}\n", state.show_grid));
    kdl.push_str(&format!("uniform_background {}\n", state.uniform_background));
    kdl.push_str(&format!("opacity {}\n\n", state.opacity));

    for node in &state.nodes {
        kdl.push_str(&format!("node {:?} {{\n", node.name));
        kdl.push_str(&format!("    position {} {}\n", node.position.0, node.position.1));
        kdl.push_str(&format!("    inputs {}\n", node.inputs));
        kdl.push_str(&format!("    outputs {}\n", node.outputs));
        kdl.push_str(&format!("    geom_visible {}\n", node.geom_visible));
        if !node.node_type.is_empty() {
            kdl.push_str(&format!("    node_type {:?}\n", node.node_type));
        }
        for (p_name, p_val, p_type) in &node.parameters {
            kdl.push_str(&format!("    parameter {:?} value={:?} type={:?}\n", p_name, p_val, p_type));
        }
        kdl.push_str("}\n\n");
    }

    for img in &state.images {
        kdl.push_str(&format!("image {:?} {{\n", img.path));
        kdl.push_str(&format!("    position {} {}\n", img.position.0, img.position.1));
        kdl.push_str(&format!("    size {} {}\n", img.size.0, img.size.1));
        kdl.push_str("}\n\n");
    }

    std::fs::write(path, kdl)?;
    Ok(())
}

fn get_view_options(show_grid: bool, uniform_bg: bool, opacity: f32, show_panel: bool) -> Vec<String> {
    vec![
        format!("{} Show Grid", if show_grid { "✓" } else { "  " }),
        format!("{} Uniform Background", if uniform_bg { "✓" } else { "  " }),
        format!("{} Opacity: 95%", if (opacity - 0.95).abs() < 0.05 { "✓" } else { "  " }),
        format!("{} Opacity: 75%", if (opacity - 0.75).abs() < 0.05 { "✓" } else { "  " }),
        format!("{} Opacity: 50%", if (opacity - 0.50).abs() < 0.05 { "✓" } else { "  " }),
        format!("{} Control Panel", if show_panel { "✓" } else { "  " }),
    ]
}

fn load_image_pixels(path: &std::path::Path) -> Option<(Vec<[u8; 4]>, u32, u32)> {
    let img = image::open(path).ok()?;
    let max_dim = 96;
    let (w, h) = img.dimensions();
    let (nw, nh) = if w > h {
        (max_dim, (h as f32 * (max_dim as f32 / w as f32)) as u32)
    } else {
        ((w as f32 * (max_dim as f32 / h as f32)) as u32, max_dim)
    };
    let img = img.resize_exact(nw, nh, image::imageops::FilterType::Triangle);
    let rgba = img.to_rgba8();
    let pixels = rgba.chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    Some((pixels, nw, nh))
}

fn matches_keybind(event: &KeyEvent, keybind: &str) -> bool {
    let kb_clean = keybind.trim().to_lowercase();
    let parts: Vec<&str> = kb_clean.split('+').collect();
    
    let mut has_ctrl = false;
    let mut has_shift = false;
    let mut main_key_str = "";

    for part in &parts {
        match *part {
            "ctrl" => has_ctrl = true,
            "shift" => has_shift = true,
            "alt" | "super" => {}
            other => main_key_str = other,
        }
    }

    if event.ctrl != has_ctrl || event.shift != has_shift {
        return false;
    }

    match &event.logical_key {
        cce_ui::widget::Key::Named(nk) => {
            let key_str = match nk {
                cce_ui::widget::NamedKey::Backspace => "backspace",
                cce_ui::widget::NamedKey::Tab => "tab",
                cce_ui::widget::NamedKey::Enter => "enter",
                cce_ui::widget::NamedKey::Space => "space",
                cce_ui::widget::NamedKey::ArrowDown => "down",
                cce_ui::widget::NamedKey::ArrowLeft => "left",
                cce_ui::widget::NamedKey::ArrowRight => "right",
                cce_ui::widget::NamedKey::ArrowUp => "up",
                cce_ui::widget::NamedKey::End => "end",
                cce_ui::widget::NamedKey::Home => "home",
                cce_ui::widget::NamedKey::PageDown => "pagedown",
                cce_ui::widget::NamedKey::PageUp => "pageup",
                cce_ui::widget::NamedKey::Delete => "delete",
                _ => "",
            };
            key_str == main_key_str
        }
        cce_ui::widget::Key::Character(s) => {
            s.to_lowercase() == main_key_str
        }
    }
}

impl GraphApp {
    fn delete_selected_node(&mut self) {
        if let Some(idx) = self.graph.selected_node() {
            let mut nodes = self.graph.get_nodes();
            if idx < nodes.len() {
                let deleted_node_name = nodes[idx].name.clone();
                nodes.remove(idx);
                
                // Clear inputs/parameters of other nodes pointing to this deleted node name
                for node in &mut nodes {
                    for param in &mut node.parameters {
                        if param.1 == deleted_node_name {
                            param.1 = String::new();
                        }
                    }
                }
                
                self.graph.set_nodes(&nodes);
                self.graph.set_selected_node(None);
                self.needs_rebuild = true;
            }
        } else if let Some(idx) = self.selected_image_idx {
            if idx < self.loaded_images.len() {
                self.loaded_images.remove(idx);
                self.selected_image_idx = None;
                self.needs_rebuild = true;
            }
        }
    }

    fn update_view_options(&mut self) {
        self.dropdown_view.options = get_view_options(self.show_grid, self.uniform_background, self.opacity, self.show_control_panel);
    }

    /// The dissolved control panel's rect (fixed 210x160, app-tracked position).
    fn panel_rect(&self) -> (f32, f32, f32, f32) {
        (self.panel_x, self.panel_y, 210.0, 160.0)
    }

    fn panel_hit(&self, px: f32, py: f32) -> bool {
        let (x, y, w, h) = self.panel_rect();
        px >= x && px < x + w && py >= y && py < y + h
    }

    /// Replicates the dissolved Plate's centered-first-child placement for the label
    /// (plate padding inset, centered, 50px nominal height on first placement).
    fn position_panel_label(&mut self) {
        let pad = cce_ui::layout::plate_padding();
        let (px, py, pw, ph) = self.panel_rect();
        let left_x = px + pad;
        let available_w = (pw - 2.0 * pad).max(1.0);
        let start_y = py + pad;
        let available_h = (ph - 2.0 * pad).max(1.0);
        let center_x = left_x + available_w / 2.0;
        let center_y = start_y + available_h / 2.0;

        let (_, _, lw, lh) = self.control_panel_label.rect();
        let use_w = if lw > 0.0 { lw.min(available_w) } else { available_w };
        let use_h = if lh > 0.0 { lh } else { 50.0 };
        let cx = (center_x - use_w / 2.0).clamp(left_x, (left_x + available_w - use_w).max(left_x));
        let cy = (center_y - use_h / 2.0).clamp(start_y, (start_y + available_h - use_h).max(start_y));
        let cw = use_w.min(px + pw - pad - cx);
        let ch = use_h.min(py + ph - pad - cy);
        self.control_panel_label.set_rect(cx, cy, cw, ch);
    }

    /// The dissolved Plate's visual: config plate color (else page-low, with the drag tint),
    /// plate opacity, negative-alpha blur flag, config border and corner radius.
    fn panel_visual(&self) -> ([f32; 4], Option<([f32; 4], f32)>, f32) {
        let mut c = if let Some(c) = cce_ui::colors::plate_color() {
            c
        } else if self.panel_dragging {
            let b = cce_ui::colors::page_low_color();
            [(b[0] + 0.10).min(1.0), (b[1] + 0.15).min(1.0), (b[2] + 0.12).min(1.0), b[3]]
        } else {
            cce_ui::colors::page_low_color()
        };
        c[3] *= cce_ui::layout::plate_opacity();
        if cce_ui::colors::plate_blur() {
            c[3] = -c[3].abs();
        }
        let border = cce_ui::colors::plate_border_color()
            .map(|bc| (bc, cce_ui::colors::plate_border_thickness()));
        (c, border, cce_ui::layout::plate_corner_radius())
    }

    fn new_project(&mut self) {
        self.graph.set_nodes(&[]);
        self.loaded_images.clear();
        self.loaded_project_path = None;
        self.needs_rebuild = true;
    }

    fn save_project_to_path(&mut self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let project_dir = path;
        std::fs::create_dir_all(project_dir)?;

        // Create assets and code subdirectories
        std::fs::create_dir_all(project_dir.join("assets"))?;
        std::fs::create_dir_all(project_dir.join("code"))?;

        let state_file_path = project_dir.join("state.kdl");

        let project_images: Vec<GraphProjectImage> = self.loaded_images.iter()
            .map(|img| GraphProjectImage {
                path: img.path.clone(),
                position: img.position,
                size: img.size,
            })
            .collect();

        let state = GraphProjectState {
            name: project_dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Graph Project")
                .to_string(),
            nodes: self.graph.get_nodes(),
            images: project_images,
            show_grid: self.show_grid,
            uniform_background: self.uniform_background,
            opacity: self.opacity,
        };

        save_project_to_kdl_path(&state_file_path, &state)?;

        // Clean up old state.json if it exists
        let old_json_path = project_dir.join("state.json");
        if old_json_path.exists() {
            let _ = std::fs::remove_file(old_json_path);
        }
        
        self.loaded_project_path = Some(project_dir.to_path_buf());
        self.needs_rebuild = true;
        Ok(())
    }

    fn load_recent_files(&self) -> Vec<String> {
        cce_ui::config::load_recent_files()
    }

    fn save_recent_files(&self, files: &[String]) {
        cce_ui::config::save_recent_files(files)
    }

    fn add_recent_file(&mut self, file_path: &std::path::Path) {
        if let Ok(abs_path) = std::fs::canonicalize(file_path) {
            let abs_str = abs_path.to_string_lossy().to_string();
            let mut recent = self.load_recent_files();
            recent.retain(|p| p != &abs_str);
            recent.insert(0, abs_str);
            if recent.len() > 10 {
                recent.truncate(10);
            }
            self.save_recent_files(&recent);
            self.update_recent_files_dropdown(recent);
        }
    }

    fn update_recent_files_dropdown(&mut self, recent: Vec<String>) {
        let mut options = vec![
            "New".to_string(),
            "Open...".to_string(),
            "Save".to_string(),
            "Save As".to_string(),
        ];
        if !recent.is_empty() {
            options.push("-".to_string());
            options.extend(recent);
        }
        options.push("-".to_string());
        options.push("Exit".to_string());
        self.dropdown_file.options = options;
        self.dropdown_file.selected = 999;
        self.needs_rebuild = true;
    }

    fn load_project_from_path(&mut self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let (state_file_path, project_dir) = if path.is_dir() {
            let kdl_path = path.join("state.kdl");
            if kdl_path.exists() {
                (kdl_path, path.to_path_buf())
            } else {
                (path.join("state.json"), path.to_path_buf())
            }
        } else {
            if path.file_name().map_or(false, |name| name == "state.json" || name == "state.kdl") {
                (path.to_path_buf(), path.parent().unwrap_or(path).to_path_buf())
            } else {
                (path.to_path_buf(), path.parent().unwrap_or(path).to_path_buf())
            }
        };

        let state: GraphProjectState = if state_file_path.extension().map_or(false, |ext| ext == "kdl") {
            load_project_from_kdl_path(&state_file_path)?
        } else {
            let content = std::fs::read_to_string(&state_file_path)?;
            serde_json::from_str(&content)?
        };

        self.graph.set_nodes(&state.nodes);
        self.show_grid = state.show_grid;
        self.uniform_background = state.uniform_background;
        self.opacity = state.opacity;

        // Load images
        self.loaded_images.clear();
        for img in state.images {
            let image_path = if std::path::Path::new(&img.path).is_absolute() {
                std::path::PathBuf::from(&img.path)
            } else {
                project_dir.join(&img.path)
            };

            if let Some((pixels, pw, ph)) = load_image_pixels(&image_path) {
                self.loaded_images.push(LoadedImage {
                    path: img.path.clone(),
                    position: img.position,
                    size: img.size,
                    pixels,
                    pixel_width: pw,
                    pixel_height: ph,
                });
            } else {
                eprintln!("Warning: Failed to load image at {:?}", image_path);
            }
        }

        // Apply grid/background settings to self.graph
        self.graph.set_show_network_grid(self.show_grid);
        self.graph.set_uniform_background(self.uniform_background);
        self.graph.set_network_opacity(self.opacity);

        // Update view dropdown options
        self.update_view_options();

        self.loaded_project_path = Some(project_dir);
        self.needs_rebuild = true;
        Ok(())
    }

    fn hit_test_image(&self, px: f32, py: f32) -> Option<usize> {
        let (grid_origin_x, grid_origin_y) = self.graph.grid_origin();
        let (grid_size_x, grid_size_y) = self.graph.grid_sizes();
        let (skipped_row_h, skipped_col_w) = self.graph.skipped_sizes();
        
        let step_x = grid_size_x + skipped_col_w;
        let step_y = grid_size_y + skipped_row_h;
        
        for (i, img) in self.loaded_images.iter().enumerate().rev() {
            let col = img.position.0;
            let row = img.position.1;
            
            let screen_x = grid_origin_x + col * step_x;
            let screen_y = grid_origin_y + row * step_y;
            
            let screen_w = img.size.0 * grid_size_x + (img.size.0 - 1.0).max(0.0) * skipped_col_w;
            let aspect = img.pixel_height as f32 / img.pixel_width as f32;
            let screen_h = screen_w * aspect;
            
            if px >= screen_x && px <= screen_x + screen_w && py >= screen_y && py <= screen_y + screen_h {
                return Some(i);
            }
        }
        None
    }

    fn add_image(&mut self, src_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let image_name = src_path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "image.png".to_string());

        let final_path = if let Some(ref project_dir) = self.loaded_project_path {
            let dest_dir = project_dir.join("assets");
            std::fs::create_dir_all(&dest_dir)?;
            let dest_path = dest_dir.join(&image_name);
            std::fs::copy(src_path, &dest_path)?;
            format!("assets/{}", image_name)
        } else {
            src_path.to_string_lossy().to_string()
        };

        if let Some((pixels, pw, ph)) = load_image_pixels(src_path) {
            let aspect = ph as f32 / pw as f32;
            let size_w = 4.0;
            let size_h = size_w * aspect;
            
            let col = 2.0;
            let row = 2.0 + self.loaded_images.len() as f32 * 5.0;

            self.loaded_images.push(LoadedImage {
                path: final_path,
                position: (col, row),
                size: (size_w, size_h),
                pixels,
                pixel_width: pw,
                pixel_height: ph,
            });
            self.needs_rebuild = true;
        }
        Ok(())
    }
}

impl Application for GraphApp {
    type Message = AppMessage;

    fn ui_context(&self) -> Option<&cce_ui::context::UiContext> {
        Some(&self.ui_context)
    }

    fn is_movable_backplate_at(&self, px: f32, py: f32) -> bool {
        // Root Backplate dissolved (Phase 6m): the surface itself is the movable plate; drag
        // anywhere a drag-blocking widget isn't.
        self.ui_context.drag_allowed_at(px, py)
    }

    fn ui_context_mut(&mut self) -> Option<&mut cce_ui::context::UiContext> {
        Some(&mut self.ui_context)
    }

    fn new(_qh: &QueueHandle<EngineState<Self>>, _sender: calloop::channel::Sender<Self::Message>) -> Self {
        let mut graph = Graph::new();
        
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
        // Build Menu Bar background
        let menu_bar = MenuBar::new(0.0, 0.0, 1024.0, 42.0)
            .with_color([0.08, 0.08, 0.12, 1.0]);

        let recent = cce_ui::config::load_recent_files();

        let mut file_options = vec![
            "New".to_string(),
            "Open...".to_string(),
            "Save".to_string(),
            "Save As".to_string(),
        ];
        if !recent.is_empty() {
            file_options.push("-".to_string());
            file_options.extend(recent);
        }
        file_options.push("-".to_string());
        file_options.push("Exit".to_string());

        let dropdown_file = Dropdown::new(file_options, 999)
            .with_custom_display_text("File");

        let dropdown_edit = Dropdown::new(
            vec![
                "Add Node".to_string(),
                "Add Image".to_string(),
            ],
            999,
        )
        .with_custom_display_text("Edit");

        let dropdown_view = Dropdown::new(
            get_view_options(show_grid, uniform_background, opacity, false),
            999,
        )
        .with_custom_display_text("View");



        let show_control_panel = false;

        let control_panel_label = Label::new("No Node Selected")
            .with_font_size(12.0)
            .with_color([204, 204, 221]);

        let mut app = Self {

            menu_bar,
            dropdown_file,
            dropdown_edit,
            dropdown_view,
            graph,
            needs_rebuild: true,
            width: 1024,
            height: 768,
            scale_factor: 1.0,
            show_grid,
            uniform_background,
            opacity,
            ui_context: cce_ui::context::UiContext::new(),
            loaded_project_path: None,
            loaded_images: Vec::new(),
            widgets_registered: false,
            message_sender: _sender.clone(),
            dragging_image_idx: None,
            drag_image_ox: 0.0,
            drag_image_oy: 0.0,
            selected_image_idx: None,
            panel_x: 800.0,
            panel_y: 50.0,
            panel_dragging: false,
            panel_drag_ox: 0.0,
            panel_drag_oy: 0.0,
            show_control_panel,
            control_panel_label,
        };
        

        app.menu_bar.set_rect(0.0, 0.0, 1024.0, 42.0);
        app.dropdown_file.set_rect(10.0, 8.0, 70.0, 26.0);
        app.dropdown_edit.set_rect(90.0, 8.0, 70.0, 26.0);
        app.dropdown_view.set_rect(170.0, 8.0, 70.0, 26.0);
        app.graph.set_rect(0.0, 42.0, 1024.0, 768.0 - 42.0);

        let args: Vec<String> = std::env::args().collect();
        if args.len() > 1 {
            let path = std::path::PathBuf::from(&args[1]);
            if path.exists() {
                if let Err(e) = app.load_project_from_path(&path) {
                    eprintln!("Failed to load project on startup: {:?}", e);
                } else {
                    app.add_recent_file(&path);
                }
            }
        } else {
            let default_path = get_default_project_path();
            let _ = ensure_default_project_file(&default_path);
            if let Err(e) = app.load_project_from_path(&default_path) {
                eprintln!("Failed to load default project: {:?}", e);
            }
        }

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
                let sender = self.message_sender.clone();
                std::thread::spawn(move || {
                    if let Some(path) = cce_ui::file_dialog::pick_file("Open CCE Graph Project", &[]) {
                        let _ = sender.send(AppMessage::OpenRecent(path));
                    }
                });
            }
            AppMessage::OpenRecent(path) => {
                if let Err(e) = self.load_project_from_path(&path) {
                    eprintln!("Failed to load project: {:?}", e);
                } else {
                    self.add_recent_file(&path);
                }
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::Save => {
                if let Some(path) = self.loaded_project_path.clone() {
                    if let Err(e) = self.save_project_to_path(&path) {
                        eprintln!("Failed to save project: {:?}", e);
                    } else {
                        self.add_recent_file(&path);
                    }
                    *needs_rebuild = true;
                    self.needs_rebuild = true;
                } else {
                    let sender = self.message_sender.clone();
                    std::thread::spawn(move || {
                        if let Some(path) = cce_ui::file_dialog::save_file("Save CCE Graph Project", &[]) {
                            let _ = sender.send(AppMessage::SaveToPath(path));
                        }
                    });
                }
            }
            AppMessage::SaveAs => {
                let sender = self.message_sender.clone();
                std::thread::spawn(move || {
                    if let Some(path) = cce_ui::file_dialog::save_file("Save CCE Graph Project As", &[]) {
                        let _ = sender.send(AppMessage::SaveToPath(path));
                    }
                });
            }
            AppMessage::SaveToPath(path) => {
                if let Err(e) = self.save_project_to_path(&path) {
                    eprintln!("Failed to save project: {:?}", e);
                } else {
                    self.add_recent_file(&path);
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
                self.update_view_options();
                write_config_value("graph_show_grid", &self.show_grid.to_string());
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::ToggleUniformBackground => {
                self.uniform_background = !self.uniform_background;
                self.graph.set_uniform_background(self.uniform_background);
                self.update_view_options();
                write_config_value("style.surface.graph.uniform_background", &self.uniform_background.to_string());
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SetOpacity95 => {
                self.opacity = 0.95;
                self.graph.set_network_opacity(0.95);
                self.update_view_options();
                write_config_value("graph_network_opacity", "0.95");
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SetOpacity75 => {
                self.opacity = 0.75;
                self.graph.set_network_opacity(0.75);
                self.update_view_options();
                write_config_value("graph_network_opacity", "0.75");
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SetOpacity50 => {
                self.opacity = 0.50;
                self.graph.set_network_opacity(0.50);
                self.update_view_options();
                write_config_value("graph_network_opacity", "0.50");
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::ToggleControlPanel => {
                self.show_control_panel = !self.show_control_panel;
                self.update_view_options();
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
            AppMessage::AddImage => {
                let sender = self.message_sender.clone();
                std::thread::spawn(move || {
                    let exts: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp"];
                    let filters = [("Images", exts)];
                    if let Some(path) = cce_ui::file_dialog::pick_file("Select Image", &filters) {
                        let _ = sender.send(AppMessage::AddImageFromPath(path));
                    }
                });
            }
            AppMessage::AddImageFromPath(path) => {
                if let Err(e) = self.add_image(&path) {
                    eprintln!("Failed to add image: {:?}", e);
                }
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
        }
    }

    fn tick(&mut self, dt: f32, needs_rebuild: &mut bool) {
        // Pump the widget tick walk: animating widgets (the menu dropdowns'
        // expand/contract) register as tick receivers and report changed
        // until their transition lands — without this a closing menu freezes
        // fully open.
        if self.ui_context.tick(dt) {
            *needs_rebuild = true;
            self.needs_rebuild = true;
        }
    }

    fn display_list(&mut self, size: LogicalSize, scale: f64) -> Option<cce_ui::scene::paint::DisplayList> {
        // Phase 6 single paint path: setup/relayout (the old view() body), then the whole
        // frame — the widget tree walked into one list plus the loaded images' pixel
        // quads — is built here. Widget text comes from the paint walk (Graph's node names
        // through its per-label hatch, the control panel via the 6i container-text fix).
        self.ui_context.clear_popovers();

        let is_first_layout = !self.widgets_registered;
        if !self.widgets_registered {
            // The root Backplate is DISSOLVED (Phase 6m recipe): top-level widgets register
            // directly (parentless), the window plate is emitted below as prims, and the two
            // Plates keep their own children.
            unsafe {
                let self_ptr = self as *mut Self;

                self.ui_context.register_widget(self.menu_bar.id(), (*self_ptr).menu_bar.as_ptr_mut());
                self.ui_context.register_widget(self.dropdown_file.base().id(), (*self_ptr).dropdown_file.as_ptr_mut());
                self.ui_context.register_widget(self.dropdown_edit.base().id(), (*self_ptr).dropdown_edit.as_ptr_mut());
                self.ui_context.register_widget(self.dropdown_view.base().id(), (*self_ptr).dropdown_view.as_ptr_mut());
                // Registered under the widget's OWN base id (the id-rooted router resolves
                // dispatch roots through the registry; the old synthetic `graph_id` key left
                // `graph.id()` unresolvable — a latent hole the pointer router masked).
                self.ui_context.register_widget(self.graph.id(), (*self_ptr).graph.as_ptr_mut());
                self.ui_context.register_widget(self.control_panel_label.base().id(), (*self_ptr).control_panel_label.as_ptr_mut());
            }
            self.widgets_registered = true;
        }

        // Check selected node and update control panel label
        let selected_node_idx = self.graph.selected_node();
        let label_text = if let Some(idx) = selected_node_idx {
            let nodes = self.graph.get_nodes();
            if let Some(node) = nodes.get(idx) {
                let mut info = format!("Selected Node:\nID: {}\nName: {}\nType: {}\nInputs: {}\nOutputs: {}",
                    node.id, node.name, node.node_type, node.inputs, node.outputs
                );
                if !node.parameters.is_empty() {
                    info.push_str("\n\nParameters:");
                    for (name, val, p_type) in &node.parameters {
                        info.push_str(&format!("\n- {}: {} ({})", name, val, p_type));
                    }
                }
                info
            } else {
                "No Object Selected".to_string()
            }
        } else if let Some(img_idx) = self.selected_image_idx {
            if let Some(img) = self.loaded_images.get(img_idx) {
                let filename = std::path::Path::new(&img.path)
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or(&img.path);
                format!(
                    "Selected Image:\nName: {}\nPosition: (Col {:.1}, Row {:.1})\nSize: {:.1} x {:.1} cells\nResolution: {} x {} px",
                    filename, img.position.0, img.position.1,
                    img.size.0, img.size.1,
                    img.pixel_width, img.pixel_height
                )
            } else {
                "No Object Selected".to_string()
            }
        } else {
            "No Object Selected".to_string()
        };

        let text_changed = {
            let current_text = self.control_panel_label.base().label.as_ref();
            current_text != Some(&label_text)
        };
        if text_changed {
            self.control_panel_label.set_text(&label_text);
            self.needs_rebuild = true;
        }

        if self.dropdown_file.popover_rect().is_some() {
            // ui_context ONLY (the 6l pattern): the popover is drawn in the display list by
            // the walk; a global registration spawns a render-only xdg popup that swallows
            // clicks on the open menu.
            self.ui_context.register_popover(&mut self.dropdown_file);
        }
        if self.dropdown_edit.popover_rect().is_some() {
            // ui_context ONLY (the 6l pattern): the popover is drawn in the display list by
            // the walk; a global registration spawns a render-only xdg popup that swallows
            // clicks on the open menu.
            self.ui_context.register_popover(&mut self.dropdown_edit);
        }
        if self.dropdown_view.popover_rect().is_some() {
            // ui_context ONLY (the 6l pattern): the popover is drawn in the display list by
            // the walk; a global registration spawns a render-only xdg popup that swallows
            // clicks on the open menu.
            self.ui_context.register_popover(&mut self.dropdown_view);
        }

        let size_changed = self.width != size.width as u32 || self.height != size.height as u32 || self.scale_factor != scale;
        if self.needs_rebuild || size_changed || is_first_layout {
            self.width = size.width as u32;
            self.height = size.height as u32;
            self.scale_factor = scale;
            
            // Layout MenuBar at the top
            self.menu_bar.set_rect(0.0, 0.0, size.width, 42.0);
            
            // The File/Edit/View dropdown row, laid out directly (the transparent layout
            // Plate is DISSOLVED): a row at x=10/y=8 with 10px gaps, each dropdown sized
            // to its label via measure (the same intrinsic sizes the scene solver used).
            {
                let mut x = 10.0;
                let self_ptr = self as *mut Self;
                let dds: [&mut cce_ui::widget::Adapted<Dropdown>; 3] = unsafe {
                    [&mut (*self_ptr).dropdown_file, &mut (*self_ptr).dropdown_edit, &mut (*self_ptr).dropdown_view]
                };
                for dd in dds {
                    // Same sizing rule the retired scene bridge used: the dropdown's intrinsic
                    // size (widest option x configured dropdown height).
                    let sz = dd.intrinsic_size()
                        .unwrap_or(cce_ui::scene::layout::Size::new(70.0, 26.0));
                    dd.set_rect(x, 8.0, sz.width, sz.height);
                    x += sz.width + 10.0;
                }
            }

            // Layout Graph below MenuBar
            self.graph.set_rect(0.0, 42.0, size.width, size.height - 42.0);

            // Initial control panel positioning (bounds are clamped at drag time)
            if size_changed || is_first_layout {
                self.panel_x = (size.width - 230.0).max(10.0);
                self.panel_y = 55.0; // Float below MenuBar
            }
            self.position_panel_label();
            
            self.needs_rebuild = false;

            self.ui_context.rebuild_spatial_grid();
        }

        // 1. The dissolved root Backplate's plate, then the top-level widgets walked in the
        // old child order (menu bar, dropdown row, graph canvas, control panel on top).
        let mut pc = cce_ui::scene::paint::PaintCtx::new();
        {
            use cce_ui::scene::layout::Rect;
            let mut plate_color = cce_ui::color::page_low_color();
            if plate_color[3] > 0.001 {
                plate_color[3] = cce_ui::color::root_plate_opacity();
            }
            let rect = Rect { x: 0.0, y: 0.0, width: self.width as f32, height: self.height as f32 };
            // Silhouette radius (cce-ui RFC 7b): matches the compositor clip.
            let radius = cce_ui::layout::window_silhouette_radius();
            if radius > 0.1 {
                pc.rounded_rect(rect, radius, (true, true, true, true), plate_color);
            } else if plate_color[3] > 0.001 {
                pc.quad(rect, plate_color);
            }
        }
        {
            // The walk takes shared borrows now — no self-alias, no pointers.
            let tops: [&dyn cce_ui::widget::WidgetHost; 5] = [
                &self.menu_bar,
                &self.dropdown_file,
                &self.dropdown_edit,
                &self.dropdown_view,
                &self.graph,
            ];
            for top in tops {
                cce_ui::scene::painter::paint_root_into(&self.ui_context, top, &mut pc);
            }
        }

        // The dissolved control panel, on top: its plate as prims, then the label walked.
        if self.show_control_panel {
            use cce_ui::scene::layout::Rect;
            let (px, py, pw, ph) = self.panel_rect();
            let rect = Rect { x: px, y: py, width: pw, height: ph };
            let (fill, border, radius) = self.panel_visual();
            let radii = (radius, radius, radius, radius);
            if let Some((bc, thickness)) = border {
                pc.border(rect, radii, fill, bc, thickness);
            } else if fill[3].abs() > 0.001 {
                if radius > 0.1 {
                    pc.rounded_rect(rect, radius, (true, true, true, true), fill);
                } else {
                    pc.quad(rect, fill);
                }
            }
            cce_ui::scene::painter::paint_root_into(&self.ui_context, &self.control_panel_label, &mut pc);
        }

        // Draw foreground images in the graph grid (above nodes, fully opaque, preserving aspect ratio)
        let (grid_origin_x, grid_origin_y) = self.graph.grid_origin();
        let (grid_size_x, grid_size_y) = self.graph.grid_sizes();
        let (skipped_row_h, skipped_col_w) = self.graph.skipped_sizes();
        
        let step_x = grid_size_x + skipped_col_w;
        let step_y = grid_size_y + skipped_row_h;

        let (graph_x, graph_y, graph_w, graph_h) = self.graph.rect();
        let min_x = graph_x;
        let min_y = graph_y;
        let max_x = graph_x + graph_w;
        let max_y = graph_y + graph_h;

        let push_clipped = |qx: f32, qy: f32, qw: f32, qh: f32, qc: [f32; 4], q: &mut cce_ui::scene::paint::PaintCtx| {
            let rx1 = qx.max(min_x);
            let ry1 = qy.max(min_y);
            let rx2 = (qx + qw).min(max_x);
            let ry2 = (qy + qh).min(max_y);
            let rw = rx2 - rx1;
            let rh = ry2 - ry1;
            if rw > 0.0 && rh > 0.0 {
                q.quad(cce_ui::scene::layout::Rect { x: rx1, y: ry1, width: rw, height: rh }, qc);
            }
        };

        for (i, img) in self.loaded_images.iter().enumerate() {
            let col = img.position.0;
            let row = img.position.1;
            
            let screen_x = grid_origin_x + col * step_x;
            let screen_y = grid_origin_y + row * step_y;
            
            let screen_w = img.size.0 * grid_size_x + (img.size.0 - 1.0).max(0.0) * skipped_col_w;
            let aspect = img.pixel_height as f32 / img.pixel_width as f32;
            let screen_h = screen_w * aspect;
            
            let px_w = screen_w / img.pixel_width as f32;
            let px_h = screen_h / img.pixel_height as f32;
            
            for y in 0..img.pixel_height {
                for x in 0..img.pixel_width {
                    let idx = (y * img.pixel_width + x) as usize;
                    let rgba = img.pixels[idx];
                    let r = rgba[0] as f32 / 255.0;
                    let g = rgba[1] as f32 / 255.0;
                    let b = rgba[2] as f32 / 255.0;
                    let a = rgba[3] as f32 / 255.0;
                    
                    let px_x = screen_x + (x as f32) * px_w;
                    let px_y = screen_y + (y as f32) * px_h;
                    
                    push_clipped(px_x, px_y, px_w + 0.5, px_h + 0.5, [r, g, b, a], &mut pc);
                }
            }

            if Some(i) == self.selected_image_idx {
                let border_thickness = 2.0;
                let border_color = [0.0, 0.75, 1.0, 1.0]; // Vibrant cyan selection outline
                
                // Top border
                push_clipped(screen_x - border_thickness, screen_y - border_thickness, screen_w + 2.0 * border_thickness, border_thickness, border_color, &mut pc);
                // Bottom border
                push_clipped(screen_x - border_thickness, screen_y + screen_h, screen_w + 2.0 * border_thickness, border_thickness, border_color, &mut pc);
                // Left border
                push_clipped(screen_x - border_thickness, screen_y, border_thickness, screen_h, border_color, &mut pc);
                // Right border
                push_clipped(screen_x + screen_w, screen_y, border_thickness, screen_h, border_color, &mut pc);
            }
        }

        // Open dropdown popovers — geometry and labels last, on top of everything, exactly
        // where they hit-test (the ui_context registration above is occlusion/routing only;
        // nothing else paints them). Labels carry bounds equal to the popover rect, which
        // clips them to the plate and exempts them from the dl-text occlusion clamp.
        {
            use cce_ui::scene::layout::Rect;
            for &pop_id in &self.ui_context.active_popovers {
                let Some(pop_ptr) = self.ui_context.tree.get_ptr(pop_id) else { continue };
                let popover = unsafe { &*pop_ptr };
                if popover.popover_rect().is_none() {
                    continue;
                }
                // PaintCtx is a RenderTarget: the popover draws its real prims
                // (the dropdown's expanded inset-plate surface) with its own
                // per-label bounds — no flattening collector round-trip.
                popover.render_popover(&mut pc);
            }
            if cce_ui::widget::context_menu::is_visible() {
                let menu_bounds = Some([
                    cce_ui::widget::context_menu::x(),
                    cce_ui::widget::context_menu::y(),
                    cce_ui::widget::context_menu::x() + cce_ui::widget::context_menu::w(),
                    cce_ui::widget::context_menu::y() + cce_ui::widget::context_menu::h(),
                ]);
                for (qx, qy, qw, qh, qc) in cce_ui::widget::context_menu::extra_quads() {
                    pc.quad(Rect { x: qx, y: qy, width: qw, height: qh }, qc);
                }
                for label in cce_ui::widget::context_menu::text_labels() {
                    pc.text_with(
                        label.text.clone(),
                        label.x,
                        label.y,
                        label.font_size,
                        label.color,
                        None,
                        menu_bounds,
                    );
                }
            }
        }

        Some(pc.finish())
    }

    fn display_list_text(&self) -> bool {
        true
    }

    fn handle_pointer_move(&mut self, pos: LogicalPosition, needs_rebuild: &mut bool) {
        let mut changed = false;

        // The global config context menu (right-click on a dropdown) gets the pointer
        // exclusively while open — same priority the popovers get below.
        if cce_ui::widget::context_menu::is_visible() {
            if cce_ui::widget::context_menu::cursor_moved(pos.x, pos.y) {
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            return;
        }

        let over_menu = self.dropdown_file.open || self.dropdown_edit.open || self.dropdown_view.open
            || self.dropdown_file.hit_test(pos.x, pos.y, &self.ui_context)
            || self.dropdown_edit.hit_test(pos.x, pos.y, &self.ui_context)
            || self.dropdown_view.hit_test(pos.x, pos.y, &self.ui_context)
            || self.menu_bar.hit_test(pos.x, pos.y, &self.ui_context);

        // Routed dispatch (6bd shrink): one Event through the router per root; the panel
        // and loaded-image drags stay app-owned (they are not widgets).
        let mv = Event::PointerMove { x: pos.x, y: pos.y, local_x: pos.x, local_y: pos.y };
        if over_menu {
            let dd_roots = [self.dropdown_file.id(), self.dropdown_edit.id(), self.dropdown_view.id()];
            for root in dd_roots {
                if self.ui_context.propagate_event(&mv, root) { changed = true; }
            }
        } else {
            let mut handled_by_panel = false;
            if self.show_control_panel {
                if self.panel_dragging {
                    // Dissolved Plate drag: move within the graph area's bounds.
                    let w = self.width as f32;
                    let h = self.height as f32;
                    let nx = (pos.x - self.panel_drag_ox).clamp(0.0, (w - 210.0).max(0.0));
                    let ny = (pos.y - self.panel_drag_oy).clamp(42.0, (42.0 + (h - 42.0) - 160.0).max(42.0));
                    if (nx - self.panel_x).abs() > 0.01 || (ny - self.panel_y).abs() > 0.01 {
                        self.panel_x = nx;
                        self.panel_y = ny;
                        self.position_panel_label();
                        changed = true;
                    }
                    handled_by_panel = true;
                } else if self.panel_hit(pos.x, pos.y) {
                    handled_by_panel = true;
                }
            }

            if !handled_by_panel {
                // Otherwise feed it to Graph
                if let Some(img_idx) = self.dragging_image_idx {
                    let (grid_origin_x, grid_origin_y) = self.graph.grid_origin();
                    let (grid_size_x, grid_size_y) = self.graph.grid_sizes();
                    let (skipped_row_h, skipped_col_w) = self.graph.skipped_sizes();
                    let step_x = grid_size_x + skipped_col_w;
                    let step_y = grid_size_y + skipped_row_h;

                    let nx = pos.x - self.drag_image_ox;
                    let ny = pos.y - self.drag_image_oy;

                    let mut col = (nx - grid_origin_x) / step_x;
                    let mut row = (ny - grid_origin_y) / step_y;

                    if self.graph.grid_snap_enabled() {
                        col = (col * 2.0).round() / 2.0;
                        row = (row * 2.0).round() / 2.0;
                    }

                    if let Some(img) = self.loaded_images.get_mut(img_idx) {
                        if (img.position.0 - col).abs() > 0.001 || (img.position.1 - row).abs() > 0.001 {
                            img.position = (col, row);
                            changed = true;
                        }
                    }
                } else {
                    // The router forwards DragUpdate to a mid-drag node grab; a plain move
                    // runs the hover recompute. Node positions change without the propagate
                    // reporting it — rebuild every move while a drag is live.
                    let g = self.graph.id();
                    if self.ui_context.propagate_event(&mv, g) {
                        changed = true;
                    }
                    if self.ui_context.is_dragging {
                        changed = true;
                    }
                }
            } else {
                let clear = Event::PointerMove { x: -1000.0, y: -1000.0, local_x: -1000.0, local_y: -1000.0 };
                let g = self.graph.id();
                if self.ui_context.propagate_event(&clear, g) { changed = true; }
            }

            // Clear hover states if cursor moved away
            let dd_roots = [self.dropdown_file.id(), self.dropdown_edit.id(), self.dropdown_view.id()];
            for root in dd_roots {
                if self.ui_context.propagate_event(&mv, root) { changed = true; }
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

        // An open config context menu swallows the click (select or dismiss) before any
        // widget routing.
        if cce_ui::widget::context_menu::is_visible() {
            if cce_ui::widget::context_menu::mouse_input(button, state, pos.x, pos.y, Some(&mut self.ui_context)) {
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            return None;
        }

        let over_menu = self.dropdown_file.open || self.dropdown_edit.open || self.dropdown_view.open
            || self.dropdown_file.hit_test(pos.x, pos.y, &self.ui_context)
            || self.dropdown_edit.hit_test(pos.x, pos.y, &self.ui_context)
            || self.dropdown_view.hit_test(pos.x, pos.y, &self.ui_context)
            || self.menu_bar.hit_test(pos.x, pos.y, &self.ui_context);

        let ev = Event::MouseButton { button, state, x: pos.x, y: pos.y, local_x: pos.x, local_y: pos.y };
        if over_menu {
            let dd_file = self.dropdown_file.id();
            if self.ui_context.propagate_event(&ev, dd_file) {
                changed = true;
                if self.dropdown_file.take_change() {
                    let selected_idx = self.dropdown_file.selected;
                    if selected_idx < self.dropdown_file.options.len() {
                        let option_text = &self.dropdown_file.options[selected_idx];
                        match option_text.as_str() {
                            "New" => msg_out = Some(AppMessage::New),
                            "Open" | "Open..." => msg_out = Some(AppMessage::Open),
                            "Save" => msg_out = Some(AppMessage::Save),
                            "Save As" => msg_out = Some(AppMessage::SaveAs),
                            "Exit" => msg_out = Some(AppMessage::Exit),
                            "-" => {}
                            _ => {
                                let path = std::path::PathBuf::from(option_text);
                                msg_out = Some(AppMessage::OpenRecent(path));
                            }
                        }
                    }
                    self.dropdown_file.selected = 999;
                }
            }
            let dd_edit = self.dropdown_edit.id();
            if self.ui_context.propagate_event(&ev, dd_edit) {
                changed = true;
                if self.dropdown_edit.take_change() {
                    let idx = self.dropdown_edit.selected;
                    match idx {
                        0 => msg_out = Some(AppMessage::AddNode),
                        1 => msg_out = Some(AppMessage::AddImage),
                        _ => {}
                    }
                    self.dropdown_edit.selected = 999;
                }
            }
            let dd_view = self.dropdown_view.id();
            if self.ui_context.propagate_event(&ev, dd_view) {
                changed = true;
                if self.dropdown_view.take_change() {
                    let idx = self.dropdown_view.selected;
                    match idx {
                        0 => msg_out = Some(AppMessage::ToggleGrid),
                        1 => msg_out = Some(AppMessage::ToggleUniformBackground),
                        2 => msg_out = Some(AppMessage::SetOpacity95),
                        3 => msg_out = Some(AppMessage::SetOpacity75),
                        4 => msg_out = Some(AppMessage::SetOpacity50),
                        5 => msg_out = Some(AppMessage::ToggleControlPanel),
                        _ => {}
                    }
                    self.dropdown_view.selected = 999;
                }
            }

            // Outside-press dismissal. The engine already sweeps open popovers
            // before app dispatch (close_popovers_missed_by_press), but only for
            // Left — and Dropdown's own handler matches Left only too, so other
            // buttons would leave an open menu stranded. Run the same sweep for
            // those. Forcing `open = false` here instead (as this used to) skips
            // the contract animation entirely: both `Paint::popover` and
            // `draw_popover` gate on `open`, so the menu vanished in one frame
            // while `closing` ran on invisibly.
            if state == ElementState::Pressed && button != MouseButton::Left {
                self.ui_context.close_popovers_missed_by_press(pos.x, pos.y);
            }
        } else {
            let mut handled_by_panel = false;
            if self.show_control_panel && (self.panel_dragging || self.panel_hit(pos.x, pos.y)) {
                if button == MouseButton::Left {
                    match state {
                        ElementState::Pressed => {
                            self.panel_dragging = true;
                            self.panel_drag_ox = pos.x - self.panel_x;
                            self.panel_drag_oy = pos.y - self.panel_y;
                            changed = true;
                        }
                        ElementState::Released => {
                            if self.panel_dragging {
                                self.panel_dragging = false;
                                changed = true;
                            }
                        }
                    }
                }
                handled_by_panel = true;
            }

            if !handled_by_panel {
                // Otherwise route to Graph
                if button == MouseButton::Left {
                    if state == ElementState::Pressed {
                        // Routed press: a node grab records the drag target; the router
                        // synthesizes DragStart past its threshold (the old immediate
                        // drag_begin call).
                        let g = self.graph.id();
                        if self.ui_context.propagate_event(&ev, g) {
                            self.selected_image_idx = None;
                            changed = true;
                        } else if let Some(img_idx) = self.hit_test_image(pos.x, pos.y) {
                            let img = &self.loaded_images[img_idx];
                            let (grid_origin_x, grid_origin_y) = self.graph.grid_origin();
                            let (grid_size_x, grid_size_y) = self.graph.grid_sizes();
                            let (skipped_row_h, skipped_col_w) = self.graph.skipped_sizes();
                            let step_x = grid_size_x + skipped_col_w;
                            let step_y = grid_size_y + skipped_row_h;

                            let img_screen_x = grid_origin_x + img.position.0 * step_x;
                            let img_screen_y = grid_origin_y + img.position.1 * step_y;

                            self.dragging_image_idx = Some(img_idx);
                            self.drag_image_ox = pos.x - img_screen_x;
                            self.drag_image_oy = pos.y - img_screen_y;
                            self.selected_image_idx = Some(img_idx);
                            self.graph.set_selected_node(None);
                            changed = true;
                        } else {
                            self.selected_image_idx = None;
                            self.graph.set_selected_node(None);
                            changed = true;
                        }
                    } else if state == ElementState::Released {
                        if self.dragging_image_idx.is_some() {
                            self.dragging_image_idx = None;
                            changed = true;
                        } else {
                            // The router delivers DragEnd (commit) before the release
                            // reaches Graph; a committed drag leaves the release arm inert.
                            let was_dragging = self.ui_context.is_dragging;
                            let g = self.graph.id();
                            if self.ui_context.propagate_event(&ev, g) || was_dragging {
                                changed = true;
                            }
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
        let ev = Event::MouseWheel { delta: *delta, x: pos.x as f32, y: pos.y as f32, local_x: pos.x as f32, local_y: pos.y as f32 };
        let g = self.graph.id();
        if self.ui_context.propagate_event(&ev, g) {
            *needs_rebuild = true;
            self.needs_rebuild = true;
        }
    }

    fn handle_key_input(&mut self, event: &KeyEvent, needs_rebuild: &mut bool) -> Option<Self::Message> {
        if event.state == ElementState::Pressed && cce_ui::widget::context_menu::is_visible() {
            cce_ui::widget::context_menu::hide();
            *needs_rebuild = true;
            self.needs_rebuild = true;
            return None;
        }

        if event.state == ElementState::Pressed {
            // input.kdl `cce-graph.delete_node`, falling back to the legacy
            // config.kdl graph `delete` prop.
            let delete_keybind = cce_ui::input::app_chord("delete_node", &cce_ui::layout::graph_node_delete());
            if matches_keybind(event, &delete_keybind) {
                self.delete_selected_node();
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
        }
        None
    }
}

fn load_config() -> (bool, bool, bool, f32, f32) {
    let path = cce_ui::config::get_config_path();
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let val = cce_ui::config::parse_kdl_to_json(&content);
    
    let show_grid = val.pointer("/layout/graph_show_grid").and_then(|v| v.as_bool()).unwrap_or(true);
    let snap_enabled = val.pointer("/layout/graph_snap_enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let uniform_background = val.pointer("/style/surface/graph/uniform_background").and_then(|v| v.as_bool()).unwrap_or(false);
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
