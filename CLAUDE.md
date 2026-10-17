# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`cce-graph` is a node-graph / mood-board editor client for the CCE Wayland desktop
environment: a grid-aligned canvas of connected nodes plus free-floating images, saved
as KDL project directories. It is one crate of the multi-repo `cce` workspace (its own
git repo side-by-side with its siblings; `origin` is GitHub and a post-commit hook
pushes each commit). Read the workspace-level
`../cce-compositor/WORKSPACE.md` first — workspace layout, the `cce-ui` toolkit, config
conventions, and the multi-repo rules (each crate is its own git repo; commit here, not at
the workspace root) all live there.

The project editor is `src/main.rs`: a `GraphApp` struct implementing
`cce-ui`'s `Application` trait, run via `cce_ui::engine::run::<GraphApp>()`.
`src/wiring.rs` gives nodes their session ids and turns a wire drawn with the
mouse (the widget's pending connection) into the port's parameter — before it,
nodes loaded with empty ids and drawn wires were dropped. The actual node-canvas widget (`Graph`, `GraphNode`) lives in `cce-ui`, not
here — this crate is orchestration: menu bar + File/Edit/View dropdowns, the `Graph`
widget, an image overlay, and a floating "control panel" showing the selected
node/image.

## Vault mode (`cce-graph --vault`)

A second, separate `Application` (`src/vault.rs`, model in
`src/linkgraph.rs`): the notes vault as a link graph, milestone 4 of the
Obsidian-on-cce plan. `main` runs it when the arguments carry `--vault`
(`cce-graph --vault [DIR] [--local [NOTE]]`; DIR defaults to the vault in
config.kdl, as cce-notes reads it). Nothing it does touches the project
editor.

- **Model:** a node per note plus a grey ghost per link target that does
  not exist; one edge per linked pair. Built from `cce_vault::Index`, and
  rebuilt on every watcher batch keeping positions and pins. A label
  shared by several files takes as much of its folder path as tells them
  apart (`disambiguate`: `Finance/Index`, `Health/Index`; a root file keeps
  its bare name).
- **Files:** with the Files chip on (the default; Ctrl+Shift+A), canvases
  and attachments are nodes too — teal circles and amber squares, named
  with their extension — linked from notes that link or embed them and
  from canvases whose cards hold them. Toggling rebuilds keeping the
  notes' places. A click goes to cce-notes like a note's, which
  `xdg-open`s it.
- **Thumbnails:** zoomed in past `THUMB_FROM_PX`, an image's node is its
  picture (`THUMB_WORLD` units on its long side, labelled below, and hit
  as a rectangle). Each is decoded at `THUMB_PX` on one worker thread the
  first time it is on screen — `prepare_thumbs` asks, before the paint —
  and comes back as `Message::Thumb`; a watcher change drops it to decode
  again. The GPU side is the editor's `Picture`.
- **Layout:** force-directed (repulsion through a spatial grid of
  `REPULSE_RANGE` cells, springs, a centering pull), stepped in `tick`
  while `alpha` cools; once settled the app goes idle. The camera fits
  once the layout has spread and again when it settles, unless the user
  has panned or zoomed.
- **Local graph:** N hops (Depth chip, Ctrl+=/−) around the note open in
  cce-notes, polled once a second as `current` on cce-notes' instance
  socket (`idle_poll_interval` runs only in local mode). The poll runs on
  its own thread and answers as `Message::Current` — keep socket round
  trips off the UI thread, and give any that stay on it a timeout. A click on a node
  sends `open <abs path>` over that socket, or launches cce-notes.
- **Single instance** on `/tmp/cce-graph-vault-<display>.sock`; a second
  launch forwards `local [note]` / `global` and exits — cce-notes' Ctrl+G
  relies on that.
- **Filter box:** words match names, `#tag` / `tag:` tags (nested too),
  `path:` paths. Drag pins a node, right-click unpins.
- Labels fade in from zoom 1.1; the hovered node's neighbourhood is lit
  and labelled whatever the zoom.

`cargo test -p cce-graph` covers the model (build, ghosts, file nodes, hops, the layout
settling with links short, filters, hit-testing) and the editor's wiring.

## Build and run

```sh
cargo build -p cce-graph        # from the workspace root (shared ../target/)
cargo run -p cce-graph          # optionally pass a project path as the first arg
make install                    # release build, then `ccebuild install --no-build cce-graph`
```

Building from inside this directory also works (standalone clone case). `cargo run`
needs a running Wayland session — ideally the `cce` compositor.

## Persistence model

- **A "project" is a directory** containing `state.kdl` plus `assets/` and `code/`
  subdirs. `save_project_to_path` creates all three; images added while a project is
  loaded are copied into `assets/` and referenced by relative path. Legacy
  `state.json` projects still load; saving writes `state.kdl` and deletes the old
  JSON.
- The KDL schema is hand-rolled in `load_project_from_kdl_path` /
  `save_project_to_kdl_path` (top-level `name`/`show_grid`/`opacity`, then `node` and `image` blocks). Keep
  both functions in sync when changing it. A `uniform_background` line in an
  older save is ignored: the graph has had no fill of its own since 2026-09-29.
- Values go through `kdl_str` / `kdl_num` (the kdl crate's own escaping; non-finite
  numbers as 0) — never Rust's `{:?}`, which writes escapes KDL rejects and loses the
  whole file. Saves write a temp file and rename it over the old one.
- With no CLI arg, the app loads (creating if missing)
  `~/.config/cce/cce-graph/default.kdl` — a bare KDL state file, not a project dir.
  Any `.kdl` other than `state.kdl` opened directly is the same: `state_file` holds
  it, Save writes back to it, and its folder is what relative image paths resolve
  against.
- Images: Add Image decodes first, then `import_asset` copies into `assets/` — a file
  already there is referenced in place, a name clash gets `-2`, `-3`… Save As copies
  the relative assets to the new directory (`copy_relative_assets`).
- View settings persist to the **shared** `~/.config/cce/config.kdl` under
  `layout` (`graph_show_grid`, `graph_snap_enabled`, `graph_network_opacity`,
  `graph_gap_width`) — see
  `load_config()` / `write_config_value()`.
- The delete-node keybinding resolves through `input.kdl`'s `cce-graph.delete_node`
  (via `cce_ui::input::app_chord`), falling back to the legacy config.kdl value.
- **Reopen where it was** (`src/ui_state.rs`): the open document and the canvas's
  pan/zoom go to `<XDG state>/cce/graph/state.json` on open, Save As, New and
  `on_exit`, with the path argv named (`launched`). A launch whose argv matches
  `launched` (a restart replays argv) reopens the saved document and view, if it still
  exists; argv naming something else wins with the default view. Restoring only
  loads — it never writes a document.
- Recent files are shared toolkit state (`cce_ui::config::load_recent_files`),
  capped at 10, surfaced inside the File dropdown's options list.

## Architecture: the "dissolved" Phase-6 style

This app is the reference for cce-ui's post-Phase-6 shape — no container widgets, one
paint path. When editing, preserve these invariants (the inline comments citing phase
numbers, e.g. "6l pattern", "Phase 6m", document them deliberately):

- **Single paint path**: everything renders in `display_list()` — relayout when
  `needs_rebuild`/resize, then the window plate is emitted as raw prims, top-level
  widgets are walked with `paint_root_into` (shared borrows), and finally the images
  and then the control panel are drawn on top. There is no `view()`; text renders from the
  paint walk (`display_list_text()` returns true).
- **No root-plate/Plate containers**: the `UiContext` owns every widget.
  `GraphApp::with_sender` builds each top-level widget and hands it to
  `ui_context.insert`, which registers it **parentless** and returns a `Handle`;
  the app keeps only the handles (`menu_bar`, `dropdown_*`, `graph`,
  `control_panel_label`) and reaches the widget as `self.ui_context[handle]`.
  Routing takes `handle.id()` as its root; painting walks `&self.ui_context[handle]`.
  The former control-panel Plate is "dissolved": its rect,
  drag state, and visual are app fields (`panel_*`, `panel_visual()`), its plate is
  emitted as prims, and only its `Label` is a real walked widget.
- **Popovers are ui_context-only**: open dropdowns call
  `ui_context.register_popover_id(id)` each frame. Do NOT also register them globally —
  that spawns a render-only xdg popup that swallows clicks on the open menu.
- **Routed events**: input goes through `ui_context.propagate_event(&event, root_id)`
  with `WidgetId` roots (dropdowns get priority when `over_menu`; otherwise the
  graph). Two drags are deliberately app-owned rather than widget-routed: the control
  panel and loaded images (`dragging_image_idx`). The router owns node drags —
  `is_dragging` forces rebuilds mid-drag, and DragEnd commits before the release
  reaches `Graph`.
- **Dropdown selection protocol**: menu dropdowns use sentinel `selected = 999`
  ("nothing chosen"); on a menu's `WidgetEvent::Changed` (`drain_menus`, one
  `UiContext::take_events` sweep after a menu took a press or key) the app maps
  the selected option to an `AppMessage` and resets to 999. File-menu entries are matched by option **text**
  (recent-file paths are pushed straight into `options`), so renaming an entry means
  updating the match arm.
