//! `cce-graph --vault`: the notes vault as a link graph (Obsidian-on-cce
//! milestone 4). A separate `Application` from the project editor —
//! `main` picks one by its arguments.
//!
//! Global mode shows every note; local mode shows the notes within N links
//! of the one open in cce-notes, which it learns by polling `current` on
//! cce-notes' instance socket once a second. A click opens a note in
//! cce-notes (over the same socket, or by launching it); dragging a note
//! pins it, a right click unpins it. The filter box takes words (names),
//! `#tag` and `path:folder`. The Files chip (Ctrl+Shift+A) adds canvases
//! and attachments as nodes; a click on one goes to cce-notes as well,
//! which hands it to its own app. Zoomed in, an image's node is its
//! thumbnail, decoded on a worker thread the first time it shows.
//!
//! The layout steps in `tick` until it cools, then the app goes idle: no
//! frames while nothing moves.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use cce_ui::widget::Handle;
use cce_ui::engine::{Application, CursorIcon, LogicalPosition, LogicalSize, WindowSettings};
use cce_ui::scene::layout::Rect;
use cce_ui::scene::paint::{Cap, DisplayList, PaintCtx};
use cce_ui::widget::{
    Adapted, ElementState, Event, Key, KeyEvent, MouseButton, MouseScrollDelta, NamedKey, TextBox, WidgetHost,
};
use cce_vault::{notes_ipc, Index, VaultWatcher};

use cce_vault::FileKind;

use crate::linkgraph::{Filter, LinkGraph};
use crate::picture::{self, Picture};

const BAND_H: f32 = 40.0;
const STATUS_H: f32 = 26.0;
const FILTER_W: f32 = 240.0;
const LABEL_SIZE: f32 = 11.0;
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 6.0;
/// Layout steps per frame while it is warm.
const STEPS_PER_FRAME: usize = 2;
const POLL_EVERY: Duration = Duration::from_secs(1);
/// A press that moves less than this is a click, not a drag.
const CLICK_SLOP: f32 = 4.0;
/// A thumbnail's longest side, in world units.
const THUMB_WORLD: f32 = 40.0;
/// Thumbnails replace the squares once they would be this wide on screen.
const THUMB_FROM_PX: f32 = 14.0;
/// The longest side a thumbnail is decoded at: what it is drawn at, at
/// `MAX_ZOOM`.
const THUMB_PX: u32 = 256;

const FG: [f32; 4] = cce_ui::color::TEXT_FG;
const DIM: [f32; 4] = cce_ui::color::TEXT_DIM;

fn accent(alpha: f32) -> [f32; 4] {
    let mut c = cce_ui::color::to_linear([0.66, 0.55, 0.98, 1.0]);
    c[3] = alpha;
    c
}

fn srgb_u8(linear: [f32; 4]) -> [u8; 3] {
    let s = cce_ui::color::to_srgb(linear);
    [(s[0] * 255.0) as u8, (s[1] * 255.0) as u8, (s[2] * 255.0) as u8]
}

/// `cce-graph --vault [DIR] [--local [NOTE]]`, parsed in `main`.
pub struct Args {
    pub vault: Option<PathBuf>,
    pub local: bool,
    pub note: Option<String>,
}

impl Args {
    /// `None` when the arguments do not ask for vault mode.
    pub fn parse(args: &[String]) -> Option<Args> {
        let at = args.iter().position(|a| a == "--vault")?;
        let operand = |i: usize| args.get(i).filter(|a| !a.starts_with("--")).cloned();
        let vault = operand(at + 1).map(PathBuf::from);
        let local_at = args.iter().position(|a| a == "--local");
        Some(Args { vault, local: local_at.is_some(), note: local_at.and_then(|i| operand(i + 1)) })
    }
}

static ARGS: OnceLock<Args> = OnceLock::new();

// ---- single instance -------------------------------------------------------
//
// The claim, the race it closes and the bounded listener reads are
// `cce_ui::ipc::instance`'s; the lines below are this mode's protocol.

const SOCKET: &str = "cce-graph-vault";

/// The line a launch hands a running instance.
fn launch_line(args: &Args) -> String {
    match (args.local, &args.note) {
        (true, Some(n)) => format!("local {n}"),
        (true, None) => "local".into(),
        (false, _) => "global".into(),
    }
}

/// True when a running instance took this launch.
fn forward_or_claim(args: &Args) -> bool {
    cce_ui::ipc::instance::forward_or_claim(SOCKET, &launch_line(args))
}

fn spawn_listener(sender: calloop::channel::Sender<Message>) {
    cce_ui::ipc::instance::serve(move |line| {
        let msg = match line.split_once(' ') {
            Some(("local", note)) => Message::Mode { local: true, note: Some(note.to_string()) },
            _ if line == "local" => Message::Mode { local: true, note: None },
            _ if line == "global" => Message::Mode { local: false, note: None },
            _ => return None,
        };
        sender.send(msg).ok()?;
        Some("ok".into())
    });
}

// ---- cce-notes over its socket ---------------------------------------------

/// How long a `current` poll may wait on cce-notes before it is given up on.
const NOTES_TIMEOUT: Duration = Duration::from_millis(300);

/// The note open in cce-notes, by vault path; `None` when it is not
/// running or has no note open. Blocks up to `NOTES_TIMEOUT`: call it off
/// the UI thread (`VaultApp::poll_center`).
fn notes_current() -> Option<String> {
    notes_ipc::query(notes_ipc::Query::Current, NOTES_TIMEOUT)
}

// ---- the app ---------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Message {
    VaultChanged(Vec<PathBuf>),
    Mode { local: bool, note: Option<String> },
    /// A `current` poll of cce-notes came back (`notes_current`).
    Current(Option<String>),
    /// cce-notes could not be started to show a note; said in the status line.
    NotesFailed(String),
    /// The decoder finished an image (`None`: it could not read it). `gen`
    /// says which request, so an answer about an older copy of the file is
    /// dropped.
    Thumb { path: String, gen: u64, decoded: Option<(Vec<u8>, u32, u32)> },
    Exit,
}

/// An image node's picture, by vault path.
enum Thumb {
    Loading(u64),
    Failed,
    Ready(Picture),
}

