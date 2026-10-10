use cce_ui::widget::Handle;
use cce_ui::engine::{Application, LogicalPosition, LogicalSize, WindowSettings};
mod linkgraph;
mod vault;
mod wiring;

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
    SetOpacity95,
    SetOpacity75,
    SetOpacity50,
    ToggleControlPanel,
    AddNode,
    AddImage,
    AddImageFromPath(std::path::PathBuf),
    /// Load a project the user picked, already past the unsaved-changes
    /// check (`OpenRecent` is the menu's, and is checked).
    Load(std::path::PathBuf),
    /// The unsaved-changes prompt's answer, for the action it held back.
    Answered(Answer, Discarding),
}

/// An action that would drop the open document, held back while the user
/// is asked about unsaved changes (`GraphApp::guard`).
#[derive(Debug, Clone)]
enum Discarding {
    New,
    Open,
    OpenPath(std::path::PathBuf),
    Exit,
}

/// What the unsaved-changes prompt answered.
#[derive(Debug, Clone)]
enum Answer {
    /// Save first: in place (`None`), or to the path just picked.
    Save(Option<std::path::PathBuf>),
    Discard,
    Cancel,
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
    opacity: f32,
}

struct LoadedImage {
    path: String,
    position: (f32, f32),
    size: (f32, f32),
    /// RGBA8, `pixel_width` x `pixel_height`: kept to upload again if the
    /// renderer is rebuilt (its image ids die with it).
    pixels: Vec<u8>,
    pixel_width: u32,
    pixel_height: u32,
    /// The GPU image and the renderer epoch it was uploaded in; uploaded at
    /// first draw (`display_list`), freed on drop.
    texture: Option<(u32, u32)>,
}

impl LoadedImage {
    fn new(path: String, position: (f32, f32), size: (f32, f32), (pixels, pixel_width, pixel_height): (Vec<u8>, u32, u32)) -> Self {
        LoadedImage { path, position, size, pixels, pixel_width, pixel_height, texture: None }
    }

    /// The image id to draw by, uploading the pixels when this renderer has
    /// not got them yet.
    fn texture_id(&mut self) -> u32 {
        let epoch = cce_ui::draw::renderer_epoch();
        match self.texture {
            Some((id, e)) if e == epoch => id,
            _ => {
                let id = cce_ui::draw::upload_rgba_mipmapped(self.pixels.clone(), self.pixel_width, self.pixel_height);
                self.texture = Some((id, epoch));
                id
            }
        }
    }
}

impl Drop for LoadedImage {
    fn drop(&mut self) {
        // An id from an earlier renderer named nothing once it went.
        if let Some((id, epoch)) = self.texture {
            if epoch == cce_ui::draw::renderer_epoch() {
                cce_ui::draw::free_image(id);
            }
        }
    }
}

struct GraphApp {
    menu_bar: Handle<Adapted<MenuBar>>,
    dropdown_file: Handle<Adapted<Dropdown>>,
    dropdown_edit: Handle<Adapted<Dropdown>>,
    dropdown_view: Handle<Adapted<Dropdown>>,

    graph: Handle<Adapted<Graph>>,
    needs_rebuild: bool,
    width: u32,
    height: u32,
    scale_factor: f64,
    show_grid: bool,
    opacity: f32,
    ui_context: cce_ui::context::UiContext,
    loaded_project_path: Option<std::path::PathBuf>,
    /// Set when the document came from a bare KDL state file (`default.kdl`,
    /// or any other `.kdl` but `state.kdl` opened directly) rather than a
    /// project directory: Save writes back to it. `loaded_project_path` is
    /// then its parent, which relative image paths resolve against.
    state_file: Option<std::path::PathBuf>,
    loaded_images: Vec<LoadedImage>,
    /// The document as last loaded or saved (`snapshot`): it has unsaved
    /// changes while the current one differs.
    saved_snapshot: String,
    /// The unsaved-changes prompt is up; further guarded actions wait.
    asking: bool,
    /// Whether the first layout has run.
    laid_out: bool,
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
    /// The user has dragged the panel: a resize keeps it where it was put
    /// rather than snapping it back to the top-right.
    panel_moved: bool,
    panel_drag_ox: f32,
    panel_drag_oy: f32,
    show_control_panel: bool,
    control_panel_label: Handle<cce_ui::widget::Adapted<cce_ui::widget::Label>>,
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
    // (`name "default"`, `show_grid true`, `opacity 0.95` — see
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
        opacity,
    })
}

/// A KDL string literal, escaped by the kdl crate itself. Rust's `{:?}` is not
/// KDL: it writes `\0` and friends, which KDL rejects, losing the whole file.
fn kdl_str(s: &str) -> String {
    kdl::KdlValue::String(s.to_string()).to_string()
}

/// A KDL number. `{:?}` keeps a fraction or exponent, so a huge value cannot
/// print as an integer too wide for the parser; a non-finite one (which would
/// print `inf`/`NaN`, not KDL either) is written as 0.
fn kdl_num(v: f32) -> String {
    if v.is_finite() { format!("{v:?}") } else { "0.0".to_string() }
}

fn save_project_to_kdl_path(path: &std::path::Path, state: &GraphProjectState) -> Result<(), Box<dyn std::error::Error>> {
    let kdl = project_to_kdl(state);
    // Write beside it and rename over it, so a crash mid-write leaves the
    // previous save intact rather than a truncated file.
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{file_name}.tmp"));
    std::fs::write(&tmp, kdl)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The state file's text — also the unsaved-changes snapshot.
fn project_to_kdl(state: &GraphProjectState) -> String {
    let mut kdl = String::new();
    kdl.push_str(&format!("name {}\n", kdl_str(&state.name)));
    kdl.push_str(&format!("show_grid {}\n", state.show_grid));
    kdl.push_str(&format!("opacity {}\n\n", kdl_num(state.opacity)));

    for node in &state.nodes {
        kdl.push_str(&format!("node {} {{\n", kdl_str(&node.name)));
        kdl.push_str(&format!("    position {} {}\n", kdl_num(node.position.0), kdl_num(node.position.1)));
        kdl.push_str(&format!("    inputs {}\n", node.inputs));
        kdl.push_str(&format!("    outputs {}\n", node.outputs));
        kdl.push_str(&format!("    geom_visible {}\n", node.geom_visible));
        if !node.node_type.is_empty() {
            kdl.push_str(&format!("    node_type {}\n", kdl_str(&node.node_type)));
        }
        for (p_name, p_val, p_type) in &node.parameters {
            kdl.push_str(&format!(
                "    parameter {} value={} type={}\n",
                kdl_str(p_name),
                kdl_str(p_val),
                kdl_str(p_type)
            ));
        }
        kdl.push_str("}\n\n");
    }

    for img in &state.images {
        kdl.push_str(&format!("image {} {{\n", kdl_str(&img.path)));
        kdl.push_str(&format!("    position {} {}\n", kdl_num(img.position.0), kdl_num(img.position.1)));
        kdl.push_str(&format!("    size {} {}\n", kdl_num(img.size.0), kdl_num(img.size.1)));
        kdl.push_str("}\n\n");
    }
    kdl
}

/// Whether `path` is a bare KDL state file rather than a project directory
/// or the `state.kdl`/`state.json` inside one.
fn is_bare_state_file(path: &std::path::Path) -> bool {
    !path.is_dir()
        && path.extension().is_some_and(|e| e == "kdl")
        && path.file_name().is_some_and(|n| n != "state.kdl")
}

/// True when both paths exist and name the same file.
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// Copy an image into `project_dir/assets` and return the relative path to
/// save. A file already in that folder is referenced where it is: copying a
/// file onto itself truncates it to nothing. A different file with a name
/// already taken gets a numbered one rather than overwriting it.
fn import_asset(project_dir: &std::path::Path, src: &std::path::Path) -> std::io::Result<String> {
    let dest_dir = project_dir.join("assets");
    std::fs::create_dir_all(&dest_dir)?;
    let name = src.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image.png".to_string());
    let src_dir = src.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
    if same_file(src_dir, &dest_dir) {
        return Ok(format!("assets/{name}"));
    }
    let as_path = std::path::Path::new(&name);
    let stem = as_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = as_path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut dest_name = name.clone();
    let mut n = 2;
    while dest_dir.join(&dest_name).exists() {
        dest_name = format!("{stem}-{n}{ext}");
        n += 1;
    }
    std::fs::copy(src, dest_dir.join(&dest_name))?;
    Ok(format!("assets/{dest_name}"))
}

/// Bring the relative image paths a document references from the folder they
/// resolve against now to the one they will resolve against after a save
/// elsewhere (Save As) — without this the new project's `assets/` is empty
/// and every image drops out on the next load. A source that is already gone
/// is skipped with a warning rather than blocking the save.
fn copy_relative_assets<'a>(
    from_dir: &std::path::Path,
    to_dir: &std::path::Path,
    paths: impl IntoIterator<Item = &'a str>,
) -> std::io::Result<()> {
    if same_file(from_dir, to_dir) {
        return Ok(());
    }
    for rel in paths {
        let rel = std::path::Path::new(rel);
        if rel.is_absolute() {
            continue;
        }
        let (src, dest) = (from_dir.join(rel), to_dir.join(rel));
        if !src.exists() {
            eprintln!("Warning: image {:?} is missing; not copied to {:?}", src, to_dir);
            continue;
        }
        if same_file(&src, &dest) {
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dest)?;
    }
    Ok(())
}

