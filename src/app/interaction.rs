use super::*;

impl super::PatchworkApp {
    /// Pan/zoom handling — with set_zoom_factor, pointer is in logical coords.
    pub(super) fn handle_pan_zoom(&mut self, ctx: &egui::Context) {
        let z = self.canvas_zoom;
        let space_held = ctx.input(|i| i.key_down(egui::Key::Space));
        let middle_down = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Middle));
        let modifiers = ctx.input(|i| i.modifiers);

        let on_node = ctx.pointer_latest_pos().map(|p| {
            self.node_rects.values().any(|r| r.contains(p))
        }).unwrap_or(false);

        let dragging_node = ctx.pointer_latest_pos().map(|p| {
            ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary))
                && self.node_rects.values().any(|r| r.contains(p))
        }).unwrap_or(false);

        if self.show_context_menu {
            self.panning = false;
            return;
        }

        // ── Is the pointer over a SCROLLABLE UI surface? ───────────────
        // Narrower than "any non-Background layer": we want scrolling
        // over a Slider / Knob / Add / Timer to still pan the canvas
        // (user expectation: the node doesn't scroll anything, so let
        // the gesture fall through to the canvas). Only block canvas
        // scroll when pointer is on something that genuinely consumes
        // scroll — Console / Terminal / TextEditor / Add Node palette /
        // tooltips / log views / etc.
        //
        // Three cases to block:
        //   (1) Add Node menu is open AND pointer is over its window
        //   (2) Popups / tooltips above Middle order (always interactive)
        //   (3) Pointer is over a node whose NodeType is known to contain
        //       scrollable content (ScrollArea, multiline TextEdit, logs)
        let pointer_over_scroll_surface = ctx.pointer_latest_pos().map(|p| {
            // (1) + (2): layer-order heuristic for popups / menus. The
            // Add Node menu uses egui::Window (Middle), so we distinguish
            // it via `show_node_menu` — while the menu is visible ANY
            // non-background layer hover is treated as "on menu" (user is
            // in menu-interaction mode; panning the canvas underneath
            // would be actively confusing).
            if let Some(layer) = ctx.layer_id_at(p) {
                if self.show_node_menu && layer.order != egui::Order::Background {
                    return true;
                }
                if matches!(
                    layer.order,
                    egui::Order::Foreground | egui::Order::Tooltip | egui::Order::Debug
                ) {
                    return true;
                }
            }

            // (3): over a node — check if its NodeType has scroll content.
            for (&id, rect) in &self.node_rects {
                if rect.contains(p) {
                    if let Some(node) = self.graph.nodes.get(&id) {
                        return crate::nodes::node_type_has_scroll(&node.node_type);
                    }
                }
            }
            false
        }).unwrap_or(false);

        // Pan: delta is logical, multiply by zoom to get screen pixels for offset
        if middle_down || (space_held && ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary))) {
            self.panning = true;
            let delta = ctx.input(|i| i.pointer.delta());
            self.canvas_offset += delta * z;
        } else {
            self.panning = false;
        }

        // Trackpad scroll pan — only applies when pointer is over the bare
        // canvas. Cmd+scroll is reserved for zoom (handled below).
        if !modifiers.command && !pointer_over_scroll_surface {
            let scroll = ctx.input(|i| i.smooth_scroll_delta);
            if scroll.length() > 0.5 {
                self.canvas_offset += scroll * z;
            }
        }

        // Zoom
        let min_zoom: f32 = 0.25;
        let max_zoom: f32 = 2.5;

        // Pinch-to-zoom — set target, smooth interpolation handles the rest.
        // Also gated on pointer_over_scroll_surface: pinching inside a ScrollArea
        // (two-finger zoom on trackpad while hovering the palette) should
        // not zoom the canvas.
        let pinch = ctx.input(|i| i.zoom_delta());
        if (pinch - 1.0).abs() > 0.001 && !pointer_over_scroll_surface {
            self.target_zoom = (self.target_zoom * pinch).clamp(min_zoom, max_zoom);
            if let Some(pointer) = ctx.pointer_latest_pos() {
                self.zoom_anchor_screen = Some(pointer.to_vec2() * self.canvas_zoom);
            }
        }

        // Cmd+scroll → zoom (multiplicative for uniform feel at all zoom levels).
        // `on_node` guard: Cmd+scroll over ANY node (even non-scrollable
        // like Slider) shouldn't zoom the canvas — because a user might
        // be doing Cmd+scroll to e.g. adjust a parameter via modifier keys.
        // `pointer_over_scroll_surface` covers scrollable-widget hover
        // (user is navigating content, not zooming).
        if modifiers.command && !on_node && !pointer_over_scroll_surface {
            let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.5 {
                let factor = (scroll * 0.005).exp();
                self.target_zoom = (self.target_zoom * factor).clamp(min_zoom, max_zoom);
                if let Some(pointer) = ctx.pointer_latest_pos() {
                    self.zoom_anchor_screen = Some(pointer.to_vec2() * self.canvas_zoom);
                }
            }
        }

        // Smooth zoom interpolation — lerp canvas_zoom toward target_zoom
        let zoom_diff = self.target_zoom - self.canvas_zoom;
        if zoom_diff.abs() > 0.001 {
            let old_zoom = self.canvas_zoom;
            self.canvas_zoom += zoom_diff * 0.3; // ease-out over ~4 frames
            // Snap when very close to avoid perpetual micro-adjustments
            if (self.target_zoom - self.canvas_zoom).abs() < 0.002 {
                self.canvas_zoom = self.target_zoom;
            }
            // Maintain zoom anchor: keep the same canvas point under cursor
            if let Some(screen_ptr) = self.zoom_anchor_screen {
                let ratio = self.canvas_zoom / old_zoom;
                self.canvas_offset = screen_ptr - (screen_ptr - self.canvas_offset) * ratio;
            }
            ctx.request_repaint(); // keep animating until settled
        } else {
            self.zoom_anchor_screen = None;
        }

        // Clamp pan boundary
        let z = self.canvas_zoom;
        let boundary: f32 = 2000.0;
        let screen = ctx.screen_rect();
        // No conversion needed — screen rect is already in screen pixels
        let real_w = screen.width();
        let real_h = screen.height();
        let (mut min_cx, mut min_cy, mut max_cx, mut max_cy): (f32, f32, f32, f32) = (-boundary, -boundary, boundary, boundary);
        for node in self.graph.nodes.values() {
            if self.pinned_nodes.contains(&node.id) { continue; }
            min_cx = min_cx.min(node.pos[0] - 200.0);
            min_cy = min_cy.min(node.pos[1] - 200.0);
            max_cx = max_cx.max(node.pos[0] + 400.0);
            max_cy = max_cy.max(node.pos[1] + 400.0);
        }
        let max_off_x = -min_cx * z + real_w;
        let min_off_x = -max_cx * z;
        let max_off_y = -min_cy * z + real_h;
        let min_off_y = -max_cy * z;
        self.canvas_offset.x = self.canvas_offset.x.clamp(min_off_x, max_off_x);
        self.canvas_offset.y = self.canvas_offset.y.clamp(min_off_y, max_off_y);

        // Reset / fit
        if (modifiers.mac_cmd || modifiers.ctrl) && ctx.input(|i| i.key_pressed(egui::Key::Num0)) {
            self.canvas_offset = egui::Vec2::ZERO;
            self.canvas_zoom = 1.0;
            self.target_zoom = 1.0;
        }
        if (modifiers.mac_cmd || modifiers.ctrl) && ctx.input(|i| i.key_pressed(egui::Key::Num1)) {
            self.fit_all_nodes(ctx);
        }

        // ── File shortcuts (only when no text field is focused) ───────
        let no_focus = ctx.memory(|mem| mem.focused().is_none());
        let cmd = modifiers.mac_cmd || modifiers.ctrl;
        if no_focus && cmd {
            // Cmd+N → New project (fresh graph + randomised accent color)
            if ctx.input(|i| i.key_pressed(egui::Key::N)) {
                self.push_undo();
                self.graph = crate::graph::Graph::new();
                self.project_path = None;
                self.pinned_nodes.clear();
                self.undo_history.clear();
                self.spawn_default_nodes();
                // Randomise accent for the new session
                self.session_accent = crate::nodes::theme::random_accent();
            }
            // Cmd+O → Open project
            if ctx.input(|i| i.key_pressed(egui::Key::O)) {
                self.load_project();
            }
            // Cmd+S → Save (quick save to existing path)
            if !modifiers.shift && ctx.input(|i| i.key_pressed(egui::Key::S)) {
                self.save_project_quick();
            }
            // Cmd+Shift+S → Save As (always shows dialog)
            if modifiers.shift && ctx.input(|i| i.key_pressed(egui::Key::S)) {
                self.save_project();
            }
            // Cmd+Z → Undo
            if !modifiers.shift && ctx.input(|i| i.key_pressed(egui::Key::Z)) {
                self.perform_undo();
            }
            // Cmd+Shift+Z → Redo
            if modifiers.shift && ctx.input(|i| i.key_pressed(egui::Key::Z)) {
                self.perform_redo();
            }
        }

        // Delete / Backspace → delete selected nodes (when no text field focused)
        if no_focus && ctx.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
            if !self.selected_nodes.is_empty() {
                self.push_undo();
                let to_delete: Vec<_> = self.selected_nodes.iter().copied().collect();
                for id in to_delete {
                    self.audio.cleanup_node(id);
                    self.network.cleanup_node(id);
                    crate::nodes::video_player::cleanup_node(id);
                    crate::gpu_image::request_node_invalidation(id);
                    self.graph.remove_node(id);
                }
                self.selected_nodes.clear();
            }
        }

        // Cursor icon: grab when panning, move when dragging a node
        if self.panning {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if dragging_node && !self.panning {
            ctx.set_cursor_icon(egui::CursorIcon::Move);
        } else if space_held {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
    }

    /// Node interaction — pointer and node_rects are in screen pixel space.
    pub(super) fn handle_canvas_interaction(&mut self, ctx: &egui::Context) {
        // Double-click to add node
        if ctx.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
            if let Some(pos) = ctx.pointer_latest_pos() {
                if !self.node_rects.values().any(|r| r.contains(pos)) {
                    self.show_node_menu = true;
                    self.node_menu_pos = pos;
                    self.node_menu_search.clear();
                }
            }
        }
        // Right-click on empty canvas → canvas context menu
        if ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary)) {
            if let Some(pos) = ctx.pointer_latest_pos() {
                if !self.node_rects.values().any(|r| r.contains(pos)) && !self.show_context_menu {
                    self.context_menu_node = None; // No node → canvas menu
                    self.show_context_menu = true;
                    self.context_menu_pos = pos;
                    self.context_menu_opened_at = ctx.input(|i| i.time);
                }
            }
        }

        // Box selection & click-on-empty deselect
        let primary_down = ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
        let primary_pressed = ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
        let primary_released = ctx.input(|i| i.pointer.any_released());
        let on_empty = ctx.pointer_latest_pos().map(|p| !self.node_rects.values().any(|r| r.contains(p))).unwrap_or(false);
        let not_dragging_wire = self.dragging_from.is_none();
        let not_panning = !self.panning;
        let not_menu = !self.show_node_menu && !self.show_context_menu;

        if primary_pressed && on_empty && not_dragging_wire && not_panning && not_menu {
            if let Some(pos) = ctx.pointer_latest_pos() {
                self.box_select_start = Some(pos);
                self.box_select_end = Some(pos);
            }
        }

        if let Some(_start) = self.box_select_start {
            // Escape cancels box selection without changing selection
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.box_select_start = None;
                self.box_select_end = None;
            } else if primary_down {
                if let Some(pos) = ctx.pointer_latest_pos() {
                    self.box_select_end = Some(pos);
                }
            }
            if primary_released {
                // Compute selection rect
                if let (Some(start), Some(end)) = (self.box_select_start, self.box_select_end) {
                    let sel_rect = egui::Rect::from_two_pos(start, end);
                    let shift = ctx.input(|i| i.modifiers.shift);
                    // Only select if drag was meaningful (> 4px), otherwise treat as click-deselect
                    if sel_rect.width() > 4.0 || sel_rect.height() > 4.0 {
                        let hits: std::collections::HashSet<NodeId> = self.node_rects.iter()
                            .filter(|(_, rect)| sel_rect.intersects(**rect))
                            .map(|(id, _)| *id)
                            .collect();
                        if shift {
                            for id in hits { self.selected_nodes.insert(id); }
                        } else {
                            self.selected_nodes = hits;
                        }
                    } else {
                        // Small click on empty = deselect
                        if !ctx.input(|i| i.modifiers.shift) {
                            self.selected_nodes.clear();
                        }
                        self.selected_connection = None;
                    }
                }
                self.box_select_start = None;
                self.box_select_end = None;
            }
        }
        // Tab while dragging wire → open filtered node menu for auto-connect
        if self.dragging_from.is_some() && ctx.input(|i| i.key_pressed(egui::Key::Tab)) {
            if let Some((nid, port, is_output)) = self.dragging_from {
                // Get the port kind for filtering
                let port_kind = self.graph.nodes.get(&nid).and_then(|node| {
                    let defs = if is_output { node.node_type.outputs() } else { node.node_type.inputs() };
                    defs.get(port).map(|d| d.kind)
                }).unwrap_or(PortKind::Generic);

                self.wire_menu_context = Some((nid, port, is_output, port_kind));
                self.show_node_menu = true;
                self.node_menu_pos = ctx.pointer_latest_pos().unwrap_or(egui::Pos2::ZERO);
                self.node_menu_search.clear();
            }
        }
        // Enter or Tab on canvas → open node menu (mirror of double-click).
        // Skipped when a wire is being dragged (Tab there opens a port-filtered
        // menu just above), when a text field has focus (so typing in a search
        // box / DragValue doesn't pop the picker), and when any menu is already
        // showing (so the menu's own Enter-to-select still works).
        if !self.show_node_menu
            && !self.show_context_menu
            && self.dragging_from.is_none()
            && !ctx.wants_keyboard_input()
        {
            let kb_open = ctx.input(|i| {
                i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Tab)
            });
            if kb_open {
                let pos = ctx.pointer_latest_pos()
                    .unwrap_or_else(|| ctx.screen_rect().center());
                self.show_node_menu = true;
                self.node_menu_pos = pos;
                self.node_menu_search.clear();
            }
        }
        if self.show_node_menu && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.show_node_menu = false;
            self.node_menu_search.clear();
            self.wire_menu_context = None;
        }
        if self.show_node_menu && ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary)) {
            self.show_node_menu = false;
            self.node_menu_search.clear();
            self.wire_menu_context = None;
        }
        if self.show_context_menu && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.show_context_menu = false;
        }

        // Keyboard shortcuts
        self.handle_keyboard_shortcuts(ctx);

        // Option+drag completion
        self.handle_opt_drag(ctx);
    }

    pub(super) fn handle_keyboard_shortcuts(&mut self, ctx: &egui::Context) {
        let modifiers = ctx.input(|i| i.modifiers);
        let cmd = modifiers.mac_cmd || modifiers.ctrl;
        let text_focused = ctx.wants_keyboard_input();

        // Cmd+Z = undo, Cmd+Shift+Z = redo
        if cmd && !modifiers.shift && !text_focused && ctx.input(|i| i.key_pressed(egui::Key::Z)) {
            self.perform_undo();
        }
        if cmd && modifiers.shift && !text_focused && ctx.input(|i| i.key_pressed(egui::Key::Z)) {
            self.perform_redo();
        }

        // Cmd+C = copy node (only when no text field is focused)
        if cmd && !text_focused && ctx.input(|i| i.key_pressed(egui::Key::C)) {
            if let Some(id) = self.primary_selected() {
                if let Some(node) = self.graph.nodes.get(&id) {
                    self.clipboard = Some(node.node_type.clone());
                }
            }
        }
        // Cmd+V = paste node (only when no text field is focused)
        if cmd && !text_focused && ctx.input(|i| i.key_pressed(egui::Key::V)) {
            if let Some(nt) = self.clipboard.clone() {
                let (cx, cy) = if let Some(id) = self.primary_selected() {
                    if let Some(node) = self.graph.nodes.get(&id) {
                        (node.pos[0] + 30.0, node.pos[1] + 30.0)
                    } else {
                        // pointer_latest_pos() is in egui logical coords.
                        // Rendering: egui_pos = canvas_pos + offset/zoom
                        // Inverse:   canvas_pos = egui_pos - offset/zoom
                        let pos = ctx.pointer_latest_pos().unwrap_or(egui::pos2(200.0, 200.0));
                        let off_e = self.canvas_offset / self.canvas_zoom;
                        (pos.x - off_e.x + 20.0, pos.y - off_e.y + 20.0)
                    }
                } else {
                    let pos = ctx.pointer_latest_pos().unwrap_or(egui::pos2(200.0, 200.0));
                    let off_e = self.canvas_offset / self.canvas_zoom;
                    (pos.x - off_e.x + 20.0, pos.y - off_e.y + 20.0)
                };
                self.push_undo();
                let new_id = self.add_node_selected(ctx, nt, [cx, cy]);
                let _ = new_id;
            }
        }
        // Cmd+D = duplicate (only when no text field is focused)
        if cmd && !text_focused && ctx.input(|i| i.key_pressed(egui::Key::D)) {
            self.duplicate_selected();
        }
        // Delete / Backspace = delete selected (only if no text input is focused)
        if !text_focused && ctx.input(|i| i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete)) {
            if let Some(conn_id) = self.selected_connection.take() {
                if let Some(conn_idx) = self.find_connection_index(&conn_id) {
                    self.push_undo();
                    self.graph.connections.remove(conn_idx);
                }
            } else if !self.selected_nodes.is_empty() {
                self.push_undo();
                let to_delete: Vec<NodeId> = self.selected_nodes.drain().collect();
                for id in to_delete {
                    self.midi.cleanup_node(id);
                    self.serial.cleanup_node(id);
                    self.osc.cleanup_node(id);
                    self.network.cleanup_node(id);
                    self.ob.cleanup_node(id);
                    self.audio.cleanup_node(id);
                    self.dmx.cleanup_node(id);
                    self.artnet.cleanup_node(id);
                    crate::nodes::video_player::cleanup_node(id);
                    crate::gpu_image::request_node_invalidation(id);
                    self.graph.remove_node(id);
                }
            }
        }
    }

    pub(super) fn handle_opt_drag(&mut self, ctx: &egui::Context) {
        if let Some(source_id) = self.opt_drag_source {
            if self.opt_drag_created.is_none() {
                // Create duplicate at same position
                if let Some(node) = self.graph.nodes.get(&source_id) {
                    let nt = node.node_type.clone();
                    let pos = node.pos;
                    self.push_undo();
                    let new_id = self.add_node_selected(ctx, nt, [pos[0] + 30.0, pos[1] + 30.0]);
                    self.opt_drag_created = Some(new_id);
                }
            }
            if ctx.input(|i| i.pointer.any_released()) {
                self.opt_drag_source = None;
                self.opt_drag_created = None;
            }
        }
    }

    pub(super) fn duplicate_selected(&mut self) {
        let ids: Vec<NodeId> = self.selected_nodes.iter().copied().collect();
        if ids.is_empty() { return; }
        self.push_undo();
        let mut new_ids = std::collections::HashSet::new();
        for id in ids {
            if let Some(node) = self.graph.nodes.get(&id) {
                let nt = node.node_type.clone();
                let pos = node.pos;
                let new_id = self.graph.add_node(nt, [pos[0] + 30.0, pos[1] + 30.0]);
                new_ids.insert(new_id);
            }
        }
        self.selected_nodes = new_ids;
    }

    pub(super) fn context_menu(&mut self, ctx: &egui::Context) {
        if !self.show_context_menu { return; }

        // Auto-dismiss after 3 seconds if pointer is not over menu
        let now = ctx.input(|i| i.time);
        let elapsed = now - self.context_menu_opened_at;
        let pos = self.context_menu_pos;
        let menu_rect = egui::Rect::from_min_size(pos, egui::vec2(180.0, 280.0));
        let pointer_on_menu = ctx.pointer_latest_pos().map(|p| menu_rect.contains(p)).unwrap_or(false);

        if elapsed > 3.0 && !pointer_on_menu {
            self.show_context_menu = false;
            self.context_menu_node = None;
            return;
        }

        // Reset timer when pointer enters menu
        if pointer_on_menu {
            self.context_menu_opened_at = now;
        }

        let orig_style = self.apply_inverse_zoom_style(ctx);
        let mut keep_open = true;
        let on_node = self.context_menu_node.is_some();
        use crate::icons;

        egui::Area::new(egui::Id::new("node_context_menu"))
            .fixed_pos(pos)
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(140.0 / self.canvas_zoom);

                    if on_node {
                        // ── Node context menu ──────────────────────────
                        // Show node type + ID
                        if let Some(id) = self.context_menu_node {
                            if let Some(node) = self.graph.nodes.get(&id) {
                                let [cr, cg, cb] = node.node_type.color_hint();
                                let accent = egui::Color32::from_rgb(cr, cg, cb);
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(node.node_type.title()).strong().color(accent));
                                    let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
                                    ui.label(egui::RichText::new(format!("#{}", id)).small().color(dim));
                                });
                                ui.separator();
                            }
                        }
                        if icon_menu_item(ui, icons::COPY, "Copy").clicked() {
                            if let Some(id) = self.context_menu_node {
                                if let Some(node) = self.graph.nodes.get(&id) {
                                    self.clipboard = Some(node.node_type.clone());
                                }
                            }
                            keep_open = false;
                        }
                        if icon_menu_item(ui, icons::COPY, "Duplicate").clicked() {
                            if let Some(id) = self.context_menu_node {
                                if let Some(node) = self.graph.nodes.get(&id) {
                                    let nt = node.node_type.clone();
                                    let p = node.pos;
                                    self.push_undo();
                                    let _ = self.add_node_selected(ctx, nt, [p[0] + 30.0, p[1] + 30.0]);
                                }
                            }
                            keep_open = false;
                        }
                        ui.separator();
                        if let Some(id) = self.context_menu_node {
                            let is_pinned = self.pinned_nodes.contains(&id);
                            let (icon, label) = if is_pinned {
                                (icons::LOCK_OPEN, "Unpin")
                            } else {
                                (icons::PUSH_PIN, "Pin to screen")
                            };
                            if icon_menu_item(ui, icon, label).clicked() {
                                self.push_undo();
                                if is_pinned {
                                    if let Some(node) = self.graph.nodes.get_mut(&id) {
                                        let cx = (node.pos[0] - self.canvas_offset.x) / self.canvas_zoom;
                                        let cy = (node.pos[1] - self.canvas_offset.y) / self.canvas_zoom;
                                        node.pos = [cx, cy];
                                    }
                                    self.pinned_nodes.remove(&id);
                                } else {
                                    if let Some(node) = self.graph.nodes.get_mut(&id) {
                                        let sx = node.pos[0] * self.canvas_zoom + self.canvas_offset.x;
                                        let sy = node.pos[1] * self.canvas_zoom + self.canvas_offset.y;
                                        node.pos = [sx, sy];
                                    }
                                    self.pinned_nodes.insert(id);
                                }
                                keep_open = false;
                            }
                        }
                        ui.separator();
                        let del_label = if self.selected_nodes.len() > 1 {
                            format!("Delete {}", self.selected_nodes.len())
                        } else {
                            "Delete".to_string()
                        };
                        if ui.add(egui::Button::new(
                            egui::RichText::new(format!("{} {}", icons::TRASH, del_label))
                                .color(egui::Color32::from_rgb(255, 100, 100))
                        ).frame(false)).clicked() {
                            self.push_undo();
                            let to_delete: Vec<NodeId> = if self.selected_nodes.len() > 1 {
                                self.selected_nodes.drain().collect()
                            } else if let Some(id) = self.context_menu_node {
                                self.selected_nodes.remove(&id);
                                vec![id]
                            } else {
                                vec![]
                            };
                            for id in to_delete {
                                self.midi.cleanup_node(id);
                                self.serial.cleanup_node(id);
                                self.osc.cleanup_node(id);
                                self.network.cleanup_node(id);
                                self.ob.cleanup_node(id);
                                self.audio.cleanup_node(id);
                                self.dmx.cleanup_node(id);
                                self.artnet.cleanup_node(id);
                                crate::nodes::video_player::cleanup_node(id);
                                crate::gpu_image::request_node_invalidation(id);
                                self.graph.remove_node(id);
                            }
                            keep_open = false;
                        }
                    } else {
                        // ── Canvas context menu (empty space) ──────────
                        if icon_menu_item(ui, icons::PLUS, "Add Node...").clicked() {
                            self.show_node_menu = true;
                            self.node_menu_pos = pos;
                            self.node_menu_search.clear();
                            keep_open = false;
                        }
                        ui.separator();
                        ui.add_enabled_ui(self.undo_history.can_undo(), |ui| {
                            if icon_menu_item(ui, icons::ARROW_UP, "Undo").clicked() {
                                self.perform_undo();
                                keep_open = false;
                            }
                        });
                        ui.add_enabled_ui(self.undo_history.can_redo(), |ui| {
                            if icon_menu_item(ui, icons::ARROW_DOWN, "Redo").clicked() {
                                self.perform_redo();
                                keep_open = false;
                            }
                        });
                        if self.clipboard.is_some() {
                            ui.separator();
                            if icon_menu_item(ui, icons::COPY, "Paste").clicked() {
                                if let Some(nt) = self.clipboard.clone() {
                                    self.push_undo();
                                    let off_e = self.canvas_offset / self.canvas_zoom;
                                    let _ = self.add_node_selected(ctx, nt, [pos.x - off_e.x + 20.0, pos.y - off_e.y + 20.0]);
                                }
                                keep_open = false;
                            }
                        }
                        ui.separator();
                        if icon_menu_item(ui, icons::ARROWS_OUT, "Fit All").clicked() {
                            self.fit_all_nodes(ctx);
                            keep_open = false;
                        }
                        if icon_menu_item(ui, icons::FUNNEL, "Reset Zoom").clicked() {
                            self.canvas_offset = egui::Vec2::ZERO;
                            self.canvas_zoom = 1.0;
                            keep_open = false;
                        }
                        ui.separator();
                        if icon_menu_item(ui, icons::PALETTE, "Theme").clicked() {
                            // If a Theme node exists, scroll to it; otherwise create one
                            let existing = self.graph.nodes.iter()
                                .find(|(_, n)| matches!(n.node_type, NodeType::Theme { .. }))
                                .map(|(&id, _)| id);
                            if let Some(theme_id) = existing {
                                // Scroll canvas to center on the existing Theme node
                                if let Some(node) = self.graph.nodes.get(&theme_id) {
                                    let screen_center = ctx.screen_rect().center();
                                    self.canvas_offset.x = screen_center.x - node.pos[0] * self.canvas_zoom;
                                    self.canvas_offset.y = screen_center.y - node.pos[1] * self.canvas_zoom;
                                }
                                self.selected_nodes.clear();
                                self.selected_nodes.insert(theme_id);
                            } else {
                                // Create new Theme node at click position
                                let off_e = self.canvas_offset / self.canvas_zoom;
                                let accent = crate::nodes::theme::random_accent();
                                let _ = self.add_node_selected(ctx, NodeType::Theme {
                                    dark_mode: true, accent, font_size: 14.0,
                                    bg_color: [20, 20, 20], text_color: [220, 220, 220],
                                    window_bg: [24, 24, 24], window_alpha: 240,
                                    grid_color: [28, 28, 28], grid_style: 2, wire_style: 0,
                                    wiggle_gravity: 0.0, wiggle_range: 1.0, wiggle_speed: 1.0, wiggle_signal: 0.0,
                                    rounding: 16.0, spacing: 4.0, use_hsl: false,
                                    wire_thickness: 6.0, background_path: String::new(),
                                }, [pos.x - off_e.x, pos.y - off_e.y]);
                            }
                            keep_open = false;
                        }
                    }
                });
            });

        if !keep_open {
            self.show_context_menu = false;
            self.context_menu_node = None;
        }
        // Click elsewhere to close
        if ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary)
            || i.pointer.button_clicked(egui::PointerButton::Secondary))
        {
            if let Some(ptr) = ctx.pointer_latest_pos() {
                if !menu_rect.contains(ptr) {
                    self.show_context_menu = false;
                    self.context_menu_node = None;
                }
            }
        }
        ctx.set_style(orig_style);
    }

    pub(super) fn fit_all_nodes(&mut self, ctx: &egui::Context) {
        if self.graph.nodes.is_empty() { return; }
        let mut min_x = f32::MAX; let mut min_y = f32::MAX;
        let mut max_x = f32::MIN; let mut max_y = f32::MIN;
        for node in self.graph.nodes.values() {
            if self.pinned_nodes.contains(&node.id) { continue; }
            min_x = min_x.min(node.pos[0]);
            min_y = min_y.min(node.pos[1]);
            max_x = max_x.max(node.pos[0] + 200.0);
            max_y = max_y.max(node.pos[1] + 150.0);
        }
        if min_x >= max_x { return; }
        let screen = ctx.screen_rect();
        let margin = 80.0;
        // Screen rect is now in real pixels (no zoom factor applied)
        let available_w = screen.width() - margin * 2.0;
        let available_h = screen.height() - margin * 2.0;
        let content_w = max_x - min_x;
        let content_h = max_y - min_y;
        let zoom_x = available_w / content_w;
        let zoom_y = available_h / content_h;
        self.canvas_zoom = zoom_x.min(zoom_y).clamp(0.08, 2.0);
        self.canvas_offset = egui::vec2(
            margin - min_x * self.canvas_zoom,
            margin - min_y * self.canvas_zoom,
        );
    }
}

/// Menu item with Phosphor icon + label
fn icon_menu_item(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    ui.add(egui::Button::new(
        egui::RichText::new(format!("{}  {}", icon, label))
    ).frame(false))
}