/// Whether `path` is an image this build can decode.
fn is_picture(path: &str) -> bool {
    image::ImageFormat::from_path(path).is_ok_and(|f| f.reading_enabled())
}

enum Drag {
    None,
    Pan { last: (f32, f32) },
    Node { i: usize, from: (f32, f32), moved: bool },
}

pub struct VaultApp {
    index: Option<Index>,
    error: Option<String>,
    _watcher: Option<VaultWatcher>,
    graph: LinkGraph,
    visible: Vec<bool>,
    /// Canvases and attachments are nodes too (the Files chip).
    files: bool,
    /// Image thumbnails, decoded as their nodes first show zoomed in, and
    /// dropped when the file changes.
    thumbs: HashMap<String, Thumb>,
    thumb_gen: u64,
    /// The decoder thread's queue (vault path, file, request); started with
    /// the first request.
    decoder: Option<std::sync::mpsc::Sender<(String, PathBuf, u64)>>,

    local: bool,
    depth: usize,
    /// The local graph's centre, by vault path.
    center: Option<String>,
    next_poll: Instant,
    /// A `current` poll is running on its own thread; its answer arrives as
    /// `Message::Current`.
    poll_in_flight: bool,
    sender: calloop::channel::Sender<Message>,

    filter_input: Handle<Adapted<TextBox>>,
    filter_seen: String,

    /// World point at the middle of the graph area, and px per world unit.
    cam: (f32, f32),
    zoom: f32,
    /// Fit the camera to the visible nodes once the layout has spread,
    /// and again when it settles unless the user has moved the camera.
    fit_pending: bool,
    refit_on_settle: bool,
    hover: Option<usize>,
    drag: Drag,
    pointer: (f32, f32),
    status: Option<String>,

    width: u32,
    height: u32,
    needs_rebuild: bool,
    ui_context: cce_ui::context::UiContext,
}

/// Where the band's controls sit.
struct Metrics {
    area: Rect,
    files_chip: Rect,
    global_chip: Rect,
    local_chip: Rect,
    depth_chip: Rect,
    filter: Rect,
}

impl VaultApp {
    /// The app over an opened vault (or the error opening it), in global
    /// mode — `create` then applies the launch's mode; tests start here.
    fn new(
        index: Option<Index>,
        error: Option<String>,
        watcher: Option<VaultWatcher>,
        sender: calloop::channel::Sender<Message>,
    ) -> VaultApp {
        let graph = index.as_ref().map(|ix| LinkGraph::build(ix, None, true)).unwrap_or_default();
        let n = graph.len();
        // The context owns the widgets; the app keeps their handles.
        let mut ui_context = cce_ui::context::UiContext::new();
        VaultApp {
            index,
            error,
            _watcher: watcher,
            graph,
            visible: vec![true; n],
            files: true,
            thumbs: HashMap::new(),
            thumb_gen: 0,
            decoder: None,
            local: false,
            depth: 1,
            center: None,
            next_poll: Instant::now(),
            poll_in_flight: false,
            sender,
            filter_input: ui_context.insert(TextBox::new(String::new()).with_placeholder("Filter: words, #tag, path:")),
            filter_seen: String::new(),
            cam: (0.0, 0.0),
            zoom: 1.0,
            fit_pending: true,
            refit_on_settle: true,
            hover: None,
            drag: Drag::None,
            pointer: (0.0, 0.0),
            status: None,
            width: 1000,
            height: 700,
            needs_rebuild: true,
            ui_context,
        }
    }

    fn metrics(&self) -> Metrics {
        let (w, h) = (self.width as f32, self.height as f32);
        let inset = cce_ui::layout::root_plate_inset();
        // The chips and filter box at the toolkit's control heights, each
        // centred in the band.
        let chip_h = cce_ui::layout::button_height();
        let filter_h = cce_ui::layout::textbox_height();
        let y = (BAND_H - chip_h) / 2.0;
        let filter_w = FILTER_W.min(w * 0.35);
        let filter = Rect { x: w - inset - filter_w, y: (BAND_H - filter_h) / 2.0, width: filter_w, height: filter_h };
        let depth_chip = Rect { x: filter.x - 12.0 - 76.0, y, width: 76.0, height: chip_h };
        let local_chip = Rect { x: depth_chip.x - 8.0 - 64.0, y, width: 64.0, height: chip_h };
        let global_chip = Rect { x: local_chip.x - 64.0, y, width: 64.0, height: chip_h };
        let files_chip = Rect { x: global_chip.x - 12.0 - 56.0, y, width: 56.0, height: chip_h };
        Metrics {
            area: Rect { x: 0.0, y: BAND_H, width: w, height: (h - BAND_H - STATUS_H).max(0.0) },
            files_chip,
            global_chip,
            local_chip,
            depth_chip,
            filter,
        }
    }

    fn filter_query(&self) -> String {
        if self.ui_context[self.filter_input].editing {
            self.ui_context[self.filter_input].edit_buffer.clone()
        } else {
            self.ui_context[self.filter_input].text.clone()
        }
    }

    /// Which nodes show: the local neighbourhood (if local), narrowed by
    /// the filter.
    fn recompute_visible(&mut self) {
        let n = self.graph.len();
        let mut vis = vec![true; n];
        if self.local {
            match self.center.as_deref().and_then(|c| self.graph.index_of(c)) {
                Some(c) => vis = self.graph.within(c, self.depth),
                None => vis = vec![false; n],
            }
        }
        let f = Filter::parse(&self.filter_query());
        if !f.is_empty() {
            for (i, v) in vis.iter_mut().enumerate() {
                *v = *v && f.matches(&self.graph.nodes[i]);
            }
        }
        self.visible = vis;
        self.hover = None;
        self.graph.reheat(0.4);
        self.needs_rebuild = true;
    }

    fn set_mode(&mut self, local: bool, note: Option<String>) {
        if let Some(n) = note {
            self.center = self.index.as_ref().and_then(|ix| ix.lookup(&n)).or(Some(n));
        }
        self.local = local;
        if local && self.center.is_none() {
            // Ask cce-notes now rather than at the next poll.
            self.next_poll = Instant::now();
            self.poll_center();
        }
        self.recompute_visible();
        self.fit_pending = true;
        self.refit_on_settle = true;
    }