/// The View menu's rows, dispatched by INDEX (`drain_view_menu`). Its list
/// is a `Dropdown`, which draws the context menu's marks as glyphs: the two
/// switches wear `MARK_CHECK` while on, and the opacities are a radio group
/// (`MARK_ON` on the one in use, `MARK_OFF` on the rest).
fn get_view_options(show_grid: bool, opacity: f32, show_panel: bool) -> Vec<String> {
    use cce_ui::widget::context_menu::{MARK_CHECK, MARK_OFF, MARK_ON};
    let switch = |on: bool, label: &str| format!("{}{label}", if on { MARK_CHECK } else { "" });
    let opacity_row = |pct: u32, value: f32| {
        let mark = if (opacity - value).abs() < 0.05 { MARK_ON } else { MARK_OFF };
        format!("{mark}Opacity {pct}%")
    };
    vec![
        switch(show_grid, "Show Grid"),
        opacity_row(95, 0.95),
        opacity_row(75, 0.75),
        opacity_row(50, 0.50),
        switch(show_panel, "Control Panel"),
    ]
}

/// Longest side an image is kept at. It is drawn as one GPU image (mipmapped),
/// so this bounds memory, not draw cost.
const MAX_IMAGE_DIM: u32 = 1024;

/// Decode an image to RGBA8, shrunk to fit `MAX_IMAGE_DIM` (never enlarged).
fn load_image_pixels(path: &std::path::Path) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::open(path).ok()?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let scale = (MAX_IMAGE_DIM as f32 / w.max(h) as f32).min(1.0);
    // At least a pixel each way: at an extreme aspect ratio the short side
    // rounds to 0, the aspect ratio goes infinite, and the size saved with
    // it is not KDL.
    let (nw, nh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
    let img = if (nw, nh) == (w, h) { img } else { img.resize_exact(nw, nh, image::imageops::FilterType::Triangle) };
    Some((img.to_rgba8().into_raw(), nw, nh))
}

impl GraphApp {
    /// The editor with its widgets built and nothing loaded — `create` then
    /// loads the startup project; tests start here.
    fn with_sender(sender: calloop::channel::Sender<AppMessage>) -> Self {
        let mut graph = Graph::new();

        let (show_grid, snap_enabled, opacity, gap_width) = load_config();

        // Configure initial grid settings on the graph
        graph.set_show_network_grid(show_grid);
        graph.set_grid_sizes(140.0, 70.0);
        graph.set_skipped_sizes(gap_width, gap_width);
        graph.set_grid_origin(60.0, 60.0);
        graph.set_grid_snap_enabled(snap_enabled);
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
            get_view_options(show_grid, opacity, false),
            999,
        )
        .with_custom_display_text("View");

        let show_control_panel = false;

        let control_panel_label = Label::new("No Node Selected")
            .with_font_size(12.0)
            .with_color([204, 204, 221]);

        // The context owns the widgets; the app keeps their handles.
        let mut ui_context = cce_ui::context::UiContext::new();
        let control_panel_label = ui_context.insert(control_panel_label);
        let mut app = Self {
            menu_bar: ui_context.insert(menu_bar),
            dropdown_file: ui_context.insert(dropdown_file),
            dropdown_edit: ui_context.insert(dropdown_edit),
            dropdown_view: ui_context.insert(dropdown_view),
            graph: ui_context.insert(graph),
            needs_rebuild: true,
            width: 1024,
            height: 768,
            scale_factor: 1.0,
            show_grid,
            opacity,
            ui_context,
            loaded_project_path: None,
            state_file: None,
            loaded_images: Vec::new(),
            saved_snapshot: String::new(),
            asking: false,
            laid_out: false,
            message_sender: sender,
            dragging_image_idx: None,
            drag_image_ox: 0.0,
            drag_image_oy: 0.0,
            selected_image_idx: None,
            panel_x: 800.0,
            panel_y: 50.0,
            panel_dragging: false,
            panel_moved: false,
            panel_drag_ox: 0.0,
            panel_drag_oy: 0.0,
            show_control_panel,
            control_panel_label,
        };