- **Rebuild flags are dual**: handlers set both the `*needs_rebuild` out-param (frame
  redraw) and `self.needs_rebuild` (relayout in `display_list`). Set both.

## Quirks worth knowing

- Images are decoded, shrunk to fit `MAX_IMAGE_DIM` (never enlarged), and drawn as
  one mipmapped GPU image each (`cce_ui::draw::upload_rgba_mipmapped` + `pc.image`),
  clipped to the graph rect. Uploads are lazy (`Picture::texture_id` in `src/picture.rs`, at draw) and
  redone when `renderer_epoch` changes; the pixels are kept for that. Image size on the
  canvas is in grid cells (width drives height via aspect ratio). Positions are
  (column, row) floats; snap rounds to half-cells.
- Paint and input agree on stacking: nodes, then images, then the control panel. A
  press tries the panel, then images, then the graph.
- Unsaved changes: `saved_snapshot` is the state's KDL text at the last load/save;
  New, Open, Open Recent and File › Exit go through `guard`, which asks Save /
  Discard / Cancel (rfd → zenity, on a thread) and finishes the action from
  `AppMessage::Answered`. Closing the window from outside is not guarded — cce-ui has
  no close-veto hook.
- Blocking file dialogs run on spawned threads and send results back through the
  calloop message channel (`AppMessage::Load` / `SaveToPath` / `Answered` /
  `AddImageFromPath`); don't call `cce_ui::file_dialog` on the UI thread.
- `main()` creates a tokio runtime and enters it before `engine::run` — cce-ui
  (e.g. its MCP server) expects an ambient runtime.