    /// Ask cce-notes which note is open, once a second in local mode. The
    /// socket round trip runs on its own thread (it can take up to
    /// `NOTES_TIMEOUT`, which on the UI thread was a stall every second
    /// while cce-notes was busy); the answer comes back as
    /// `Message::Current`.
    fn poll_center(&mut self) {
        if !self.local || self.poll_in_flight {
            return;
        }
        let now = Instant::now();
        if now < self.next_poll {
            return;
        }
        self.next_poll = now + POLL_EVERY;
        self.poll_in_flight = true;
        let tx = self.sender.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Message::Current(notes_current()));
        });
    }

    fn got_current(&mut self, cur: Option<String>) {
        self.poll_in_flight = false;
        let Some(cur) = cur.filter(|_| self.local) else { return };
        if self.center.as_deref() != Some(cur.as_str()) {
            self.center = Some(cur);
            self.recompute_visible();
            self.fit_pending = true;
        }
    }

    fn vault_changed(&mut self, paths: Vec<PathBuf>) {
        let Some(ix) = self.index.as_mut() else { return };
        let changes = ix.apply_changes(&paths);
        // A changed picture decodes again when next shown; a gone one frees
        // its image.
        for p in changes.updated.iter().chain(&changes.removed) {
            self.thumbs.remove(p);
        }
        self.rebuild();
        // A rebuild keeps positions; it only needs a nudge, not a restart.
        // After `recompute_visible`, whose own reheat would otherwise leave
        // it at 0.4 (and `build` starts it at 1.0).
        self.graph.alpha = 0.3;
    }

    /// Show or hide canvases and attachments. The notes keep their places;
    /// the files that join start beside what links them.
    fn set_files(&mut self, on: bool) {
        self.files = on;
        self.rebuild();
        self.graph.alpha = 0.6;
        self.fit_pending = true;
    }

    /// The graph again from the index, keeping positions and pins.
    fn rebuild(&mut self) {
        let Some(ix) = self.index.as_ref() else { return };
        let g = LinkGraph::build(ix, Some(&self.graph), self.files);
        // A node drag holds an index, which the rebuild reorders: follow the
        // node by path, or let go if its note is gone. Kept as it was, the
        // drag moved (or a click opened) another note, or indexed past the
        // end and panicked.
        self.drag = match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Node { i, from, moved } => match g.index_of(&self.graph.nodes[i].path) {
                Some(i) => Drag::Node { i, from, moved },
                None => Drag::None,
            },
            other => other,
        };
        self.graph = g;
        self.recompute_visible();
    }

    fn shows_thumbs(&self) -> bool {
        THUMB_WORLD * self.zoom >= THUMB_FROM_PX
    }

    /// Half the width and height, in world units, of node `i`'s thumbnail
    /// when it is drawn as one.
    fn thumb_half(&self, i: usize) -> Option<(f32, f32)> {
        let node = &self.graph.nodes[i];
        if !self.shows_thumbs() || node.kind != FileKind::Attachment || node.ghost {
            return None;
        }
        let Some(Thumb::Ready(p)) = self.thumbs.get(&node.path) else { return None };
        let (a, half) = (p.aspect(), THUMB_WORLD / 2.0);
        Some(if a <= 1.0 { (half, half * a) } else { (half / a, half) })
    }

    /// Before a paint: ask the decoder for the pictures of image nodes on
    /// screen that have none yet, and hand back each node's GPU image to
    /// draw (uploading those just decoded).
    fn prepare_thumbs(&mut self, area: Rect) -> Vec<Option<u32>> {
        let mut out = vec![None; self.graph.len()];
        if !self.shows_thumbs() || !self.files {
            return out;
        }
        let reach = THUMB_WORLD * self.zoom;
        for (i, slot) in out.iter_mut().enumerate() {
            let node = &self.graph.nodes[i];
            if !self.visible[i] || node.kind != FileKind::Attachment || node.ghost || !is_picture(&node.path) {
                continue;
            }
            let p = self.to_screen(area, node.pos);
            if p.0 < area.x - reach || p.0 > area.x + area.width + reach || p.1 < area.y - reach || p.1 > area.y + area.height + reach {
                continue;
            }
            let path = node.path.clone();
            match self.thumbs.get_mut(&path) {
                Some(Thumb::Ready(pic)) => *slot = Some(pic.texture_id()),
                Some(_) => {}
                None => self.request_thumb(path),
            }
        }
        out
    }

    fn request_thumb(&mut self, path: String) {
        let Some(ix) = &self.index else { return };
        let abs = ix.abs(&path);
        if self.decoder.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<(String, PathBuf, u64)>();
            let out = self.sender.clone();
            let spawned = std::thread::Builder::new().name("thumbs".into()).spawn(move || {
                for (path, abs, gen) in rx {
                    let decoded = picture::decode(&abs, THUMB_PX);
                    if out.send(Message::Thumb { path, gen, decoded }).is_err() {
                        break;
                    }
                }
            });
            if spawned.is_err() {
                return;
            }
            self.decoder = Some(tx);
        }
        self.thumb_gen += 1;
        let gen = self.thumb_gen;
        if self.decoder.as_ref().is_some_and(|d| d.send((path.clone(), abs, gen)).is_ok()) {
            self.thumbs.insert(path, Thumb::Loading(gen));
        }
    }

    fn got_thumb(&mut self, path: String, gen: u64, decoded: Option<(Vec<u8>, u32, u32)>) {
        if !matches!(self.thumbs.get(&path), Some(Thumb::Loading(g)) if *g == gen) {
            return;
        }
        let thumb = match decoded {
            Some(d) => Thumb::Ready(Picture::new(d)),
            None => Thumb::Failed,
        };
        self.thumbs.insert(path, thumb);
        self.needs_rebuild = true;
    }

    fn to_screen(&self, area: Rect, p: (f32, f32)) -> (f32, f32) {
        let (mx, my) = (area.x + area.width / 2.0, area.y + area.height / 2.0);
        (mx + (p.0 - self.cam.0) * self.zoom, my + (p.1 - self.cam.1) * self.zoom)
    }

    fn to_world(&self, area: Rect, s: (f32, f32)) -> (f32, f32) {
        let (mx, my) = (area.x + area.width / 2.0, area.y + area.height / 2.0);
        (self.cam.0 + (s.0 - mx) / self.zoom, self.cam.1 + (s.1 - my) / self.zoom)
    }

    fn fit(&mut self) {
        let area = self.metrics().area;
        let Some((lo, hi)) = self.graph.bounds(&self.visible) else { return };
        let (bw, bh) = ((hi.0 - lo.0).max(1.0), (hi.1 - lo.1).max(1.0));
        let z = ((area.width - 80.0) / bw).min((area.height - 80.0) / bh);
        self.zoom = z.clamp(MIN_ZOOM, 2.0);
        self.cam = ((lo.0 + hi.0) / 2.0, (lo.1 + hi.1) / 2.0);
        self.needs_rebuild = true;
    }

    fn zoom_at(&mut self, s: (f32, f32), factor: f32) {
        let area = self.metrics().area;
        let before = self.to_world(area, s);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.to_world(area, s);
        self.cam.0 += before.0 - after.0;
        self.cam.1 += before.1 - after.1;
        self.refit_on_settle = false;
        self.needs_rebuild = true;
    }

    fn node_at(&self, s: (f32, f32)) -> Option<usize> {
        let area = self.metrics().area;
        if !area.contains(s.0, s.1) {
            return None;
        }
        let w = self.to_world(area, s);
        // A thumbnail is bigger than the dot it stands for: under the
        // pointer, the nearest one wins.
        let on_thumb = (0..self.graph.len())
            .filter(|&i| self.visible[i])
            .filter_map(|i| {
                let (hw, hh) = self.thumb_half(i)?;
                let (dx, dy) = (w.0 - self.graph.nodes[i].pos.0, w.1 - self.graph.nodes[i].pos.1);
                (dx.abs() <= hw && dy.abs() <= hh).then_some((i, dx * dx + dy * dy))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        on_thumb.or_else(|| self.graph.hit(&self.visible, w.0, w.1, 4.0 / self.zoom))
    }

    fn open(&mut self, i: usize) {
        let node = &self.graph.nodes[i];
        if node.ghost {
            self.status = Some(format!("“{}” does not exist yet — follow a link to it in cce-notes to create it", node.name));
            return;
        }
        let Some(ix) = &self.index else { return };
        // Off this thread: a cce-notes that takes the line and never
        // answers must not stall the graph.
        let tx = self.sender.clone();
        notes_ipc::open(&ix.abs(&node.path), Some(ix.root()), move |e| {
            let _ = tx.send(Message::NotesFailed(e));
        });
    }

    /// The left button came up at `s`: a press on a node that never moved
    /// was a click, and opens it.
    fn end_drag(&mut self, s: (f32, f32)) {
        if let Drag::Node { i, moved: false, .. } = self.drag {
            self.open(i);
        }
        self.drag = Drag::None;
        self.hover = self.node_at(s);
    }

    fn paint_band(&self, pc: &mut PaintCtx, m: &Metrics) {
        let w = self.width as f32;
        pc.recess_edges(
            Rect { x: 0.0, y: 0.0, width: w, height: BAND_H },
            (0.0, 0.0, 0.0, 0.0),
            cce_ui::layout::bar_wall_width(),
            (false, false, true, false),
        );
        let (family, size) = cce_ui::layout::menubar_font_parsed();
        let inset = cce_ui::layout::root_plate_inset();
        let title = match (&self.index, self.local, &self.center) {
            (Some(_), true, Some(c)) => format!("Local graph · {}", c.trim_end_matches(".md")),
            (Some(_), true, None) => "Local graph".to_string(),
            (Some(ix), false, _) => format!("Graph · {}", vault_name(ix.root())),
            (None, _, _) => "Graph".to_string(),
        };
        let ty = cce_ui::layout::align_text_y(0.0, BAND_H, size, 0.0);
        pc.text_with(title, inset, ty, size, srgb_u8(FG), Some(family.clone()), Some([inset, 0.0, m.files_chip.x - 12.0, BAND_H]));
        let chip = |pc: &mut PaintCtx, r: Rect, label: &str, on: bool| {
            pc.rounded_rect(r, 6.0, (true, true, true, true), if on { accent(0.25) } else { [1.0, 1.0, 1.0, 0.06] });
            let lw = label.chars().count() as f32 * size * 0.6;
            pc.text_with(
                label.to_string(),
                r.x + ((r.width - lw) / 2.0).max(2.0),
                cce_ui::layout::align_text_y(r.y, r.height, size, 0.0),
                size,
                srgb_u8(if on { FG } else { DIM }),
                Some(family.clone()),
                Some([r.x, r.y, r.x + r.width, r.y + r.height]),
            );
        };
        chip(pc, m.files_chip, "Files", self.files);
        chip(pc, m.global_chip, "Global", !self.local);
        chip(pc, m.local_chip, "Local", self.local);
        if self.local {
            chip(pc, m.depth_chip, &format!("Depth {}", self.depth), false);
        }
        cce_ui::widget::painter::paint_root_into(&self.ui_context, &self.ui_context[self.filter_input], pc);
    }

    fn paint_status(&self, pc: &mut PaintCtx) {
        let (w, h) = (self.width as f32, self.height as f32);
        let y = h - STATUS_H;
        pc.recess_edges(
            Rect { x: 0.0, y, width: w, height: STATUS_H },
            (0.0, 0.0, 0.0, 0.0),
            cce_ui::layout::bar_wall_width(),
            (true, false, false, false),
        );
        let (family, size) = cce_ui::layout::statusbar_font_parsed();
        let inset = cce_ui::layout::root_plate_inset();
        let ty = cce_ui::layout::align_text_y(y, STATUS_H, size, 0.0);
        let msg = if let Some(s) = &self.status {
            s.clone()
        } else if self.index.is_none() {
            String::new()
        } else if self.local && self.center.is_none() {
            "Open a note in cce-notes; the local graph follows it".to_string()
        } else {
            let shown = || self.graph.nodes.iter().zip(&self.visible).filter(|(_, v)| **v).map(|(n, _)| n);
            let notes = shown().filter(|n| n.kind == FileKind::Note).count();
            let links = self.graph.edges.iter().filter(|(a, b)| self.visible[*a] && self.visible[*b]).count();
            let files = if self.files { format!("{} files · ", shown().filter(|n| n.kind != FileKind::Note).count()) } else { String::new() };
            format!("{notes} notes · {files}{links} links · drag to pin, right-click to unpin, Ctrl+0 to fit")
        };
        pc.text_with(msg, inset, ty, size, srgb_u8(DIM), Some(family), Some([inset, y, w - inset, h]));
    }

    /// `thumbs` holds each node's GPU image to draw as (`prepare_thumbs`).
    fn paint_graph(&self, pc: &mut PaintCtx, area: Rect, thumbs: &[Option<u32>]) {
        if let Some(e) = &self.error {
            let (family, size) = cce_ui::layout::list_font_parsed();
            for (i, line) in e.lines().enumerate() {
                pc.text_with(line.to_string(), area.x + 28.0, area.y + 28.0 + i as f32 * size * 1.6, size, srgb_u8(if i == 0 { FG } else { DIM }), Some(family.clone()), None);
            }
            return;
        }
        let g = &self.graph;
        let vis = &self.visible;
        let center = if self.local { self.center.as_deref().and_then(|c| g.index_of(c)) } else { None };
        // While a node is hovered, it and its neighbours stand out and
        // everything else steps back.
        let mut lit = vec![false; g.len()];
        if let Some(h) = self.hover {
            lit[h] = true;
            for &j in g.neighbors(h) {
                lit[j] = true;
            }
        }
        let focus = self.hover.is_some();
        let margin = 40.0;
        let on_screen = |p: (f32, f32)| {
            p.0 > area.x - margin && p.0 < area.x + area.width + margin && p.1 > area.y - margin && p.1 < area.y + area.height + margin
        };
        let line_w = (self.zoom * 1.2).clamp(0.6, 2.0);
        pc.clip(area, |pc| {
            for &(a, b) in &g.edges {
                if !(vis[a] && vis[b]) {
                    continue;
                }
                let (pa, pb) = (self.to_screen(area, g.nodes[a].pos), self.to_screen(area, g.nodes[b].pos));
                if !on_screen(pa) && !on_screen(pb) {
                    continue;
                }
                let touches = self.hover.is_some_and(|h| h == a || h == b);
                let color = if touches {
                    accent(0.9)
                } else if focus {
                    [1.0, 1.0, 1.0, 0.04]
                } else {
                    [1.0, 1.0, 1.0, 0.16]
                };
                pc.vector(pa.0, pa.1, pb.0, pb.1, if touches { line_w * 1.5 } else { line_w }, color, Cap::Round);
            }
            // Labels fade in from this zoom, so a whole-vault view is
            // dots and lines and names appear as you zoom in.
            let labels_from = 1.1;
            for i in (0..g.len()).filter(|&i| vis[i]) {
                let p = self.to_screen(area, g.nodes[i].pos);
                if !on_screen(p) {
                    continue;
                }
                let r = (g.radius(i) * self.zoom).max(1.5);
                let node = &g.nodes[i];
                let dimmed = focus && !lit[i];
                let color = if Some(i) == self.hover || Some(i) == center {
                    accent(1.0)
                } else if node.ghost {
                    [0.25, 0.25, 0.28, if dimmed { 0.3 } else { 1.0 }]
                } else {
                    // Canvases teal and attachments amber, as files apart
                    // from the grey notes.
                    let c = match node.kind {
                        FileKind::Note => [0.62, 0.62, 0.68],
                        FileKind::Canvas => [0.36, 0.66, 0.68],
                        FileKind::Attachment => [0.78, 0.6, 0.32],
                    };
                    let k = if dimmed { 0.72 } else { 1.0 };
                    [c[0] * k, c[1] * k, c[2] * k, if dimmed { 0.25 } else { 1.0 }]
                };
                // How far below the centre the label goes.
                let mut below = r;
                if let (Some(id), Some((hw, hh))) = (thumbs.get(i).copied().flatten(), self.thumb_half(i)) {
                    let (w, h) = (2.0 * hw * self.zoom, 2.0 * hh * self.zoom);
                    let rect = Rect { x: p.0 - w / 2.0, y: p.1 - h / 2.0, width: w, height: h };
                    pc.image(id, rect, if dimmed { 0.25 } else { 1.0 });
                    if Some(i) == self.hover || Some(i) == center {
                        // The accent as a frame just outside the picture.
                        let (t, c) = (2.0, accent(1.0));
                        pc.quad(Rect { x: rect.x - t, y: rect.y - t, width: w + 2.0 * t, height: t }, c);
                        pc.quad(Rect { x: rect.x - t, y: rect.y + h, width: w + 2.0 * t, height: t }, c);
                        pc.quad(Rect { x: rect.x - t, y: rect.y, width: t, height: h }, c);
                        pc.quad(Rect { x: rect.x + w, y: rect.y, width: t, height: h }, c);
                    }
                    if node.pinned {
                        // In a corner, not over the picture's middle.
                        pc.circle(rect.x + 5.0, rect.y + 5.0, 3.0, [0.1, 0.1, 0.12, 1.0]);
                    }
                    below = h / 2.0;
                } else {
                    if node.kind == FileKind::Attachment && !node.ghost {
                        // A square, so a file reads as one at any zoom.
                        let r = r * 0.9;
                        pc.rounded_rect(Rect { x: p.0 - r, y: p.1 - r, width: 2.0 * r, height: 2.0 * r }, r * 0.3, (true, true, true, true), color);
                    } else {
                        pc.circle(p.0, p.1, r, color);
                    }
                    if node.pinned {
                        pc.circle(p.0, p.1, (r * 0.35).max(1.0), [0.1, 0.1, 0.12, 1.0]);
                    }
                }
                // Labels fade in with zoom; the hovered node and its
                // neighbours always show theirs.
                let alpha = if lit[i] || Some(i) == center {
                    1.0
                } else if focus {
                    0.0
                } else {
                    ((self.zoom - labels_from) / 0.5).clamp(0.0, 1.0)
                };
                if alpha <= 0.02 {
                    continue;
                }
                let lw = node.name.chars().count() as f32 * LABEL_SIZE * 0.55;
                let color = if node.ghost { DIM } else { FG };
                pc.text_faded(
                    node.name.clone(),
                    p.0 - lw / 2.0,
                    p.1 + below + 3.0,
                    LABEL_SIZE,
                    srgb_u8(color),
                    alpha,
                    Some("sans-serif".to_string()),
                    Some([area.x, area.y, area.x + area.width, area.y + area.height]),
                );
            }
        });
    }
}


fn vault_name(root: &std::path::Path) -> String {
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

impl Application for VaultApp {
    type Message = Message;

    fn ui_context(&self) -> Option<&cce_ui::context::UiContext> {
        Some(&self.ui_context)
    }

    fn ui_context_mut(&mut self) -> Option<&mut cce_ui::context::UiContext> {
        Some(&mut self.ui_context)
    }

    fn create(sender: cce_ui::engine::AppSender<Message>) -> Self {
        // The app keeps calloop's sender; `AppSender` converts into it.
        let sender: calloop::channel::Sender<Message> = sender.into();
        let args = ARGS.get().expect("args set in run");
        let root = cce_vault::config::vault_root(args.vault.as_deref()).map_err(|e| {
            format!("No vault: {e}\nSet `vault {{ path \"~/Notes\" }}` in ~/.config/cce/config.kdl, or pass --vault <dir>.")
        });
        let (index, error, watcher) = match root {
            Ok(root) => match Index::open(&root, true) {
                Ok(ix) => {
                    let tx = sender.clone();
                    let w = VaultWatcher::spawn(&root, move |paths| {
                        let _ = tx.send(Message::VaultChanged(paths));
                    })
                    .ok();
                    (Some(ix), None, w)
                }
                Err(e) => (None, Some(format!("Could not read the vault at {}\n{e}", root.display())), None),
            },
            Err(e) => (None, Some(e), None),
        };
        spawn_listener(sender.clone());
        let mut app = VaultApp::new(index, error, watcher, sender);
        app.set_mode(args.local, args.note.clone());
        app
    }

    fn settings(&self) -> WindowSettings {
        WindowSettings {
            title: "Graph".to_string(),
            app_id: "cce-graph-vault".to_string(),
            width: 1000,
            height: 700,
            fullscreen: false,
            // The band's chips and filter box fit from here.
            min_size: Some((480, 300)),
        }
    }

    fn update(&mut self, msg: Message, needs_rebuild: &mut bool, _exit: &mut bool) {
        match msg {
            Message::VaultChanged(paths) => self.vault_changed(paths),
            Message::Mode { local, note } => self.set_mode(local, note),
            Message::Current(cur) => self.got_current(cur),
            Message::NotesFailed(e) => self.status = Some(e),
            Message::Thumb { path, gen, decoded } => self.got_thumb(path, gen, decoded),
            Message::Exit => *_exit = true,
        }
        *needs_rebuild = true;
    }

    fn idle_poll_interval(&self) -> Option<Duration> {
        // The local graph follows cce-notes; nothing else needs waking.
        self.local.then_some(POLL_EVERY)
    }

    fn tick(&mut self, dt: f32, needs_rebuild: &mut bool) {
        if self.ui_context.tick(dt) {
            *needs_rebuild = true;
        }
        self.poll_center();
        if !self.graph.settled() {
            for _ in 0..STEPS_PER_FRAME {
                self.graph.step(&self.visible);
            }
            if self.fit_pending && self.graph.alpha < 0.35 {
                self.fit_pending = false;
                self.fit();
            }
            *needs_rebuild = true;
        } else if self.fit_pending || self.refit_on_settle {
            self.fit_pending = false;
            self.refit_on_settle = false;
            self.fit();
            *needs_rebuild = true;
        }
        if self.needs_rebuild {
            *needs_rebuild = true;
        }
    }

    fn display_list(&mut self, size: LogicalSize, scale: f64) -> Option<DisplayList> {
        let resized = self.width != size.width as u32 || self.height != size.height as u32;
        self.width = size.width as u32;
        self.height = size.height as u32;
        cce_ui::scale::set_scale_factor(scale as f32);
        let m = self.metrics();
        if self.needs_rebuild || resized {
            let f = m.filter;
            self.ui_context[self.filter_input].set_rect(f.x, f.y, f.width, f.height);
            self.ui_context.rebuild_spatial_grid();
            self.needs_rebuild = false;
        }
        let thumbs = self.prepare_thumbs(m.area);
        let mut pc = PaintCtx::new();
        pc.root_plate(size.width, size.height);
        self.paint_graph(&mut pc, m.area, &thumbs);
        self.paint_band(&mut pc, &m);
        self.paint_status(&mut pc);
        Some(pc.finish())
    }

    fn display_list_text(&self) -> bool {
        true
    }

    fn cursor_icon(&self, x: f32, y: f32) -> Option<CursorIcon> {
        let m = self.metrics();
        if m.filter.contains(x, y) {
            return Some(CursorIcon::Text);
        }
        match self.drag {
            Drag::Pan { .. } => Some(CursorIcon::Grabbing),
            Drag::Node { .. } => Some(CursorIcon::Grabbing),
            Drag::None if self.hover.is_some() => Some(CursorIcon::Pointer),
            Drag::None => None,
        }
    }

    fn on_exit(&mut self) {
        if let Some(ix) = self.index.as_mut() {
            let _ = ix.save_cache();
        }
    }

    fn handle_pointer_move(&mut self, pos: LogicalPosition, needs_rebuild: &mut bool) {
        let s = (pos.x, pos.y);
        self.pointer = s;
        let area = self.metrics().area;
        match &mut self.drag {
            Drag::Pan { last } => {
                let (dx, dy) = (s.0 - last.0, s.1 - last.1);
                *last = s;
                self.cam.0 -= dx / self.zoom;
                self.cam.1 -= dy / self.zoom;
                self.refit_on_settle = false;
                *needs_rebuild = true;
                return;
            }
            Drag::Node { i, from, moved } => {
                let i = *i;
                if !*moved && ((s.0 - from.0).abs() > CLICK_SLOP || (s.1 - from.1).abs() > CLICK_SLOP) {
                    *moved = true;
                }
                if *moved {
                    let w = self.to_world(area, s);
                    let node = &mut self.graph.nodes[i];
                    node.pos = w;
                    node.pinned = true;
                    self.graph.reheat(0.25);
                    *needs_rebuild = true;
                }
                return;
            }
            Drag::None => {}
        }
        let h = self.node_at(s);
        if h != self.hover {
            self.hover = h;
            *needs_rebuild = true;
        }
        let ev = Event::PointerMove { x: s.0, y: s.1, local_x: s.0, local_y: s.1 };
        if self.ui_context.propagate_event(&ev, self.filter_input.id()) {
            *needs_rebuild = true;
        }
    }

    fn handle_mouse_input(
        &mut self,
        button: MouseButton,
        state: ElementState,
        pos: LogicalPosition,
        needs_rebuild: &mut bool,
    ) -> Option<Message> {
        let s = (pos.x, pos.y);
        *needs_rebuild = true;
        let m = self.metrics();
        let ev = Event::MouseButton { button, state, x: s.0, y: s.1, local_x: s.0, local_y: s.1 };
        let pressed = state == ElementState::Pressed;

        // A drag ends on release wherever it lands. Over the filter box the
        // release went to the box instead, and the pan or node stayed stuck
        // to the pointer with no button held.
        if button == MouseButton::Left && !pressed && !matches!(self.drag, Drag::None) {
            self.end_drag(s);
            return None;
        }

        if m.filter.contains(s.0, s.1) {
            if pressed && !self.ui_context[self.filter_input].editing {
                self.ui_context.set_focused_id(self.filter_input.id());
                WidgetHost::focus(&mut self.ui_context[self.filter_input]);
            }
            self.ui_context.propagate_event(&ev, self.filter_input.id());
            return None;
        }
        if pressed && self.ui_context[self.filter_input].editing {
            self.ui_context.unfocus_id(self.filter_input.id());
        }
        if pressed && button == MouseButton::Left {
            if m.files_chip.contains(s.0, s.1) {
                self.set_files(!self.files);
                return None;
            }
            if m.global_chip.contains(s.0, s.1) {
                self.set_mode(false, None);
                return None;
            }
            if m.local_chip.contains(s.0, s.1) {
                self.set_mode(true, None);
                return None;
            }
            if self.local && m.depth_chip.contains(s.0, s.1) {
                self.depth = self.depth % 4 + 1;
                self.recompute_visible();
                self.fit_pending = true;
                return None;
            }
        }
        if !m.area.contains(s.0, s.1) && pressed {
            return None;
        }
        match (button, state) {
            (MouseButton::Left, ElementState::Pressed) => {
                self.drag = match self.node_at(s) {
                    Some(i) => Drag::Node { i, from: s, moved: false },
                    None => Drag::Pan { last: s },
                };
                self.status = None;
            }
            (MouseButton::Left, ElementState::Released) => self.end_drag(s),
            (MouseButton::Right, ElementState::Pressed) => {
                if let Some(i) = self.node_at(s) {
                    self.graph.nodes[i].pinned = false;
                    self.graph.reheat(0.3);
                }
            }
            _ => {}
        }
        None
    }

    fn handle_mouse_wheel(&mut self, delta: &MouseScrollDelta, pos: LogicalPosition, needs_rebuild: &mut bool) {
        let s = (pos.x, pos.y);
        if !self.metrics().area.contains(s.0, s.1) {
            return;
        }
        match delta {
            // A wheel notch zooms about the pointer, as Obsidian's does.
            MouseScrollDelta::LineDelta(_, y) => self.zoom_at(s, 1.15f32.powf(*y)),
            // A trackpad pans; with Ctrl (a pinch on most stacks) it zooms.
            MouseScrollDelta::PixelDelta(p) => {
                if self.ui_context.ctrl_pressed {
                    self.zoom_at(s, 1.01f32.powf(p.y as f32));
                } else {
                    self.cam.0 -= p.x as f32 / self.zoom;
                    self.cam.1 -= p.y as f32 / self.zoom;
                    self.refit_on_settle = false;
                }
            }
        }
        *needs_rebuild = true;
    }

    fn handle_key_input(&mut self, event: &KeyEvent, needs_rebuild: &mut bool) -> Option<Message> {
        *needs_rebuild = true;
        let pressed = event.state == ElementState::Pressed;
        if self.ui_context[self.filter_input].editing {
            if pressed && matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                self.ui_context.unfocus_id(self.filter_input.id());
                return None;
            }
            let ev = Event::KeyInput(event.clone());
            self.ui_context.propagate_event(&ev, self.filter_input.id());
            let q = self.filter_query();
            if q != self.filter_seen {
                self.filter_seen = q;
                self.recompute_visible();
            }
            return None;
        }
        if !pressed {
            return None;
        }
        let k = |name: &str, default: &str| cce_ui::widget::match_key_shortcut(event, &cce_ui::input::app_chord(name, default));
        if k("toggle_local", "ctrl+l") {
            let local = !self.local;
            self.set_mode(local, None);
        } else if k("toggle_files", "ctrl+shift+a") {
            self.set_files(!self.files);
        } else if k("fit", "ctrl+0") {
            self.fit();
        } else if k("filter", "ctrl+f") {
            self.ui_context.set_focused_id(self.filter_input.id());
            WidgetHost::focus(&mut self.ui_context[self.filter_input]);
        } else if k("depth_more", "ctrl+=") || k("depth_more_plus", "ctrl+shift+=") {
            self.depth = (self.depth + 1).min(4);
            self.recompute_visible();
            self.fit_pending = true;
        } else if k("depth_less", "ctrl+-") {
            self.depth = self.depth.saturating_sub(1).max(1);
            self.recompute_visible();
            self.fit_pending = true;
        } else if k("quit", "ctrl+q") {
            return Some(Message::Exit);
        }
        None
    }
}

/// Run the vault graph, or hand the launch to the one already running.
pub fn run(args: Args) {
    if forward_or_claim(&args) {
        return;
    }
    let _ = ARGS.set(args);
    cce_ui::engine::run::<VaultApp>();
    cce_ui::ipc::instance::cleanup();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn app_over(files: &[(&str, &str)]) -> (tempfile::TempDir, VaultApp) {
        let dir = tempfile::tempdir().unwrap();
        for (p, t) in files {
            std::fs::write(dir.path().join(p), t).unwrap();
        }
        let ix = Index::open(dir.path(), false).unwrap();
        let (tx, _rx) = calloop::channel::channel();
        (dir, VaultApp::new(Some(ix), None, None, tx))
    }

    #[test]
    fn a_drag_follows_its_note_across_a_rebuild() {
        let (dir, mut app) = app_over(&[("A.md", "[[Z]]"), ("B.md", ""), ("Z.md", "")]);
        let z = app.graph.index_of("Z.md").unwrap();
        app.drag = Drag::Node { i: z, from: (0.0, 0.0), moved: true };

        // A note that sorts before Z shifts its index.
        let m = dir.path().join("M.md");
        std::fs::write(&m, "").unwrap();
        app.vault_changed(vec![m]);
        let z2 = app.graph.index_of("Z.md").unwrap();
        assert_ne!(z, z2);
        assert!(matches!(app.drag, Drag::Node { i, .. } if i == z2));

        // Its note gone, the drag lets go rather than holding a stale index.
        let gone = dir.path().join("Z.md");
        std::fs::remove_file(&gone).unwrap();
        app.vault_changed(vec![gone]);
        assert!(app.graph.index_of("Z.md").is_none());
        assert!(matches!(app.drag, Drag::None));
        // And a rebuild only nudges the layout.
        assert_eq!(app.graph.alpha, 0.3);
    }

    #[test]
    fn the_files_chip_hides_files_and_keeps_notes_in_place() {
        let (_dir, mut app) = app_over(&[("A.md", "![[pic.png]]"), ("pic.png", "x")]);
        assert!(app.files);
        let pic = app.graph.index_of("pic.png").unwrap();
        let a = app.graph.index_of("A.md").unwrap();
        assert_eq!(app.graph.neighbors(a), [pic]);
        app.graph.nodes[a].pos = (300.0, 40.0);

        // Off by a click on the chip: the picture goes, the note stays put.
        let c = app.metrics().files_chip;
        let on_chip = LogicalPosition { x: c.x + c.width / 2.0, y: c.y + c.height / 2.0 };
        let mut rebuild = false;
        app.handle_mouse_input(MouseButton::Left, ElementState::Pressed, on_chip, &mut rebuild);
        assert!(!app.files);
        assert!(app.graph.index_of("pic.png").is_none());
        assert_eq!(app.visible.len(), app.graph.len());
        let a = app.graph.index_of("A.md").unwrap();
        assert_eq!(app.graph.nodes[a].pos, (300.0, 40.0));

        // A drag on a file lets go when files hide.
        app.set_files(true);
        let pic = app.graph.index_of("pic.png").unwrap();
        app.drag = Drag::Node { i: pic, from: (0.0, 0.0), moved: true };
        app.set_files(false);
        assert!(matches!(app.drag, Drag::None));
    }

    #[test]
    fn image_nodes_become_their_thumbnails_zoomed_in() {
        let (dir, mut app) = app_over(&[("A.md", "![[pic.png]]")]);
        let file = dir.path().join("pic.png");
        image::RgbaImage::new(40, 20).save(&file).unwrap();
        app.vault_changed(vec![file.clone()]);
        let pic = app.graph.index_of("pic.png").unwrap();
        let area = app.metrics().area;

        // Too far out to show a picture: nothing is asked for.
        app.zoom = 0.1;
        assert!(app.prepare_thumbs(area).iter().all(Option::is_none));
        assert!(app.thumbs.is_empty());

        app.zoom = 1.0;
        app.cam = app.graph.nodes[pic].pos;
        app.prepare_thumbs(area);
        let Some(&Thumb::Loading(gen)) = app.thumbs.get("pic.png") else { panic!("not requested") };
        // An answer to an older request is not this one.
        app.got_thumb("pic.png".into(), gen - 1, Some((vec![0; 4], 1, 1)));
        assert!(matches!(app.thumbs.get("pic.png"), Some(Thumb::Loading(_))));
        app.got_thumb("pic.png".into(), gen, picture::decode(&file, THUMB_PX));
        assert_eq!(app.thumb_half(pic), Some((20.0, 10.0)));
        assert!(app.prepare_thumbs(area)[pic].is_some());

        // The picture's corner is the node, well outside the dot.
        let p = app.to_screen(area, app.graph.nodes[pic].pos);
        assert!(app.graph.hit(&app.visible, app.graph.nodes[pic].pos.0 + 18.0, app.graph.nodes[pic].pos.1 + 8.0, 0.0) != Some(pic));
        assert_eq!(app.node_at((p.0 + 18.0, p.1 + 8.0)), Some(pic));

        // A changed file is decoded again.
        image::RgbaImage::new(30, 30).save(&file).unwrap();
        app.vault_changed(vec![file]);
        assert!(!app.thumbs.contains_key("pic.png"));
    }

    #[test]
    fn a_release_over_the_filter_box_ends_the_drag() {
        let (_dir, mut app) = app_over(&[("A.md", "")]);
        let f = app.metrics().filter;
        let over_filter = LogicalPosition { x: f.x + f.width / 2.0, y: f.y + f.height / 2.0 };
        let mut rebuild = false;
        for drag in [Drag::Pan { last: (500.0, 300.0) }, Drag::Node { i: 0, from: (0.0, 0.0), moved: true }] {
            app.drag = drag;
            app.handle_mouse_input(MouseButton::Left, ElementState::Released, over_filter, &mut rebuild);
            assert!(matches!(app.drag, Drag::None));
        }
    }

    #[test]
    fn vault_arguments() {
        assert!(Args::parse(&args(&[])).is_none());
        assert!(Args::parse(&args(&["project/dir"])).is_none());
        let a = Args::parse(&args(&["--vault"])).unwrap();
        assert!(a.vault.is_none() && !a.local);
        let a = Args::parse(&args(&["--vault", "/v", "--local", "Note"])).unwrap();
        assert_eq!(a.vault.as_deref(), Some(std::path::Path::new("/v")));
        assert!(a.local);
        assert_eq!(a.note.as_deref(), Some("Note"));
        let a = Args::parse(&args(&["--vault", "--local"])).unwrap();
        assert!(a.vault.is_none() && a.local && a.note.is_none());
        assert_eq!(launch_line(&a), "local");
    }
}