        app.ui_context[app.menu_bar].set_rect(0.0, 0.0, 1024.0, 42.0);
        let dd_h = cce_ui::layout::dropdown_height();
        let dd_y = (42.0 - dd_h) / 2.0;
        app.ui_context[app.dropdown_file].set_rect(10.0, dd_y, 70.0, dd_h);
        app.ui_context[app.dropdown_edit].set_rect(90.0, dd_y, 70.0, dd_h);
        app.ui_context[app.dropdown_view].set_rect(170.0, dd_y, 70.0, dd_h);
        app.ui_context[app.graph].set_rect(0.0, 42.0, 1024.0, 768.0 - 42.0);
        app.mark_saved();
        app
    }

    /// Drain a consumed file-menu interaction into its message — shared by the
    /// mouse path and the key path so a selection means the same thing however
    /// it was made. Keeps the sentinel `selected = 999` protocol.
    fn drain_file_menu(&mut self) -> Option<AppMessage> {
        let mut msg = None;
        if self.ui_context[self.dropdown_file].take_change() {
            let selected_idx = self.ui_context[self.dropdown_file].selected;
            if selected_idx < self.ui_context[self.dropdown_file].options.len() {
                let option_text = &self.ui_context[self.dropdown_file].options[selected_idx];
                match option_text.as_str() {
                    "New" => msg = Some(AppMessage::New),
                    "Open" | "Open..." => msg = Some(AppMessage::Open),
                    "Save" => msg = Some(AppMessage::Save),
                    "Save As" => msg = Some(AppMessage::SaveAs),
                    "Exit" => msg = Some(AppMessage::Exit),
                    "-" => {}
                    _ => {
                        let path = std::path::PathBuf::from(option_text);
                        msg = Some(AppMessage::OpenRecent(path));
                    }
                }
            }
            self.ui_context[self.dropdown_file].selected = 999;
        }
        msg
    }

    fn drain_edit_menu(&mut self) -> Option<AppMessage> {
        let mut msg = None;
        if self.ui_context[self.dropdown_edit].take_change() {
            match self.ui_context[self.dropdown_edit].selected {
                0 => msg = Some(AppMessage::AddNode),
                1 => msg = Some(AppMessage::AddImage),
                _ => {}
            }
            self.ui_context[self.dropdown_edit].selected = 999;
        }
        msg
    }

    fn drain_view_menu(&mut self) -> Option<AppMessage> {
        let mut msg = None;
        if self.ui_context[self.dropdown_view].take_change() {
            match self.ui_context[self.dropdown_view].selected {
                0 => msg = Some(AppMessage::ToggleGrid),
                1 => msg = Some(AppMessage::SetOpacity95),
                2 => msg = Some(AppMessage::SetOpacity75),
                3 => msg = Some(AppMessage::SetOpacity50),
                4 => msg = Some(AppMessage::ToggleControlPanel),
                _ => {}
            }
            self.ui_context[self.dropdown_view].selected = 999;
        }
        msg
    }

    fn delete_selected_node(&mut self) {
        if let Some(idx) = self.ui_context[self.graph].selected_node() {
            let mut nodes = self.ui_context[self.graph].get_nodes();
            if idx < nodes.len() {
                let deleted = nodes.remove(idx);
                wiring::disconnect(&mut nodes, &deleted.name);

                self.ui_context[self.graph].set_nodes(&nodes);
                self.ui_context[self.graph].set_selected_node(None);
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
        self.ui_context[self.dropdown_view].options = get_view_options(self.show_grid, self.opacity, self.show_control_panel);
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

        let (_, _, lw, lh) = self.ui_context[self.control_panel_label].rect();
        let use_w = if lw > 0.0 { lw.min(available_w) } else { available_w };
        let use_h = if lh > 0.0 { lh } else { 50.0 };
        let cx = (center_x - use_w / 2.0).clamp(left_x, (left_x + available_w - use_w).max(left_x));
        let cy = (center_y - use_h / 2.0).clamp(start_y, (start_y + available_h - use_h).max(start_y));
        let cw = use_w.min(px + pw - pad - cx);
        let ch = use_h.min(py + ph - pad - cy);
        self.ui_context[self.control_panel_label].set_rect(cx, cy, cw, ch);
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
        self.ui_context[self.graph].set_nodes(&[]);
        self.loaded_images.clear();
        self.loaded_project_path = None;
        self.state_file = None;
        self.clear_selection();
        self.mark_saved();
        self.needs_rebuild = true;
    }

    /// The document as saving it would write it, bar the name (which comes
    /// from where it is saved).
    fn snapshot(&self) -> String {
        project_to_kdl(&self.project_state(None))
    }

    fn mark_saved(&mut self) {
        self.saved_snapshot = self.snapshot();
    }

    fn is_dirty(&self) -> bool {
        self.snapshot() != self.saved_snapshot
    }

    /// Run `action` now if nothing would be lost; otherwise ask about the
    /// unsaved changes first and run it from the answer (`Answered`).
    fn guard(&mut self, action: Discarding) -> Option<Discarding> {
        if !self.is_dirty() {
            return Some(action);
        }
        if !self.asking {
            self.asking = true;
            self.ask_to_save(action);
        }
        None
    }

    /// The Save / Discard / Cancel prompt, on its own thread like the file
    /// dialogs (it blocks); the answer comes back as `Answered`.
    fn ask_to_save(&self, action: Discarding) {
        let sender = self.message_sender.clone();
        let name = self.state_file.as_ref().or(self.loaded_project_path.as_ref())
            .and_then(|p| p.file_name())
            .map(|n| format!("“{}”", n.to_string_lossy()))
            .unwrap_or_else(|| "this graph".to_string());
        let has_target = self.state_file.is_some() || self.loaded_project_path.is_some();
        std::thread::spawn(move || {
            let (save, discard) = ("Save".to_string(), "Discard".to_string());
            let choice = rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Warning)
                .set_title("Unsaved changes")
                .set_description(format!("Save the changes to {name} first?"))
                .set_buttons(rfd::MessageButtons::YesNoCancelCustom(save.clone(), discard.clone(), "Cancel".to_string()))
                .show();
            let answer = match choice {
                rfd::MessageDialogResult::Custom(c) if c == save => {
                    if has_target {
                        Answer::Save(None)
                    } else {
                        match cce_ui::file_dialog::save_file("Save CCE Graph Project", &[]) {
                            Some(path) => Answer::Save(Some(path)),
                            None => Answer::Cancel,
                        }
                    }
                }
                rfd::MessageDialogResult::Custom(c) if c == discard => Answer::Discard,
                _ => Answer::Cancel,
            };
            let _ = sender.send(AppMessage::Answered(answer, action));
        });
    }

    /// Carry out an action the unsaved-changes check let through.
    fn perform(&mut self, action: Discarding, exit: &mut bool) {
        match action {
            Discarding::New => self.new_project(),
            Discarding::Open => {
                let sender = self.message_sender.clone();
                std::thread::spawn(move || {
                    if let Some(path) = cce_ui::file_dialog::pick_file("Open CCE Graph Project", &[]) {
                        let _ = sender.send(AppMessage::Load(path));
                    }
                });
            }
            Discarding::OpenPath(path) => self.open_path(&path),
            Discarding::Exit => *exit = true,
        }
    }

    fn open_path(&mut self, path: &std::path::Path) {
        if let Err(e) = self.load_project_from_path(path) {
            eprintln!("Failed to load project: {:?}", e);
        } else {
            self.add_recent_file(path);
        }
    }

    /// Start a new document that Save will write to `path`, which does not
    /// exist yet: a bare state file for a `.kdl` path, else a project
    /// directory — as an editor given a new file name opens it empty.
    fn new_project_at(&mut self, path: &std::path::Path) {
        self.new_project();
        if path.file_name().is_some_and(|n| n == "state.kdl") {
            self.loaded_project_path = path.parent().map(|p| p.to_path_buf());
        } else if is_bare_state_file(path) {
            self.loaded_project_path = path.parent().map(|p| p.to_path_buf());
            self.state_file = Some(path.to_path_buf());
        } else {
            self.loaded_project_path = Some(path.to_path_buf());
        }
    }

    /// Forget what is selected or held. Selections are indices, so one kept
    /// across New/Open lands on whatever now sits there, and Delete deletes it.
    fn clear_selection(&mut self) {
        self.selected_image_idx = None;
        self.dragging_image_idx = None;
        self.ui_context[self.graph].set_selected_node(None);
    }

    /// The document as saved.
    fn project_state(&self, name: Option<&std::ffi::OsStr>) -> GraphProjectState {
        let images = self.loaded_images.iter()
            .map(|img| GraphProjectImage {
                path: img.path.clone(),
                position: img.position,
                size: img.size,
            })
            .collect();
        GraphProjectState {
            name: name
                .and_then(|n| n.to_str())
                .unwrap_or("Graph Project")
                .to_string(),
            nodes: self.ui_context[self.graph].get_nodes(),
            images,
            show_grid: self.show_grid,
            opacity: self.opacity,
        }
    }

    /// Save: back to the bare state file or project directory the document
    /// came from. The path saved to, or `None` when there is none yet (the
    /// caller asks for one).
    fn save_in_place(&mut self) -> Option<Result<std::path::PathBuf, Box<dyn std::error::Error>>> {
        if let Some(file) = self.state_file.clone() {
            let state = self.project_state(file.file_stem());
            let saved = file.parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .map_err(Into::into)
                .and_then(|()| save_project_to_kdl_path(&file, &state));
            if saved.is_ok() {
                self.mark_saved();
            }
            return Some(saved.map(|()| file));
        }
        let dir = self.loaded_project_path.clone()?;
        Some(self.save_project_to_path(&dir).map(|()| dir))
    }

    fn save_project_to_path(&mut self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let project_dir = path;
        std::fs::create_dir_all(project_dir)?;

        // Create assets and code subdirectories
        std::fs::create_dir_all(project_dir.join("assets"))?;
        std::fs::create_dir_all(project_dir.join("code"))?;

        // Save As: relative image paths must resolve in the new directory too.
        if let Some(old_dir) = self.loaded_project_path.clone() {
            copy_relative_assets(&old_dir, project_dir, self.loaded_images.iter().map(|i| i.path.as_str()))?;
        }

        let state_file_path = project_dir.join("state.kdl");
        let state = self.project_state(project_dir.file_name());
        save_project_to_kdl_path(&state_file_path, &state)?;

        // Clean up old state.json if it exists
        let old_json_path = project_dir.join("state.json");
        if old_json_path.exists() {
            let _ = std::fs::remove_file(old_json_path);
        }

        self.loaded_project_path = Some(project_dir.to_path_buf());
        self.state_file = None;
        self.mark_saved();
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
        self.ui_context[self.dropdown_file].options = options;
        self.ui_context[self.dropdown_file].selected = 999;
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
            (path.to_path_buf(), path.parent().unwrap_or(path).to_path_buf())
        };

        let state: GraphProjectState = if state_file_path.extension().map_or(false, |ext| ext == "kdl") {
            load_project_from_kdl_path(&state_file_path)?
        } else {
            let content = std::fs::read_to_string(&state_file_path)?;
            serde_json::from_str(&content)?
        };

        let mut nodes = state.nodes.clone();
        wiring::ensure_ids(&mut nodes);
        self.ui_context[self.graph].set_nodes(&nodes);
        self.clear_selection();
        self.show_grid = state.show_grid;
        self.opacity = state.opacity;

        // Load images
        self.loaded_images.clear();
        for img in state.images {
            let image_path = if std::path::Path::new(&img.path).is_absolute() {
                std::path::PathBuf::from(&img.path)
            } else {
                project_dir.join(&img.path)
            };

            if let Some(decoded) = load_image_pixels(&image_path) {
                self.loaded_images.push(LoadedImage::new(img.path.clone(), img.position, img.size, decoded));
            } else {
                eprintln!("Warning: Failed to load image at {:?}", image_path);
            }
        }

        // Apply grid/background settings to self.ui_context[self.graph]
        self.ui_context[self.graph].set_show_network_grid(self.show_grid);
        self.ui_context[self.graph].set_network_opacity(self.opacity);

        // Update view dropdown options
        self.update_view_options();

        self.loaded_project_path = Some(project_dir);
        self.state_file = is_bare_state_file(path).then(|| path.to_path_buf());
        self.mark_saved();
        self.needs_rebuild = true;
        Ok(())
    }

    /// Where an image is on screen, (x, y, w, h): placed by grid cell, as
    /// wide as its `size.0` columns, as tall as its aspect ratio makes it.
    fn image_screen_rect(&self, img: &LoadedImage) -> (f32, f32, f32, f32) {
        let (grid_origin_x, grid_origin_y) = self.ui_context[self.graph].grid_origin();
        let (grid_size_x, grid_size_y) = self.ui_context[self.graph].grid_sizes();
        let (skipped_row_h, skipped_col_w) = self.ui_context[self.graph].skipped_sizes();
        let x = grid_origin_x + img.position.0 * (grid_size_x + skipped_col_w);
        let y = grid_origin_y + img.position.1 * (grid_size_y + skipped_row_h);
        let w = img.size.0 * grid_size_x + (img.size.0 - 1.0).max(0.0) * skipped_col_w;
        let h = w * img.pixel_height as f32 / img.pixel_width as f32;
        (x, y, w, h)
    }

    /// The topmost image at a point.
    fn hit_test_image(&self, px: f32, py: f32) -> Option<usize> {
        self.loaded_images.iter().rposition(|img| {
            let (x, y, w, h) = self.image_screen_rect(img);
            px >= x && px <= x + w && py >= y && py <= y + h
        })
    }

    fn add_image(&mut self, src_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        // Decode first, so a file that is not an image is not copied in.
        let Some(decoded) = load_image_pixels(src_path) else {
            return Err(format!("{} is not an image cce-graph can read", src_path.display()).into());
        };
        let final_path = if let Some(ref project_dir) = self.loaded_project_path {
            import_asset(project_dir, src_path)?
        } else {
            src_path.to_string_lossy().to_string()
        };

        let aspect = decoded.2 as f32 / decoded.1 as f32;
        let size_w = 4.0;
        let size_h = size_w * aspect;

        let col = 2.0;
        let row = 2.0 + self.loaded_images.len() as f32 * 5.0;

        self.loaded_images.push(LoadedImage::new(final_path, (col, row), (size_w, size_h), decoded));
        self.needs_rebuild = true;
        Ok(())
    }
}

