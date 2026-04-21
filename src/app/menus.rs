use super::*;

/// Secondary pill placements for nodes that conceptually belong to more
/// than one category.
///
/// Every catalog entry is placed in its primary pill via
/// `NodeCatalogEntry.category` (e.g. Sample & Hold → Math). This function
/// adds extra pill columns the same entry should also appear in — so the
/// user finds it under multiple browsing contexts without having to
/// curate separate catalog entries.
///
/// Keep this list small and intentional. Not every node needs to be
/// multi-placed; reserve it for nodes that genuinely serve distinct
/// workflows (Sample & Hold is both a math tool and a utility-style
/// time-lapse capture).
fn also_in(label: &str) -> &'static [&'static str] {
    match label {
        "Sample & Hold" => &["Utility"],
        _ => &[],
    }
}

/// Check if two port kinds are compatible for the wire+Tab filtered menu.
fn port_kinds_compatible(a: PortKind, b: PortKind) -> bool {
    use PortKind::*;
    if a == b { return true; }
    // Number-like types are interchangeable
    let number_like = |k: PortKind| matches!(k, Number | Normalized | Trigger | Gate | Color);
    if number_like(a) && number_like(b) { return true; }
    // Generic matches number-like and text, but NOT image or audio
    if a == Generic { return number_like(b) || b == Text; }
    if b == Generic { return number_like(a) || a == Text; }
    false
}

impl super::PatchworkApp {
    /// Apply inverse-zoom style so menus appear at native screen size
    pub(super) fn apply_inverse_zoom_style(&self, ctx: &egui::Context) -> std::sync::Arc<egui::Style> {
        let original = ctx.style();
        let inv = 1.0 / self.canvas_zoom;
        if (self.canvas_zoom - 1.0).abs() > 0.001 {
            let mut style = original.as_ref().clone();
            for (_, font_id) in style.text_styles.iter_mut() {
                font_id.size *= inv;
            }
            style.spacing.item_spacing *= inv;
            style.spacing.button_padding *= inv;
            style.spacing.interact_size *= inv;
            style.spacing.window_margin *= inv;
            ctx.set_style(std::sync::Arc::new(style));
        }
        original
    }