impl Application for GraphApp {
    type Message = AppMessage;

    fn ui_context(&self) -> Option<&cce_ui::context::UiContext> {
        Some(&self.ui_context)
    }

    fn is_movable_root_plate_at(&self, px: f32, py: f32) -> bool {
        // root plate container dissolved (Phase 6m): the surface itself is the movable plate; drag
        // anywhere a drag-blocking widget isn't.
        self.ui_context.drag_allowed_at(px, py)
    }

    fn ui_context_mut(&mut self) -> Option<&mut cce_ui::context::UiContext> {
        Some(&mut self.ui_context)
    }

    fn create(sender: cce_ui::engine::AppSender<Self::Message>) -> Self {
        // The app keeps calloop's sender; `AppSender` converts into it.
        let mut app = GraphApp::with_sender(sender.into());

        let args: Vec<String> = std::env::args().collect();
        if args.len() > 1 {
            let path = std::path::PathBuf::from(&args[1]);
            if path.exists() {
                if let Err(e) = app.load_project_from_path(&path) {
                    eprintln!("Failed to load project on startup: {:?}", e);
                } else {
                    app.add_recent_file(&path);
                }
            } else {
                // Not a typo to swallow silently: say so, and make it the
                // place Save writes.
                eprintln!("cce-graph: nothing at {}; starting a new graph there (Save creates it)", path.display());
                app.new_project_at(&path);
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
        if let Some(path) = self.state_file.as_ref().or(self.loaded_project_path.as_ref()) {
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
            AppMessage::New | AppMessage::Open | AppMessage::OpenRecent(_) | AppMessage::Exit => {
                let action = match msg {
                    AppMessage::New => Discarding::New,
                    AppMessage::Open => Discarding::Open,
                    AppMessage::OpenRecent(path) => Discarding::OpenPath(path),
                    _ => Discarding::Exit,
                };
                if let Some(action) = self.guard(action) {
                    self.perform(action, exit);
                }
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::Load(path) => {
                self.open_path(&path);
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::Answered(answer, action) => {
                self.asking = false;
                let proceed = match answer {
                    Answer::Cancel => false,
                    Answer::Discard => true,
                    Answer::Save(to) => {
                        let saved = match to {
                            Some(dir) => self.save_project_to_path(&dir).map(|()| dir),
                            None => self.save_in_place().unwrap_or_else(|| Err("nowhere to save to".into())),
                        };
                        match saved {
                            Ok(path) => {
                                if path != get_default_project_path() {
                                    self.add_recent_file(&path);
                                }
                                true
                            }
                            Err(e) => {
                                eprintln!("Failed to save project: {:?}", e);
                                false
                            }
                        }
                    }
                };
                if proceed {
                    self.perform(action, exit);
                }
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::Save => {
                if let Some(saved) = self.save_in_place() {
                    match saved {
                        Err(e) => eprintln!("Failed to save project: {:?}", e),
                        // The default document is where a bare launch lands
                        // anyway; it is not a recent file.
                        Ok(path) if path == get_default_project_path() => {}
                        Ok(path) => self.add_recent_file(&path),
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
            AppMessage::ToggleGrid => {
                self.show_grid = !self.show_grid;
                self.ui_context[self.graph].set_show_network_grid(self.show_grid);
                self.update_view_options();
                write_config_value("graph_show_grid", &self.show_grid.to_string());
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SetOpacity95 => {
                self.opacity = 0.95;
                self.ui_context[self.graph].set_network_opacity(0.95);
                self.update_view_options();
                write_config_value("graph_network_opacity", "0.95");
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SetOpacity75 => {
                self.opacity = 0.75;
                self.ui_context[self.graph].set_network_opacity(0.75);
                self.update_view_options();
                write_config_value("graph_network_opacity", "0.75");
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
            AppMessage::SetOpacity50 => {
                self.opacity = 0.50;
                self.ui_context[self.graph].set_network_opacity(0.50);
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
                let mut nodes = self.ui_context[self.graph].get_nodes();
                let next_id = nodes.len() + 1;
                let name = wiring::fresh_name(&nodes);
                nodes.push(GraphNode {
                    id: String::new(),
                    name,
                    position: (2.0 + (next_id % 3) as f32, 2.0 + (next_id / 3) as f32),
                    parameters: vec![],
                    geom_visible: true,
                    node_type: String::new(),
                    inputs: 1,
                    outputs: 1,
                });
                wiring::ensure_ids(&mut nodes);
                self.ui_context[self.graph].set_nodes(&nodes);
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

        let is_first_layout = !self.laid_out;
        self.laid_out = true;

        // Check selected node and update control panel label
        let selected_node_idx = self.ui_context[self.graph].selected_node();
        let label_text = if let Some(idx) = selected_node_idx {
            let nodes = self.ui_context[self.graph].get_nodes();
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
            let current_text = self.ui_context[self.control_panel_label].base().label.as_ref();
            current_text != Some(&label_text)
        };
        if text_changed {
            self.ui_context[self.control_panel_label].set_text(&label_text);
            self.needs_rebuild = true;
        }

        if self.ui_context[self.dropdown_file].popover_rect().is_some() {
            // ui_context ONLY (the 6l pattern): the popover is drawn in the display list by
            // the walk; a global registration spawns a render-only xdg popup that swallows
            // clicks on the open menu.
            self.ui_context.register_popover_id(self.dropdown_file.id());
        }
        if self.ui_context[self.dropdown_edit].popover_rect().is_some() {
            // ui_context ONLY (the 6l pattern): the popover is drawn in the display list by
            // the walk; a global registration spawns a render-only xdg popup that swallows
            // clicks on the open menu.
            self.ui_context.register_popover_id(self.dropdown_edit.id());
        }
        if self.ui_context[self.dropdown_view].popover_rect().is_some() {
            // ui_context ONLY (the 6l pattern): the popover is drawn in the display list by
            // the walk; a global registration spawns a render-only xdg popup that swallows
            // clicks on the open menu.
            self.ui_context.register_popover_id(self.dropdown_view.id());
        }

        let size_changed = self.width != size.width as u32 || self.height != size.height as u32 || self.scale_factor != scale;
        if self.needs_rebuild || size_changed || is_first_layout {
            self.width = size.width as u32;
            self.height = size.height as u32;
            self.scale_factor = scale;
            
            // Layout MenuBar at the top
            self.ui_context[self.menu_bar].set_rect(0.0, 0.0, size.width, 42.0);
            
            // The File/Edit/View dropdown row, laid out directly (the transparent layout
            // Plate is DISSOLVED): a row from x=10 with 10px gaps, centred in the 42px
            // bar, each dropdown sized to its label via measure (the same intrinsic
            // sizes the scene solver used).
            {
                let mut x = 10.0;
                for h in [self.dropdown_file, self.dropdown_edit, self.dropdown_view] {
                    let dd = &mut self.ui_context[h];
                    // Same sizing rule the retired scene bridge used: the dropdown's intrinsic
                    // size (widest option x configured dropdown height).
                    let sz = dd.intrinsic_size()
                        .unwrap_or(cce_ui::scene::layout::Size::new(70.0, cce_ui::layout::dropdown_height()));
                    dd.set_rect(x, (42.0 - sz.height) / 2.0, sz.width, sz.height);
                    x += sz.width + 10.0;
                }
            }

            // Layout Graph below MenuBar
            self.ui_context[self.graph].set_rect(0.0, 42.0, size.width, size.height - 42.0);

            // The control panel sits top-right until the user moves it; after
            // that a resize keeps it where it was put, pulled back inside.
            if !self.panel_moved && (size_changed || is_first_layout) {
                self.panel_x = (size.width - 230.0).max(10.0);
                self.panel_y = 55.0; // Float below MenuBar
            } else if size_changed {
                let (_, _, pw, ph) = self.panel_rect();
                self.panel_x = self.panel_x.clamp(0.0, (size.width - pw).max(0.0));
                self.panel_y = self.panel_y.clamp(42.0, (size.height - ph).max(42.0));
            }
            self.position_panel_label();
            
            self.needs_rebuild = false;

            self.ui_context.rebuild_spatial_grid();
        }

        // 1. The dissolved root plate container's plate, then the top-level widgets walked in the
        // old child order (menu bar, dropdown row, graph canvas, control panel on top).
        let mut pc = cce_ui::scene::paint::PaintCtx::new();
        // The standard root plate (cce-ui PlateSpec::window).
        pc.root_plate(self.width as f32, self.height as f32);
        {
            // The walk takes shared borrows now — no self-alias, no pointers.
            let tops: [&dyn cce_ui::widget::WidgetHost; 5] = [
                &self.ui_context[self.menu_bar],
                &self.ui_context[self.dropdown_file],
                &self.ui_context[self.dropdown_edit],
                &self.ui_context[self.dropdown_view],
                &self.ui_context[self.graph],
            ];
            for top in tops {
                cce_ui::scene::painter::paint_root_into(&self.ui_context, top, &mut pc);
            }
        }

        // Images over the nodes, under the control panel, clipped to the
        // canvas: one GPU image each.
        {
            use cce_ui::scene::layout::Rect;
            let (gx, gy, gw, gh) = self.ui_context[self.graph].rect();
            let rects: Vec<(f32, f32, f32, f32)> = self.loaded_images.iter().map(|img| self.image_screen_rect(img)).collect();
            let selected = self.selected_image_idx;
            let images = &mut self.loaded_images;
            pc.clip(Rect { x: gx, y: gy, width: gw, height: gh }, |pc| {
                for (i, (img, &(x, y, w, h))) in images.iter_mut().zip(&rects).enumerate() {
                    pc.image(img.texture_id(), Rect { x, y, width: w, height: h }, 1.0);
                    if Some(i) == selected {
                        // A cyan selection outline just outside the image.
                        let (t, c) = (2.0, [0.0, 0.75, 1.0, 1.0]);
                        pc.quad(Rect { x: x - t, y: y - t, width: w + 2.0 * t, height: t }, c);
                        pc.quad(Rect { x: x - t, y: y + h, width: w + 2.0 * t, height: t }, c);
                        pc.quad(Rect { x: x - t, y, width: t, height: h }, c);
                        pc.quad(Rect { x: x + w, y, width: t, height: h }, c);
                    }
                }
            });
        }

        // The dissolved control panel, on top of the images too (it takes the
        // clicks there): its plate as prims, then the label walked.
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
            cce_ui::scene::painter::paint_root_into(&self.ui_context, &self.ui_context[self.control_panel_label], &mut pc);
        }

        // Open dropdown popovers — geometry and labels last, on top of everything, exactly
        // where they hit-test (the ui_context registration above is occlusion/routing only;
        // nothing else paints them). Labels carry bounds equal to the popover rect, which
        // clips them to the plate and exempts them from the dl-text occlusion clamp.
        {
            for &pop_id in &self.ui_context.active_popovers {
                let Some(popover) = self.ui_context.get_widget(pop_id) else { continue };
                if popover.popover_rect().is_none() {
                    continue;
                }
                // PaintCtx is a RenderTarget: the popover draws its real prims
                // (the dropdown's expanded inset-plate surface) with its own
                // per-label bounds — no flattening collector round-trip.
                popover.render_popover(&mut pc);
            }
            // The lit plate and the menu font in one call — the flat
            // `extra_quads` look was the pre-frost menu the other apps
            // have moved off.
            cce_ui::widget::context_menu::paint_with_labels(&mut pc);
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

        let over_menu = self.ui_context[self.dropdown_file].open || self.ui_context[self.dropdown_edit].open || self.ui_context[self.dropdown_view].open
            || self.ui_context[self.dropdown_file].hit_test(pos.x, pos.y, &self.ui_context)
            || self.ui_context[self.dropdown_edit].hit_test(pos.x, pos.y, &self.ui_context)
            || self.ui_context[self.dropdown_view].hit_test(pos.x, pos.y, &self.ui_context)
            || self.ui_context[self.menu_bar].hit_test(pos.x, pos.y, &self.ui_context);

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
                        self.panel_moved = true;
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
                    let (grid_origin_x, grid_origin_y) = self.ui_context[self.graph].grid_origin();
                    let (grid_size_x, grid_size_y) = self.ui_context[self.graph].grid_sizes();
                    let (skipped_row_h, skipped_col_w) = self.ui_context[self.graph].skipped_sizes();
                    let step_x = grid_size_x + skipped_col_w;
                    let step_y = grid_size_y + skipped_row_h;

                    let nx = pos.x - self.drag_image_ox;
                    let ny = pos.y - self.drag_image_oy;

                    let mut col = (nx - grid_origin_x) / step_x;
                    let mut row = (ny - grid_origin_y) / step_y;

                    if self.ui_context[self.graph].grid_snap_enabled() {
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

        // The app-owned drags end on release wherever it lands. Routed by
        // position, a release over the menu bar went to the dropdowns and the
        // panel or image stayed stuck to the pointer with no button held.
        // (Node drags are the router's: it delivers DragEnd from any root.)
        if button == MouseButton::Left && state == ElementState::Released
            && (self.panel_dragging || self.dragging_image_idx.is_some())
        {
            self.panel_dragging = false;
            self.dragging_image_idx = None;
            *needs_rebuild = true;
            self.needs_rebuild = true;
            return None;
        }

        let over_menu = self.ui_context[self.dropdown_file].open || self.ui_context[self.dropdown_edit].open || self.ui_context[self.dropdown_view].open
            || self.ui_context[self.dropdown_file].hit_test(pos.x, pos.y, &self.ui_context)
            || self.ui_context[self.dropdown_edit].hit_test(pos.x, pos.y, &self.ui_context)
            || self.ui_context[self.dropdown_view].hit_test(pos.x, pos.y, &self.ui_context)
            || self.ui_context[self.menu_bar].hit_test(pos.x, pos.y, &self.ui_context);

        let ev = Event::MouseButton { button, state, x: pos.x, y: pos.y, local_x: pos.x, local_y: pos.y };
        if over_menu {
            let dd_file = self.dropdown_file.id();
            if self.ui_context.propagate_event(&ev, dd_file) {
                changed = true;
                if let Some(m) = self.drain_file_menu() {
                    msg_out = Some(m);
                }
            }
            let dd_edit = self.dropdown_edit.id();
            if self.ui_context.propagate_event(&ev, dd_edit) {
                changed = true;
                if let Some(m) = self.drain_edit_menu() {
                    msg_out = Some(m);
                }
            }
            let dd_view = self.dropdown_view.id();
            if self.ui_context.propagate_event(&ev, dd_view) {
                changed = true;
                if let Some(m) = self.drain_view_menu() {
                    msg_out = Some(m);
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
                // (The release that ends the drag is handled up top.)
                if button == MouseButton::Left && state == ElementState::Pressed {
                    self.panel_dragging = true;
                    self.panel_drag_ox = pos.x - self.panel_x;
                    self.panel_drag_oy = pos.y - self.panel_y;
                    changed = true;
                }
                handled_by_panel = true;
            }

            if !handled_by_panel {
                // Otherwise route to Graph
                if button == MouseButton::Left {
                    if state == ElementState::Pressed {
                        if let Some(img_idx) = self.hit_test_image(pos.x, pos.y) {
                            // Images are drawn over the nodes, so they take the
                            // press first — not the node hidden under them.
                            let (ix, iy, _, _) = self.image_screen_rect(&self.loaded_images[img_idx]);
                            self.dragging_image_idx = Some(img_idx);
                            self.drag_image_ox = pos.x - ix;
                            self.drag_image_oy = pos.y - iy;
                            self.selected_image_idx = Some(img_idx);
                            self.ui_context[self.graph].set_selected_node(None);
                            changed = true;
                        } else {
                            // Routed press: a node grab records the drag target; the router
                            // synthesizes DragStart past its threshold (the old immediate
                            // drag_begin call).
                            let g = self.graph.id();
                            if !self.ui_context.propagate_event(&ev, g) {
                                self.ui_context[self.graph].set_selected_node(None);
                            }
                            self.selected_image_idx = None;
                            changed = true;
                            // A press on a port can complete a wire drawn
                            // with the mouse; write it into the node.
                            if let Some((to, from, port)) =
                                GraphController::take_pending_connection_to_port(&mut *self.ui_context[self.graph])
                            {
                                let mut nodes = self.ui_context[self.graph].get_nodes();
                                if wiring::connect(&mut nodes, &to, &from, port) {
                                    self.ui_context[self.graph].set_nodes(&nodes);
                                }
                            }
                        }
                    } else if state == ElementState::Released {
                        // (An image drag's release is handled up top.) The router
                        // delivers DragEnd (commit) before the release reaches
                        // Graph; a committed drag leaves the release arm inert.
                        let was_dragging = self.ui_context.is_dragging;
                        let g = self.graph.id();
                        if self.ui_context.propagate_event(&ev, g) || was_dragging {
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
        let ev = Event::MouseWheel { delta: *delta, x: pos.x, y: pos.y, local_x: pos.x, local_y: pos.y };
        // While a menu is open the wheel is its (a long recent-files list
        // scrolls), never the canvas behind it.
        let open_menu = [self.dropdown_file, self.dropdown_edit, self.dropdown_view]
            .into_iter()
            .find(|&h| self.ui_context[h].open);
        let root = match open_menu {
            Some(h) => h.id(),
            None => self.graph.id(),
        };
        if self.ui_context.propagate_event(&ev, root) {
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

        // An open menu dropdown takes the keyboard — Escape closes it, arrows
        // move the hover, Enter selects — routed to the widget exactly like
        // its mouse events above, drained through the same helpers. The
        // widget has handled these keys itself since cce-ui's routed events;
        // this app just never forwarded a key to it (the cce-files bug).
        if self.ui_context[self.dropdown_file].open || self.ui_context[self.dropdown_edit].open || self.ui_context[self.dropdown_view].open {
            let kev = Event::KeyInput(event.clone());
            let root = if self.ui_context[self.dropdown_file].open {
                self.dropdown_file.id()
            } else if self.ui_context[self.dropdown_edit].open {
                self.dropdown_edit.id()
            } else {
                self.dropdown_view.id()
            };
            if self.ui_context.propagate_event(&kev, root) {
                *needs_rebuild = true;
                self.needs_rebuild = true;
                return self
                    .drain_file_menu()
                    .or_else(|| self.drain_edit_menu())
                    .or_else(|| self.drain_view_menu());
            }
            // The open menu has the keyboard even for keys it ignores: Delete
            // fell through and deleted the selected node behind it.
            return None;
        }

        if event.state == ElementState::Pressed {
            // input.kdl `cce-graph.delete_node`, falling back to the legacy
            // config.kdl graph `delete` prop.
            let delete_keybind = cce_ui::input::app_chord("delete_node", &cce_ui::layout::graph_node_delete());
            if cce_ui::widget::match_key_shortcut(event, &delete_keybind) {
                self.delete_selected_node();
                *needs_rebuild = true;
                self.needs_rebuild = true;
            }
        }
        None
    }
}

fn load_config() -> (bool, bool, f32, f32) {
    let path = cce_ui::config::get_config_path();
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let val = cce_ui::config::parse_kdl_to_json(&content);
    
    let show_grid = val.pointer("/layout/graph_show_grid").and_then(|v| v.as_bool()).unwrap_or(true);
    let snap_enabled = val.pointer("/layout/graph_snap_enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let opacity = val.pointer("/layout/graph_network_opacity").and_then(|v| v.as_f64()).map(|n| n as f32).unwrap_or(0.95);
    let gap_width = val.pointer("/layout/graph_gap_width").and_then(|v| v.as_f64()).map(|n| n as f32).unwrap_or(35.0);
    
    (show_grid, snap_enabled, opacity, gap_width)
}

fn write_config_value(key: &str, value: &str) -> bool {
    let path = cce_ui::config::get_config_path();
    let path_str = path.to_string_lossy();
    cce_ui::config::write_config_value(&path_str, key, value, "layout")
}

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _guard = rt.enter();

    // `--vault` is the notes vault's link graph, a separate app; anything
    // else is the project editor.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(v) = vault::Args::parse(&args) {
        vault::run(v);
        return;
    }
    cce_ui::engine::run::<GraphApp>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn png(path: &Path, w: u32, h: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::RgbaImage::from_pixel(w, h, image::Rgba([200, 40, 40, 255])).save(path).unwrap();
    }

    fn app() -> GraphApp {
        let (tx, _rx) = calloop::channel::channel();
        GraphApp::with_sender(tx)
    }

    fn node(name: &str, params: Vec<(&str, &str, &str)>) -> GraphNode {
        GraphNode {
            id: String::new(),
            name: name.into(),
            position: (1.5, -2.0),
            parameters: params.into_iter().map(|(a, b, c)| (a.into(), b.into(), c.into())).collect(),
            geom_visible: false,
            node_type: "sop".into(),
            inputs: 2,
            outputs: 1,
        }
    }

    #[test]
    fn kdl_round_trip_survives_awkward_values() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.kdl");
        let awkward = "nul\0 \"quoted\" back\\slash\nnewline\ttab \u{1b}esc";
        let state = GraphProjectState {
            name: awkward.into(),
            nodes: vec![node(awkward, vec![("input", awkward, "node"), ("k", "v", "")])],
            images: vec![
                GraphProjectImage { path: "assets/a b \"c\".png".into(), position: (1e20, -0.5), size: (4.0, 2.0) },
                GraphProjectImage { path: "x.png".into(), position: (0.0, 0.0), size: (4.0, f32::INFINITY) },
            ],
            show_grid: false,
            opacity: 0.75,
        };
        save_project_to_kdl_path(&file, &state).unwrap();
        let back = load_project_from_kdl_path(&file).unwrap();

        assert_eq!(back.name, awkward);
        assert!(!back.show_grid);
        assert_eq!(back.opacity, 0.75);
        let n = &back.nodes[0];
        assert_eq!(n.name, awkward);
        assert_eq!(n.position, (1.5, -2.0));
        assert_eq!((n.inputs, n.outputs, n.geom_visible, n.node_type.as_str()), (2, 1, false, "sop"));
        assert_eq!(n.parameters, state.nodes[0].parameters);
        assert_eq!(back.images[0].path, state.images[0].path);
        assert_eq!(back.images[0].position, (1e20, -0.5));
        // A non-finite number is written as 0 rather than breaking the file.
        assert_eq!(back.images[1].size, (4.0, 0.0));
        // Written by rename: no temp file left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn extreme_aspect_images_keep_a_pixel_each_way() {
        let dir = tempfile::tempdir().unwrap();
        for (w, h) in [(5, 5000), (5000, 5), (1, 1)] {
            let p = dir.path().join(format!("{w}x{h}.png"));
            png(&p, w, h);
            let (pixels, pw, ph) = load_image_pixels(&p).unwrap();
            assert!(pw >= 1 && ph >= 1, "{w}x{h} -> {pw}x{ph}");
            assert_eq!(pixels.len(), (pw * ph * 4) as usize);
        }
    }

    #[test]
    fn images_shrink_to_fit_but_never_grow() {
        let dir = tempfile::tempdir().unwrap();
        for ((w, h), want) in [((40, 30), (40, 30)), ((4096, 1024), (MAX_IMAGE_DIM, 256))] {
            let p = dir.path().join(format!("{w}x{h}.png"));
            png(&p, w, h);
            let (_, pw, ph) = load_image_pixels(&p).unwrap();
            assert_eq!((pw, ph), want);
        }
    }

    #[test]
    fn an_image_takes_the_press_over_the_node_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("cover.png");
        png(&src, 40, 40);
        let mut a = app();
        a.add_image(&src).unwrap();
        let mut n = node("under", vec![]);
        n.position = a.loaded_images[0].position;
        a.ui_context[a.graph].set_nodes(&[n]);

        let (x, y, _, _) = a.image_screen_rect(&a.loaded_images[0]);
        let at = LogicalPosition { x: x + 10.0, y: y + 10.0 };
        let (press, release) = (ElementState::Pressed, ElementState::Released);
        let mut rebuild = false;
        // The node really is under that point: with the image out of the
        // way, the press selects it.
        let image = a.loaded_images.pop().unwrap();
        a.handle_mouse_input(MouseButton::Left, press, at, &mut rebuild);
        a.handle_mouse_input(MouseButton::Left, release, at, &mut rebuild);
        assert_eq!(a.ui_context[a.graph].selected_node(), Some(0));
        a.loaded_images.push(image);

        a.handle_mouse_input(MouseButton::Left, press, at, &mut rebuild);
        assert_eq!(a.selected_image_idx, Some(0));
        assert_eq!(a.dragging_image_idx, Some(0));
        assert_eq!(a.ui_context[a.graph].selected_node(), None);
    }

    #[test]
    fn a_moved_panel_stays_put_across_a_resize() {
        let mut a = app();
        a.display_list(LogicalSize { width: 1200.0, height: 800.0 }, 1.0);
        assert_eq!(a.panel_x, 1200.0 - 230.0, "top-right until moved");
        a.display_list(LogicalSize { width: 1400.0, height: 800.0 }, 1.0);
        assert_eq!(a.panel_x, 1400.0 - 230.0, "unmoved, it follows the right edge");

        (a.panel_x, a.panel_y, a.panel_moved) = (300.0, 200.0, true);
        a.display_list(LogicalSize { width: 1300.0, height: 900.0 }, 1.0);
        assert_eq!((a.panel_x, a.panel_y), (300.0, 200.0));
        // A window too small for it pulls it back inside.
        a.display_list(LogicalSize { width: 400.0, height: 300.0 }, 1.0);
        assert_eq!((a.panel_x, a.panel_y), (400.0 - 210.0, 300.0 - 160.0));
    }

    #[test]
    fn an_open_menu_keeps_the_wheel_and_the_keys() {
        let mut a = app();
        a.ui_context[a.graph].set_nodes(&[node("kept", vec![])]);
        a.ui_context[a.graph].set_selected_node(Some(0));
        a.ui_context[a.dropdown_file].open = true;
        let mut rebuild = false;

        let origin = a.ui_context[a.graph].grid_origin();
        a.handle_mouse_wheel(&MouseScrollDelta::LineDelta(0.0, -3.0), LogicalPosition { x: 500.0, y: 400.0 }, &mut rebuild);
        assert_eq!(a.ui_context[a.graph].grid_origin(), origin, "the canvas did not scroll");

        let delete = KeyEvent {
            state: ElementState::Pressed,
            logical_key: cce_ui::widget::Key::Named(cce_ui::widget::NamedKey::Delete),
            text: None,
            repeat: false,
            ctrl: false,
            shift: false,
            alt: false,
        };
        a.handle_key_input(&delete, &mut rebuild);
        assert_eq!(a.ui_context[a.graph].get_nodes().len(), 1, "Delete behind an open menu");

        // With the menu closed the same key deletes — when it is the
        // binding (input.kdl may rebind it).
        let chord = cce_ui::input::app_chord("delete_node", &cce_ui::layout::graph_node_delete());
        if chord.eq_ignore_ascii_case("delete") {
            a.ui_context[a.dropdown_file].open = false;
            a.handle_key_input(&delete, &mut rebuild);
            assert!(a.ui_context[a.graph].get_nodes().is_empty());
        }
    }

    #[test]
    fn unsaved_changes_are_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = app();
        assert!(!a.is_dirty(), "a fresh graph");
        assert!(matches!(a.guard(Discarding::New), Some(Discarding::New)), "nothing to lose: no prompt");

        a.ui_context[a.graph].set_nodes(&[node("x", vec![])]);
        assert!(a.is_dirty());
        a.save_project_to_path(&dir.path().join("p")).unwrap();
        assert!(!a.is_dirty(), "saved");

        a.show_grid = !a.show_grid;
        assert!(a.is_dirty(), "view settings are saved with the project");
        a.load_project_from_path(&dir.path().join("p")).unwrap();
        assert!(!a.is_dirty(), "reloaded");

        // The prompt's answers: Cancel keeps everything, Discard goes ahead.
        a.ui_context[a.graph].set_nodes(&[node("y", vec![])]);
        let (mut rebuild, mut exit) = (false, false);
        a.asking = true;
        a.update(AppMessage::Answered(Answer::Cancel, Discarding::Exit), &mut rebuild, &mut exit);
        assert!(!exit && !a.asking && a.is_dirty());
        a.update(AppMessage::Answered(Answer::Discard, Discarding::New), &mut rebuild, &mut exit);
        assert!(a.ui_context[a.graph].get_nodes().is_empty() && !a.is_dirty());
        a.update(AppMessage::Answered(Answer::Discard, Discarding::Exit), &mut rebuild, &mut exit);
        assert!(exit);
    }

    #[test]
    fn a_path_that_does_not_exist_is_where_save_creates_the_graph() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = app();
        let file = dir.path().join("new/board.kdl");
        a.new_project_at(&file);
        a.ui_context[a.graph].set_nodes(&[node("x", vec![])]);
        assert_eq!(a.save_in_place().unwrap().unwrap(), file);
        assert!(file.exists() && !a.is_dirty());

        let project = dir.path().join("fresh");
        a.new_project_at(&project);
        a.save_in_place().unwrap().unwrap();
        assert!(project.join("state.kdl").exists());
    }

    #[test]
    fn importing_an_asset_never_clobbers_one() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        let own = project.join("assets/pic.png");
        png(&own, 10, 10);
        let len = std::fs::metadata(&own).unwrap().len();

        // The project's own asset: referenced in place, not copied onto itself.
        assert_eq!(import_asset(&project, &own).unwrap(), "assets/pic.png");
        assert_eq!(std::fs::metadata(&own).unwrap().len(), len);

        // A different file with the same name gets a name of its own.
        let other = dir.path().join("elsewhere/pic.png");
        png(&other, 20, 3);
        assert_eq!(import_asset(&project, &other).unwrap(), "assets/pic-2.png");
        assert_eq!(std::fs::metadata(&own).unwrap().len(), len);
        assert!(project.join("assets/pic-2.png").exists());
    }

    #[test]
    fn a_bare_state_file_saves_back_to_itself() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("default.kdl");
        ensure_default_project_file(&file).unwrap();

        let mut a = app();
        a.load_project_from_path(&file).unwrap();
        a.ui_context[a.graph].set_nodes(&[node("kept", vec![])]);
        assert_eq!(a.save_in_place().unwrap().unwrap(), file);
        // Nothing written as if the folder were a project.
        assert!(!dir.path().join("state.kdl").exists());

        let mut b = app();
        b.load_project_from_path(&file).unwrap();
        let names: Vec<String> = b.ui_context[b.graph].get_nodes().into_iter().map(|n| n.name).collect();
        assert_eq!(names, ["kept"]);
    }

    #[test]
    fn save_as_carries_the_images_along() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (dir.path().join("first"), dir.path().join("second"));
        let src = dir.path().join("in/photo.png");
        png(&src, 30, 20);

        let mut a = app();
        a.save_project_to_path(&first).unwrap();
        a.add_image(&src).unwrap();
        assert_eq!(a.loaded_images[0].path, "assets/photo.png");
        a.save_in_place().unwrap().unwrap();
        a.save_project_to_path(&second).unwrap();

        let mut b = app();
        b.load_project_from_path(&second).unwrap();
        assert_eq!(b.loaded_images.len(), 1, "the image survives Save As and reload");
        assert!(second.join("assets/photo.png").exists());
    }

    #[test]
    fn app_owned_drags_end_on_a_release_over_the_menu_bar() {
        let mut a = app();
        let over_bar = LogicalPosition { x: 600.0, y: 20.0 };
        let mut rebuild = false;
        a.show_control_panel = true;
        a.panel_dragging = true;
        a.handle_mouse_input(MouseButton::Left, ElementState::Released, over_bar, &mut rebuild);
        assert!(!a.panel_dragging);

        a.dragging_image_idx = Some(0);
        a.handle_mouse_input(MouseButton::Left, ElementState::Released, over_bar, &mut rebuild);
        assert!(a.dragging_image_idx.is_none());
    }

    #[test]
    fn opening_a_project_drops_the_old_selection() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty.kdl");
        std::fs::write(&file, "").unwrap();
        let mut a = app();
        a.selected_image_idx = Some(0);
        a.dragging_image_idx = Some(0);
        a.load_project_from_path(&file).unwrap();
        assert_eq!((a.selected_image_idx, a.dragging_image_idx), (None, None));
        a.selected_image_idx = Some(0);
        a.new_project();
        assert_eq!(a.selected_image_idx, None);
    }

    #[test]
    fn a_file_that_is_not_an_image_is_refused_and_not_copied() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        let bogus = dir.path().join("notes.png");
        std::fs::write(&bogus, "not an image").unwrap();

        let mut a = app();
        a.save_project_to_path(&project).unwrap();
        assert!(a.add_image(&bogus).is_err());
        assert!(a.loaded_images.is_empty());
        assert!(!project.join("assets/notes.png").exists());
    }
}