    pub(super) fn node_menu(&mut self, ctx: &egui::Context) {
        if !self.show_node_menu { return; }
        // Menus at native screen size regardless of canvas zoom.
        let orig_style = self.apply_inverse_zoom_style(ctx);
        let _ = self.node_menu_pos; // retained for spawn offset (used below)
        let mut keep_open = true;
        let menu_id = egui::Id::new("add_node_menu_window");

        // No accent/brand colors in this menu — categories and node entries
        // stay neutral. Colors and shapes are reserved for port kinds.
        let dim = egui::Color32::from_rgb(110, 110, 120);
        let text_normal = egui::Color32::from_rgb(200, 200, 210);
        let pill_active_fill = egui::Color32::from_rgb(70, 70, 85);
        let pill_inactive_fill = egui::Color32::from_rgba_unmultiplied(48, 48, 58, 140);
        let selected_fill = egui::Color32::from_rgb(60, 60, 72);
        let wip_color = egui::Color32::from_rgb(200, 160, 60);

        // Large modal sizing. Menu scales up with the screen but caps at
        // 1400×800 logical so it doesn't stretch absurdly on big monitors.
        let inv = 1.0 / self.canvas_zoom;
        let screen = ctx.screen_rect();
        let menu_w = (screen.width() * 0.9).min(1400.0 * inv).max(800.0 * inv);
        let menu_h = (screen.height() * 0.8).min(800.0 * inv).max(480.0 * inv);
        let center = screen.center();
        let menu_pos = egui::pos2(center.x - menu_w * 0.5, center.y - menu_h * 0.5);

        // Separate from `keep_open` (which is mutated inside the closure
        // when a node is spawned) — `open_flag` is only written by egui's
        // built-in close button on the title bar. We merge the two after
        // the window closes. Two flags because the closure borrows
        // `keep_open` mutably; Rust won't let us also pass `&mut keep_open`
        // to `.open()` simultaneously.
        let mut open_flag = true;
        let resp = egui::Window::new(egui::RichText::new("Add Node").strong())
            .id(menu_id)
            .open(&mut open_flag)
            .fixed_pos(menu_pos)
            .fixed_size([menu_w, menu_h])
            .resizable(false)
            .collapsible(false)
            .title_bar(true)
            .scroll([false, false])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(menu_w);

                // ── Search box (auto-focused) ────────────────────────────
                let prev_search = self.node_menu_search.clone();
                let search_re = ui.add(
                    egui::TextEdit::singleline(&mut self.node_menu_search)
                        .hint_text("🔍 Search nodes...")
                        .desired_width(menu_w - 16.0),
                );
                if search_re.gained_focus() || prev_search.is_empty() {
                    search_re.request_focus();
                }
                if self.node_menu_search != prev_search {
                    self.node_menu_selected = 0;
                }

                ui.add_space(4.0);

                // ── Category pills (neutral, no accent colors) ───────────
                let categories = nodes::CATEGORY_GROUPS;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for &(label, _) in categories {
                        let is_active = if label == "All" {
                            self.node_menu_category.is_empty()
                        } else {
                            self.node_menu_category == label
                        };
                        let text = egui::RichText::new(label).small();
                        let btn = if is_active {
                            egui::Button::new(text.strong().color(egui::Color32::WHITE))
                                .fill(pill_active_fill)
                                .corner_radius(12.0)
                        } else {
                            egui::Button::new(text.color(egui::Color32::from_rgb(160, 160, 170)))
                                .fill(pill_inactive_fill)
                                .corner_radius(12.0)
                        };
                        if ui.add(btn).clicked() {
                            self.node_menu_category =
                                if label == "All" { String::new() } else { label.to_string() };
                            self.node_menu_selected = 0;
                        }
                    }
                });

                ui.add_space(2.0);
                ui.separator();
                ui.add_space(2.0);

                // ── Filter logic ─────────────────────────────────────────
                let query = self.node_menu_search.to_lowercase();
                let catalog = nodes::catalog();
                let wire_ctx = self.wire_menu_context;
                let cat_filter = self.node_menu_category.clone();

                if wire_ctx.is_some() {
                    ui.label(egui::RichText::new("⚡ Compatible nodes").small().color(dim));
                    ui.add_space(2.0);
                }

                // Resolve a raw category string to its pill label. Single-
                // pass lookup via CATEGORY_GROUPS. Returns "Other" only for
                // orphan categories (e.g. System) which we filter out above.
                let pill_for = |cat: &str| -> &'static str {
                    for (label, cats) in nodes::CATEGORY_GROUPS.iter() {
                        if *label == "All" { continue; }
                        if cats.contains(&cat) { return *label; }
                    }
                    "Other"
                };

                // Which entries pass the global filters (search / wire
                // compatibility). Pill filter happens per-column below.
                let all_visible: Vec<usize> = catalog.iter().enumerate().filter_map(|(i, entry)| {
                    if entry.category == "System" { return None; }
                    if !query.is_empty()
                        && !entry.label.to_lowercase().contains(&query)
                        && !entry.category.to_lowercase().contains(&query)
                    { return None; }
                    if let Some((_, _, src_is_output, src_kind)) = wire_ctx {
                        let candidate = (entry.factory)();
                        let target_ports = if src_is_output { candidate.inputs() } else { candidate.outputs() };
                        if !target_ports.iter().any(|p| port_kinds_compatible(src_kind, p.kind)) {
                            return None;
                        }
                    }
                    Some(i)
                }).collect();

                // Build columns: one per pill group (Input / Math / Visual /
                // ML / Audio / I/O / Utility). Pill filter, when active,
                // narrows to just that column full-width.
                // An entry lands in its primary pill (via category) AND in
                // any additional pills returned by also_in(label) — letting
                // nodes like Sample & Hold appear under both Math and
                // Utility without duplicating catalog entries.
                let all_pills: Vec<&'static str> = categories.iter()
                    .filter(|(label, _)| *label != "All")
                    .map(|(label, _)| *label)
                    .collect();

                let mut columns: Vec<(&'static str, Vec<usize>)> = Vec::new();
                for &pill in &all_pills {
                    // Pill filter: when active, show ONLY that column full-width.
                    if !cat_filter.is_empty() && cat_filter != pill { continue; }
                    let entries: Vec<usize> = all_visible.iter().copied().filter(|&i| {
                        let entry = &catalog[i];
                        pill_for(entry.category) == pill || also_in(entry.label).contains(&pill)
                    }).collect();
                    // Always include every column so the layout stays stable
                    // as the user types. Empty columns render the "—"
                    // placeholder; no reflow.
                    columns.push((pill, entries));
                }
                // Flatten entries for keyboard nav: walks columns in left-
                // to-right order, entries within each column in catalog
                // order. This way Up/Down walks top-to-bottom through a
                // column, then wraps to the next column's top.
                let visible_flat: Vec<usize> =
                    columns.iter().flat_map(|(_, v)| v.iter().copied()).collect();

                let up = ui.ctx().input(|i| i.key_pressed(egui::Key::ArrowUp));
                let down = ui.ctx().input(|i| i.key_pressed(egui::Key::ArrowDown));
                let left = ui.ctx().input(|i| i.key_pressed(egui::Key::ArrowLeft));
                let right = ui.ctx().input(|i| i.key_pressed(egui::Key::ArrowRight));
                let enter = ui.ctx().input(|i| i.key_pressed(egui::Key::Enter));

                // Up/Down walk within a single column (flat index is built
                // column-major, so ±1 stays inside the current column until
                // it overruns — then jumps to the next column's top).
                if up && self.node_menu_selected > 0 { self.node_menu_selected -= 1; }
                if down && self.node_menu_selected + 1 < visible_flat.len() {
                    self.node_menu_selected += 1;
                }

                // Left/Right jump between columns at the same row. Skip
                // empty columns. `col_offsets[i]` is the flat index where
                // column i begins; last entry is the total flat length.
                let mut col_offsets: Vec<usize> = Vec::with_capacity(columns.len() + 1);
                col_offsets.push(0);
                for (_, entries) in &columns {
                    col_offsets.push(col_offsets.last().unwrap() + entries.len());
                }
                if (left || right) && !visible_flat.is_empty() {
                    let sel = self.node_menu_selected;
                    // Find current column (index of the first offset > sel − 1).
                    let current_col = col_offsets
                        .iter().rposition(|&o| o <= sel).unwrap_or(0);
                    let row_in_col = sel - col_offsets[current_col];

                    // Walk in the requested direction, skipping empty columns
                    // so a filter-emptied one doesn't trap the selection.
                    let n_cols = columns.len();
                    let dir: isize = if right { 1 } else { -1 };
                    let mut next = current_col as isize + dir;
                    while next >= 0 && (next as usize) < n_cols {
                        let size = col_offsets[next as usize + 1] - col_offsets[next as usize];
                        if size > 0 {
                            let new_row = row_in_col.min(size - 1);
                            self.node_menu_selected = col_offsets[next as usize] + new_row;
                            break;
                        }
                        next += dir;
                    }
                }

                if !visible_flat.is_empty() {
                    self.node_menu_selected = self.node_menu_selected.min(visible_flat.len() - 1);
                }

                let off_e = self.canvas_offset / self.canvas_zoom;
                let spawn_x = self.node_menu_pos.x - off_e.x;
                let spawn_y_base = self.node_menu_pos.y - off_e.y;

                let mut spawn_idx: Option<usize> = None;
                if enter && !visible_flat.is_empty() {
                    spawn_idx = Some(visible_flat[self.node_menu_selected]);
                }

                // ── Column layout ───────────────────────────────────────
                let total_cols = columns.len();
                if total_cols == 0 {
                    // Empty state (e.g. no search matches in any column).
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new("No matches").color(dim).italics());
                    });
                    ui.add_space(40.0);
                } else {
                    let col_gap = 6.0;
                    let available_w = ui.available_width();
                    let col_w = ((available_w - col_gap * (total_cols.saturating_sub(1)) as f32)
                        / total_cols as f32).max(140.0);
                    // Use the actual remaining vertical space in the window,
                    // not a precomputed guess. Subtract a small floor so the
                    // ScrollArea's scrollbar doesn't overflow the window
                    // chrome. Earlier version used `menu_h - 140.0` which
                    // gave a logical value that didn't match `ui`'s actual
                    // available height after search + pills + separator,
                    // collapsing the columns to ~90 px tall.
                    let col_h = (ui.available_height() - 4.0).max(120.0);

                    // Walk position tracks the flat index for selection
                    // highlight. Incremented in the exact same order we
                    // built `visible_flat`, so they stay in lockstep.
                    let mut walk_pos = 0usize;

                    // `horizontal_top` aligns columns to the top edge of
                    // the horizontal strip — matters when one column is
                    // shorter than its neighbours after search filtering.
                    // The `ui.set_min_height(col_h)` below forces each
                    // column's vertical layout to fill the full `col_h`
                    // regardless of content size, which is what pins the
                    // ScrollArea's effective height.
                    ui.horizontal_top(|ui| {
                        for (col_idx, (pill_name, entries)) in columns.iter().enumerate() {
                            if col_idx > 0 { ui.add_space(col_gap); }
                            ui.vertical(|ui| {
                                ui.set_width(col_w);
                                ui.set_min_height(col_h);
                                // Column header — neutral, no icon (icons
                                // were a per-raw-category detail that didn't
                                // fit the cleaner column layout).
                                ui.label(
                                    egui::RichText::new(format!("{} ({})", pill_name, entries.len()))
                                        .small().strong().color(dim),
                                );
                                ui.separator();
                                // Per-column scroll — scrolling inside one
                                // doesn't affect the others. id_salt
                                // disambiguates each column's scroll state.
                                egui::ScrollArea::vertical()
                                    .id_salt(format!("addmenu_col_{}", pill_name))
                                    .max_height(col_h)
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        for &cat_idx in entries {
                                            let entry = &catalog[cat_idx];
                                            let is_selected = walk_pos == self.node_menu_selected;
                                            let node_icon = crate::icons::node_icon(entry.label);
                                            let label_text = format!("{} {}", node_icon, entry.label);

                                            // Text color stays neutral —
                                            // WIP entries get dimmed; no
                                            // accent on selected.
                                            let text_color = if entry.wip {
                                                egui::Color32::from_rgb(100, 100, 110)
                                            } else {
                                                text_normal
                                            };
                                            let fill = if is_selected {
                                                selected_fill
                                            } else {
                                                egui::Color32::TRANSPARENT
                                            };

                                            let btn = ui.add_sized(
                                                [ui.available_width(), 24.0],
                                                egui::Button::new(
                                                    egui::RichText::new(&label_text).size(12.0).color(text_color),
                                                ).fill(fill).frame(true).corner_radius(4.0),
                                            );

                                            if entry.wip && ui.is_rect_visible(btn.rect) {
                                                let badge_pos = egui::pos2(
                                                    btn.rect.right() - 28.0,
                                                    btn.rect.center().y,
                                                );
                                                ui.painter().text(
                                                    badge_pos, egui::Align2::RIGHT_CENTER,
                                                    "[WIP]", egui::FontId::proportional(8.0), wip_color,
                                                );
                                            }

                                            if is_selected && (up || down) {
                                                btn.scroll_to_me(Some(egui::Align::Center));
                                            }

                                            // Click = spawn + close. Hover
                                            // state drives the selection so
                                            // keyboard-vs-mouse stay in sync.
                                            if btn.hovered() {
                                                self.node_menu_selected = walk_pos;
                                            }
                                            if btn.clicked() {
                                                spawn_idx = Some(cat_idx);
                                            }
                                            walk_pos += 1;
                                        }

                                        if entries.is_empty() {
                                            ui.add_space(8.0);
                                            ui.label(
                                                egui::RichText::new("—").small().color(dim),
                                            );
                                        }
                                    });
                            });
                        }

                    });
                }

                // ── Spawn the selected node ──────────────────────────────
                if let Some(cat_idx) = spawn_idx {
                    let entry = &catalog[cat_idx];
                    self.push_undo();
                    let nt = (entry.factory)();
                    let target_port_idx = if let Some((_, _, src_is_output, src_kind)) = wire_ctx {
                        let target_ports = if src_is_output { nt.inputs() } else { nt.outputs() };
                        target_ports.iter().position(|p| port_kinds_compatible(src_kind, p.kind))
                    } else { None };

                    let new_id = self.graph.add_node(nt, [spawn_x, spawn_y_base]);
                    if let Some((src_nid, src_port, src_is_output, _)) = wire_ctx {
                        if let Some(tp) = target_port_idx {
                            if src_is_output {
                                self.graph.add_connection(src_nid, src_port, new_id, tp);
                            } else {
                                self.graph.add_connection(new_id, tp, src_nid, src_port);
                            }
                        }
                        self.dragging_from = None;
                        self.wire_menu_context = None;
                    }
                    keep_open = false;
                }
            });

        // Fold the title-bar X button's state into keep_open. egui sets
        // `open_flag = false` the instant the X is clicked; we AND it
        // into keep_open so all close paths (X, Esc, click-outside,
        // node spawn) share the same cleanup.
        if !open_flag { keep_open = false; }

        // Close on click outside the menu window or Escape key.
        if let Some(inner) = &resp {
            let menu_rect = inner.response.rect;
            let clicked_outside = ctx.input(|i| {
                i.pointer.button_clicked(egui::PointerButton::Primary)
                    || i.pointer.button_clicked(egui::PointerButton::Secondary)
            }) && ctx.pointer_latest_pos().map(|p| !menu_rect.contains(p)).unwrap_or(false);
            let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if clicked_outside || esc { keep_open = false; }
        }

        if !keep_open {
            self.show_node_menu = false;
            self.node_menu_search.clear();
            self.node_menu_category.clear();
            self.node_menu_selected = 0;
            self.wire_menu_context = None;
        }
        ctx.set_style(orig_style);
    }

    /// Check for file/zoom actions from system nodes (communicated via egui temp data).
    pub(super) fn handle_system_node_actions(&mut self, ctx: &egui::Context) {
        let new_project = ctx.data_mut(|d| d.get_temp::<bool>(egui::Id::new("file_action_new")).unwrap_or(false));
        let load_project = ctx.data_mut(|d| d.get_temp::<bool>(egui::Id::new("file_action_load")).unwrap_or(false));
        let save_project = ctx.data_mut(|d| d.get_temp::<bool>(egui::Id::new("file_action_save")).unwrap_or(false));

        // Clear flags
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new("file_action_new"), false);
            d.insert_temp(egui::Id::new("file_action_load"), false);
            d.insert_temp(egui::Id::new("file_action_save"), false);
        });

        if new_project {
            self.graph = Graph::new();
            self.project_path = None;
            self.pinned_nodes.clear();
            self.undo_history.clear();
            self.session_accent = crate::nodes::theme::random_accent();
            self.spawn_default_nodes();
        }
        if load_project { self.load_project(); }
        if save_project { self.save_project(); }

        // Zoom control
        let zoom_action: Option<f32> = ctx.data_mut(|d| d.get_temp(egui::Id::new("zoom_action")));
        if let Some(new_zoom) = zoom_action {
            let z = new_zoom.clamp(0.1, 5.0);
            self.canvas_zoom = z;
            self.target_zoom = z;
            ctx.data_mut(|d| d.remove::<f32>(egui::Id::new("zoom_action")));
        }

        // Provide current zoom to ZoomControl nodes
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("current_zoom"), self.canvas_zoom));
    }

    /// Spawn default system nodes if they don't already exist.
    pub(super) fn spawn_default_nodes(&mut self) {
        let has = |nodes: &std::collections::HashMap<NodeId, crate::graph::Node>, check: &dyn Fn(&NodeType) -> bool| -> bool {
            nodes.values().any(|n| check(&n.node_type))
        };
        if !has(&self.graph.nodes, &|t| t.title() == "File" && matches!(t, NodeType::Dynamic { .. } | NodeType::FileMenu)) {
            let id = self.graph.add_node(
                NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(crate::nodes::file_menu_node::FileMenuNode) } },
                [10.0, 10.0]);
            self.pinned_nodes.insert(id);
        }
        if !has(&self.graph.nodes, &|t| t.title() == "Zoom") {
            let id = self.graph.add_node(
                NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(crate::nodes::zoom_control_node::ZoomControlNode::default()) } },
                [1100.0, 10.0]);
            self.pinned_nodes.insert(id);
        }
        if !has(&self.graph.nodes, &|t| matches!(t, NodeType::Palette { .. })) {
            let id = self.graph.add_node(NodeType::Palette { search: String::new() }, [10.0, 200.0]);
            self.pinned_nodes.insert(id);
        }
        if !has(&self.graph.nodes, &|t| matches!(t, NodeType::Dynamic { .. }) && t.title() == "Monitor") {
            let id = self.graph.add_node(
                NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(crate::nodes::monitor_node::MonitorNode::default()) } },
                [1100.0, 500.0],
            );
            self.pinned_nodes.insert(id);
        }
        if !has(&self.graph.nodes, &|t| matches!(t, NodeType::AudioDevice { .. })) {
            let id = self.graph.add_node(NodeType::AudioDevice {
                selected_output: String::new(), selected_input: String::new(),
                master_volume: 0.8, enabled: false,
            }, [1100.0, 350.0]);
            self.pinned_nodes.insert(id);
        }
        // Theme node is optional — not added by default.
        // Users can add it from the palette when needed.
    }
}
