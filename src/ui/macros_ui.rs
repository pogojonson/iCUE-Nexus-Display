//! G-key macro library UI: panel + Recorder / Combo / Calibration dialogs
//! (`MacroPanel.tsx`, `RecorderDialog.tsx`, `ComboDialog.tsx`,
//! `CalibrationDialog.tsx` equivalents).

use crate::core::events::MacroCmd;
use crate::core::types::*;
use crate::state::AppState;
use crate::ui::widgets::{self, AlertKind, ButtonKind};
use eframe::egui::{self, Color32, RichText, Vec2};

pub const ACTION_LABELS: [(GKeyAction, &str); 3] = [
    (GKeyAction::Once, "Play once"),
    (GKeyAction::Repeat, "While held"),
    (GKeyAction::Toggle, "Toggle loop"),
];
pub const SPEEDS: [f64; 8] = [0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];

pub struct MacroPanelState {
    pub dialog: Option<Dialog>,
    pub assign_menu_for: Option<String>, // macro id whose assign menu is open
    pub keycap_menu_for: Option<usize>,  // G-slot whose menu is open
    pub recorder: RecorderState,
    pub combo: ComboState,
    pub calibration: CalibrationState,
}

impl Default for MacroPanelState {
    fn default() -> Self {
        MacroPanelState {
            dialog: None,
            assign_menu_for: None,
            keycap_menu_for: None,
            recorder: RecorderState::default(),
            combo: ComboState::default(),
            calibration: CalibrationState::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    Recorder,
    Combo,
    Calibration,
}

// ─── Panel ─────────────────────────────────────────────────────────────

pub fn show(ui: &mut egui::Ui, state: &mut AppState, mp: &mut MacroPanelState) {
    let mut open_dialog: Option<Dialog> = None;
    let mut action: Option<PanelAction> = None;

    // ── Header row ──────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(RichText::new("Library").strong().size(15.0));
        ui.add_space(6.0);

        if state.recording() {
            widgets::chip(ui, "● Recording", Some(Color32::from_rgb(211, 47, 47)), Color32::WHITE);
        }
        if state.playing() {
            widgets::chip(ui, "Playing", Some(Color32::from_rgb(46, 125, 50)), Color32::WHITE);
            if ui.button("⏹").on_hover_text("Stop playback").clicked() {
                action = Some(PanelAction::StopPlay);
            }
        }
        if state.helper_offline() {
            widgets::chip(ui, "Helper offline", None, Color32::from_gray(190));
        }
        if let Some(err) = state.macro_last_error.clone() {
            let (fill, tc) = (Some(Color32::from_rgb(211, 47, 47)), Color32::WHITE);
            egui::Frame::none()
                .fill(fill.unwrap())
                .rounding(egui::Rounding::same(16.0))
                .inner_margin(egui::Margin::symmetric(8.0, 2.0))
                .show(ui, |ui| {
                    ui.label(RichText::new("Error").color(tc).size(11.0).strong())
                        .on_hover_text(err);
                });
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::button(
                ui,
                "⌨ Record Combo",
                ButtonKind::Outlined,
                !state.recording(),
            )
            .clicked()
            {
                mp.combo = ComboState::default();
                open_dialog = Some(Dialog::Combo);
            }
            if widgets::button(
                ui,
                "⏺ Record new macro",
                ButtonKind::Contained(Color32::from_rgb(211, 47, 47)),
                !state.recording(),
            )
            .clicked()
            {
                mp.recorder = RecorderState::default();
                open_dialog = Some(Dialog::Recorder);
            }
        });
    });
    ui.add_space(6.0);

    // ── Alerts ──────────────────────────────────────────────────────
    if state.macro_state.gkey_mode == GkeyMode::Hw && state.has_bindings() {
        widgets::alert(
            ui,
            AlertKind::Info,
            "Macros are released to hardware mode — G-keys send their stock chords. Press a macro's G chip to reassign, or use “Take over G-keys”.",
        );
        ui.add_space(4.0);
    }
    if state.has_bindings() && !state.macro_state.gbits_calibrated {
        ui.horizontal(|ui| {
            widgets::alert(
                ui,
                AlertKind::Warning,
                "G-key bit layout not calibrated — macros may not trigger.",
            );
            if ui.button("Calibrate").clicked() {
                mp.calibration = CalibrationState::default();
                open_dialog = Some(Dialog::Calibration);
            }
        });
        ui.add_space(4.0);
    }

    // ── Cards grid ──────────────────────────────────────────────────
    let mut macros: Vec<Macro> = state.macro_state.macros.values().cloned().collect();
    macros.sort_by(|a, b| b.created.cmp(&a.created));

    if macros.is_empty() {
        widgets::caption(ui, "No macros yet — record one to get started.");
    } else {
        let card_width = 320.0;
        let columns = ((ui.available_width() + 8.0) / (card_width + 8.0))
            .floor()
            .max(1.0) as usize;

        // A column grid, not a horizontal row: ui.horizontal() cross-aligns
        // items Center, and each card is allocated with a 0-height desired
        // size, so cards got centered on the row as it grew — the diagonal
        // stagger where every card sat a few px lower than its left neighbor.
        // `Ui::columns` places all column tops at the same cursor instead.
        ui.columns(columns, |cols| {
            for (ci, col) in cols.iter_mut().enumerate() {
                col.spacing_mut().item_spacing = egui::vec2(0.0, 8.0);
                let card_w = col.available_width();
                for chunk in macros.chunks(columns) {
                    let Some(m) = chunk.get(ci) else { break };
                    let m = m.clone();
                    col.allocate_ui_with_layout(
                        Vec2::new(card_w, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_min_width(card_w);
                            ui.set_max_width(card_w);
                            macro_card(ui, state, mp, &m, &mut action, &mut open_dialog);
                        },
                    );
                }
            }
        });
    }

    ui.add_space(8.0);

    // ── G1–G6 keycap strip ──────────────────────────────────────────
    ui.horizontal(|ui| {
        for g in 1..=GKEY_COUNT {
            keycap(ui, state, mp, g, &mut action, &mut open_dialog);
            ui.add_space(6.0);
        }
    });

    ui.add_space(8.0);

    // ── Footer row ──────────────────────────────────────────────────
    ui.horizontal(|ui| {
        let mode = state.macro_state.gkey_mode;
        let mode_label = match mode {
            GkeyMode::Sw => "Macro mode (SW)",
            GkeyMode::Hw => "Hardware mode (HW)",
        };
        let mode_color = if mode == GkeyMode::Sw {
            crate::ui::theme::PINK
        } else {
            Color32::from_gray(80)
        };
        widgets::chip(ui, mode_label, Some(mode_color), Color32::WHITE);

        let can_release = mode == GkeyMode::Sw && !state.recording();
        if can_release {
            if widgets::button(ui, "Release to hardware", ButtonKind::Outlined, true).clicked() {
                action = Some(PanelAction::ReleaseToHw);
            }
        }
        if widgets::button(ui, "Calibrate G-keys", ButtonKind::Outlined, !state.recording())
            .clicked()
        {
            mp.calibration = CalibrationState::default();
            open_dialog = Some(Dialog::Calibration);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let caption = if state.recording() {
                "Recording in progress…"
            } else if mode == GkeyMode::Sw {
                "G-keys trigger assigned macros (stock chords suppressed)."
            } else {
                "G-keys send their stock Ctrl+Shift+F1…F6 chords."
            };
            widgets::caption(ui, caption);
        });
    });

    // ── Deferred actions ────────────────────────────────────────────
    if let Some(a) = action {
        match a {
            PanelAction::StopPlay => state.send_macro(MacroCmd::StopPlay),
            PanelAction::ReleaseToHw => state.send_macro(MacroCmd::ReleaseToHw),
            PanelAction::TestMacro { events, speed, repeat } => {
                state.send_macro(MacroCmd::Play {
                    events,
                    speed,
                    repeat,
                })
            }
            PanelAction::Assign { slot, macro_id } => {
                state.send_macro(MacroCmd::SetBinding(
                    slot,
                    Some(GKeyBinding { macro_id }),
                ));
            }
            PanelAction::Unbind { slot } => state.send_macro(MacroCmd::SetBinding(slot, None)),
            PanelAction::Delete { id } => state.send_macro(MacroCmd::DeleteMacro(id)),
        }
    }
    if let Some(d) = open_dialog {
        state.clear_rec_queues();
        state.clear_ifc2_queue();
        mp.dialog = Some(d);
    }

    // ── Dialogs ─────────────────────────────────────────────────────
    match mp.dialog.clone() {
        Some(Dialog::Recorder) => recorder_dialog(ui, state, mp),
        Some(Dialog::Combo) => combo_dialog(ui, state, mp),
        Some(Dialog::Calibration) => calibration_dialog(ui, state, mp),
        None => {}
    }
}

enum PanelAction {
    StopPlay,
    ReleaseToHw,
    TestMacro {
        events: Vec<MacroEvent>,
        speed: f64,
        repeat: bool,
    },
    Assign {
        slot: usize,
        macro_id: String,
    },
    Unbind {
        slot: usize,
    },
    Delete {
        id: String,
    },
}

fn macro_card(
    ui: &mut egui::Ui,
    state: &mut AppState,
    mp: &mut MacroPanelState,
    m: &Macro,
    action: &mut Option<PanelAction>,
    open_dialog: &mut Option<Dialog>,
) {
    let is_combo = m.kind == Some(MacroKind::Combo);
    let action_kind = m.action.unwrap_or(GKeyAction::Once);
    let speed = m.speed.unwrap_or(1.0);
    let duration: u64 = m.events.iter().map(|e| e.dt).sum();
    let secs = duration as f64 / 1000.0;

    egui::Frame::none()
        .fill(crate::ui::theme::PAPER)
        .stroke(egui::Stroke::new(
            1.0_f32,
            crate::ui::theme::DIVIDER,
        ))
        .rounding(egui::Rounding::same(8.0))
        .shadow(crate::ui::theme::paper_shadow())
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());

            // Row 1: name + combo chip + action buttons
            ui.horizontal(|ui| {
                let name = if m.name.chars().count() > 30 {
                    format!("{}…", m.name.chars().take(30).collect::<String>())
                } else {
                    m.name.clone()
                };
                ui.label(RichText::new(name).strong().size(13.5));
                if is_combo {
                    widgets::chip(ui, "Combo", None, Color32::from_gray(210));
                }

                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        // Delete
                        if widgets::icon_button(ui, "🗑", "Delete", true).clicked() {
                            *action = Some(PanelAction::Delete { id: m.id.clone() });
                        }
                        // Assign to G
                        let assign_label = if mp.assign_menu_for.as_deref() == Some(m.id.as_str()) {
                            "Assign ▴"
                        } else {
                            "Assign ▾"
                        };
                        let assign_resp = ui
                            .add(egui::Button::new(RichText::new(assign_label).size(12.0)));
                        if assign_resp.clicked() {
                            mp.assign_menu_for = if mp.assign_menu_for.as_deref() == Some(m.id.as_str())
                            {
                                None
                            } else {
                                Some(m.id.clone())
                            };
                        }
                        if mp.assign_menu_for.as_deref() == Some(m.id.as_str()) {
                            egui::popup::popup_below_widget(
                                ui,
                                egui::Id::new(("assign", m.id.clone())),
                                &assign_resp,
                                egui::popup::PopupCloseBehavior::CloseOnClickOutside,
                                |ui: &mut egui::Ui| {
                                    for g in 1..=GKEY_COUNT {
                                        let bound = state
                                            .macro_state
                                            .gkeys
                                            .get(&g.to_string())
                                            .map(|b| b.macro_id == m.id)
                                            .unwrap_or(false);
                                        let label = if state.macro_state.gkeys.contains_key(&g.to_string()) {
                                            format!("Replace G{g} → {}", m.name)
                                        } else {
                                            format!("Assign to G{g}")
                                        };
                                        if ui.selectable_label(bound, label).clicked() {
                                            *action = Some(PanelAction::Assign {
                                                slot: g,
                                                macro_id: m.id.clone(),
                                            });
                                            mp.assign_menu_for = None;
                                        }
                                    }
                                },
                            );
                        }
                        // Test
                        if widgets::icon_button(ui, "▶", "Test", !state.recording()).clicked() {
                            *action = Some(PanelAction::TestMacro {
                                events: m.events.clone(),
                                speed,
                                repeat: action_kind != GKeyAction::Once,
                            });
                        }
                        // Edit
                        if widgets::icon_button(ui, "✏", "Edit", !state.recording()).clicked() {
                            if is_combo {
                                mp.combo = ComboState::editing(m.clone());
                                *open_dialog = Some(Dialog::Combo);
                            } else {
                                mp.recorder = RecorderState::editing(m.clone());
                                *open_dialog = Some(Dialog::Recorder);
                            }
                        }
                    },
                );
            });

            // Row 2: caption
            let action_name = ACTION_LABELS
                .iter()
                .find(|(a, _)| *a == action_kind)
                .map(|(_, l)| *l)
                .unwrap_or("?");
            ui.label(
                RichText::new(format!(
                    "{} events · {:.1}s · {} · {}×",
                    m.events.len(),
                    secs,
                    action_name,
                    speed
                ))
                .size(11.0)
                .color(Color32::from_gray(160)),
            );

            // Row 3: bound G chips
            let mut unbind: Option<usize> = None;
            let bound_slots: Vec<usize> = state
                .macro_state
                .gkeys
                .iter()
                .filter(|(_, b)| b.macro_id == m.id)
                .filter_map(|(k, _)| k.parse::<usize>().ok())
                .collect();
            if !bound_slots.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for slot in bound_slots {
                        egui::Frame::none()
                            .fill(crate::ui::theme::PINK)
                            .rounding(egui::Rounding::same(12.0))
                            .inner_margin(egui::Margin::symmetric(6.0, 2.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!("G{slot}")).size(11.0).strong().color(Color32::WHITE),
                                    );
                                    if ui
                                        .add(egui::Button::new(RichText::new("×").size(9.0).color(Color32::WHITE)).frame(false))
                                        .clicked()
                                    {
                                        unbind = Some(slot);
                                    }
                                });
                            });
                        ui.add_space(4.0);
                    }
                });
            }
            if let Some(slot) = unbind {
                *action = Some(PanelAction::Unbind { slot });
            }
        });
}

fn keycap(
    ui: &mut egui::Ui,
    state: &mut AppState,
    mp: &mut MacroPanelState,
    g: usize,
    action: &mut Option<PanelAction>,
    open_dialog: &mut Option<Dialog>,
) {
    let pressed = state.pressed_g.get(g - 1).copied().unwrap_or(false);
    let active_slot = state
        .macro_status
        .as_ref()
        .and_then(|s| s.active_slot)
        .map(|s| s + 1);
    let is_active = active_slot == Some(g);

    let binding = state.macro_state.gkeys.get(&g.to_string()).cloned();
    let macro_name = binding
        .as_ref()
        .and_then(|b| state.macro_by_id(&b.macro_id))
        .map(|m| m.name.clone());

    let border = if pressed {
        Color32::from_rgb(156, 39, 176)
    } else if is_active {
        Color32::from_rgb(46, 125, 50)
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke.color
    };
    let fill = if pressed {
        Color32::from_gray(70)
    } else {
        ui.visuals().panel_fill
    };

    // Allocate an exact 74×54 cap — a Frame inside a horizontal row inherits
    // the whole remaining width, which would let G1 swallow the strip.
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(74.0, 54.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::Rounding::same(6.0), fill);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(6.0),
        egui::Stroke::new(if pressed { 2.0 } else { 1.0_f32 }, border),
    );
    painter.text(
        rect.center() - Vec2::new(0.0, 7.0),
        egui::Align2::CENTER_CENTER,
        format!("G{g}"),
        egui::FontId::proportional(14.0),
        Color32::WHITE,
    );
    let name = macro_name.unwrap_or_else(|| "—".into());
    let name = if name.chars().count() > 9 {
        format!("{}…", name.chars().take(9).collect::<String>())
    } else {
        name
    };
    painter.text(
        rect.center() + Vec2::new(0.0, 10.0),
        egui::Align2::CENTER_CENTER,
        name,
        egui::FontId::proportional(9.5),
        if binding.is_some() {
            Color32::from_gray(210)
        } else {
            Color32::from_gray(120)
        },
    );
    let resp = resp.on_hover_text(if let Some(b) = &binding {
        format!("G{g}: {}", b.macro_id)
    } else {
        format!("G{g}: unbound")
    });

    if resp.clicked() {
        mp.keycap_menu_for = if mp.keycap_menu_for == Some(g) {
            None
        } else {
            Some(g)
        };
    }

    if mp.keycap_menu_for == Some(g) {
        egui::popup::popup_below_widget(
            ui,
            egui::Id::new(("keycap", g)),
            &resp,
            egui::popup::PopupCloseBehavior::CloseOnClickOutside,
            |ui: &mut egui::Ui| {
                if binding.is_some()
                    && ui.selectable_label(false, format!("Unbind G{g}")).clicked()
                {
                    *action = Some(PanelAction::Unbind { slot: g });
                    mp.keycap_menu_for = None;
                }
                if binding.is_some() {
                    ui.separator();
                }
                let mut macros: Vec<Macro> = state.macro_state.macros.values().cloned().collect();
                macros.sort_by(|a, b| b.created.cmp(&a.created));
                for m in macros {
                    let selected = binding.as_ref().is_some_and(|b| b.macro_id == m.id);
                    let label = if binding.is_some() {
                        format!("Replace → {}", m.name)
                    } else {
                        m.name.clone()
                    };
                    if ui.selectable_label(selected, label).clicked() {
                        *action = Some(PanelAction::Assign {
                            slot: g,
                            macro_id: m.id.clone(),
                        });
                        mp.keycap_menu_for = None;
                    }
                }
                if state.macro_state.macros.is_empty() {
                    widgets::caption(ui, "No macros yet");
                }
                let _ = open_dialog;
            },
        );
    }
}

// ─── Recorder dialog ───────────────────────────────────────────────────

#[derive(Default)]
pub struct RecorderState {
    pub phase: RecPhase,
    pub editing: Option<Macro>,
    pub raw: Vec<MacroEvent>,
    pub name: String,
    pub fixed_mode: bool,
    pub fixed_ms: u64,
    pub action: GKeyAction,
    pub speed: f64,
    pub dt_drafts: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum RecPhase {
    #[default]
    Starting,
    Recording,
    Review,
}

impl RecorderState {
    pub fn editing(m: Macro) -> Self {
        let fixed_ms = 100;
        RecorderState {
            phase: RecPhase::Review,
            editing: Some(m.clone()),
            name: m.name.clone(),
            action: m.action.unwrap_or(GKeyAction::Once),
            speed: m.speed.unwrap_or(1.0),
            fixed_ms,
            ..Default::default()
        }
    }
    fn derived(&self) -> Vec<MacroEvent> {
        if self.editing.as_ref().is_some_and(|m| m.kind == Some(MacroKind::Combo)) {
            return self.raw.clone();
        }
        if !self.fixed_mode {
            return self.raw.clone();
        }
        let mut out = Vec::with_capacity(self.raw.len());
        for (i, e) in self.raw.iter().enumerate() {
            let mut e = e.clone();
            e.dt = if i == 0 { 0 } else { self.fixed_ms };
            out.push(e);
        }
        out
    }
}

fn recorder_dialog(ui: &mut egui::Ui, state: &mut AppState, mp: &mut MacroPanelState) {
    let rec = &mut mp.recorder;
    let editing = rec.editing.is_some();
    let mut close = false;

    // Kick off recording on first frame
    if rec.phase == RecPhase::Starting && !editing && !state.recording() {
        state.send_macro(MacroCmd::RecordStart);
    }
    if rec.phase == RecPhase::Starting && state.recording() {
        rec.phase = RecPhase::Recording;
    }

    // Consume events
    if let Some((ok, err)) = state.rec_started.take() {
        if ok {
            rec.phase = RecPhase::Recording;
            rec.error = None;
        } else {
            rec.error = Some(err.unwrap_or_else(|| "recording unavailable".into()));
            rec.phase = RecPhase::Review; // show retry
        }
    }
    if let Some(stopped) = state.rec_stopped.take() {
        if !stopped.events.is_empty() {
            rec.raw = stopped.events;
            rec.phase = RecPhase::Review;
            rec.dt_drafts.clear();
        } else if !stopped.ok {
            rec.error = Some(stopped.err.unwrap_or("recording failed".into()));
        }
    }
    // Live events during recording
    if rec.phase == RecPhase::Recording && !state.rec_events.is_empty() {
        rec.raw.extend(state.rec_events.drain(..));
    }

    let title = if editing {
        "Edit macro"
    } else {
        "Record macro"
    };
    let mut open = true;
    egui::Window::new(title)
        .id(egui::Id::new("recorder-dialog"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(520.0)
        .show(ui.ctx(), |ui| {
            match rec.phase {
                RecPhase::Starting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Starting recorder…");
                    });
                }
                RecPhase::Recording => {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("⏺ REC")
                                .color(Color32::from_rgb(211, 47, 47))
                                .strong(),
                        );
                        ui.spinner();
                    });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("{} events", rec.raw.len()))
                            .size(20.0)
                            .strong(),
                    );
                    // Last 8 event chips
                    ui.horizontal_wrapped(|ui| {
                        for e in rec.raw.iter().rev().take(8) {
                            let text = match e.kind {
                                MacroEventKind::Key => format!("{} ↓", e.code),
                                MacroEventKind::Rel => format!("{} +{}", e.code, e.value),
                            };
                            widgets::chip(ui, &text, None, Color32::from_gray(210));
                        }
                    });
                }
                RecPhase::Review => {
                    if let Some(err) = &rec.error {
                        widgets::alert(ui, AlertKind::Error, err);
                        ui.add_space(4.0);
                    }
                    // Name
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Name").strong().size(13.0));
                        ui.add(
                            egui::TextEdit::singleline(&mut rec.name)
                                .desired_width(260.0)
                                .char_limit(60),
                        );
                    });
                    ui.add_space(4.0);
                    // Delay mode
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Delays").size(13.0));
                        let mut realtime = !rec.fixed_mode;
                        if ui.selectable_value(&mut realtime, true, "As typed").clicked() {
                            rec.fixed_mode = false;
                        }
                        if ui.selectable_value(&mut realtime, false, "Fixed").clicked() {
                            rec.fixed_mode = true;
                        }
                        if rec.fixed_mode {
                            let mut ms = rec.fixed_ms as f64;
                            if ui
                                .add(egui::Slider::new(&mut ms, 0.0..=60000.0).fixed_decimals(0).suffix(" ms"))
                                .changed()
                            {
                                rec.fixed_ms = ms.round() as u64;
                            }
                        }
                    });
                    ui.add_space(4.0);
                    // Playback controls
                    playback_controls(ui, &mut rec.action, &mut rec.speed);
                    ui.add_space(4.0);
                    // Chips
                    ui.horizontal_wrapped(|ui| {
                        let derived = rec.derived();
                        widgets::chip(
                            ui,
                            &format!("{} events", derived.len()),
                            None,
                            Color32::from_gray(210),
                        );
                        let total: u64 = derived.iter().map(|e| e.dt).sum();
                        widgets::chip(
                            ui,
                            &format!("{} ms", total),
                            None,
                            Color32::from_gray(210),
                        );
                    });
                    ui.add_space(4.0);
                    // Event list with editable dt — egui::Grid so the dt /
                    // key / direction / remove columns line up on every row
                    // (the chips are variable-width, so plain ui.horizontal
                    // made each row drift).
                    egui::ScrollArea::vertical()
                        .id_salt("recorder-events")
                        .max_height(260.0)
                        .show(ui, |ui| {
                            let derived = rec.derived();
                            let mut remove: Option<usize> = None;
                            egui::Grid::new("recorder-events-grid")
                                .num_columns(3)
                                .spacing([8.0, 3.0])
                                .show(ui, |ui| {
                                    for (i, e) in derived.iter().enumerate() {
                                        // dt edit
                                        let mut draft = rec
                                            .dt_drafts
                                            .get(i)
                                            .cloned()
                                            .unwrap_or_else(|| e.dt.to_string());
                                        let resp = ui.add(
                                            egui::TextEdit::singleline(&mut draft)
                                                .desired_width(64.0)
                                                .hint_text("dt ms"),
                                        );
                                        if resp.changed() {
                                            if rec.dt_drafts.len() <= i {
                                                rec.dt_drafts.resize(i + 1, String::new());
                                            }
                                            rec.dt_drafts[i] = draft.clone();
                                            if let Ok(v) = draft.trim().parse::<u64>() {
                                                if let Some(ev) = rec.raw.get_mut(i) {
                                                    if i != 0 {
                                                        ev.dt = v;
                                                    }
                                                }
                                            }
                                        }
                                        let kind_icon = match e.kind {
                                            MacroEventKind::Key => "⌨",
                                            MacroEventKind::Rel => "↕",
                                        };
                                        ui.horizontal(|ui| {
                                            widgets::chip(
                                                ui,
                                                &format!("{} {}", kind_icon, e.code),
                                                None,
                                                Color32::from_gray(70),
                                            );
                                            let val_text = match e.kind {
                                                MacroEventKind::Key => {
                                                    if e.value == 1 {
                                                        "down".to_string()
                                                    } else {
                                                        "up".to_string()
                                                    }
                                                }
                                                MacroEventKind::Rel => format!("×{}", e.value),
                                            };
                                            ui.label(
                                                RichText::new(val_text)
                                                    .size(11.5)
                                                    .color(Color32::from_gray(170)),
                                            );
                                        });
                                        if ui.button("×").clicked() {
                                            remove = Some(i);
                                        }
                                        ui.end_row(); // next row
                                    }
                                });
                            if let Some(i) = remove {
                                rec.raw.remove(i);
                                if i < rec.dt_drafts.len() {
                                    rec.dt_drafts.remove(i);
                                }
                            }
                        });
                }
            }

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                match rec.phase {
                    RecPhase::Starting => {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    }
                    RecPhase::Recording => {
                        if widgets::button(ui, "⏹ Stop & review", ButtonKind::Contained(Color32::from_rgb(211, 47, 47)), true)
                            .clicked()
                        {
                            state.send_macro(MacroCmd::RecordStop);
                            rec.phase = RecPhase::Review;
                        }
                        if ui.button("Discard").clicked() {
                            state.send_macro(MacroCmd::RecordStop);
                            close = true;
                        }
                    }
                    RecPhase::Review => {
                        if widgets::button(ui, "Record again", ButtonKind::Outlined, !state.recording())
                            .clicked()
                        {
                            rec.raw.clear();
                            rec.dt_drafts.clear();
                            rec.error = None;
                            rec.editing = None;
                            state.send_macro(MacroCmd::RecordStart);
                            rec.phase = RecPhase::Starting;
                        }
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if widgets::button(
                                    ui,
                                    "Save",
                                    ButtonKind::Contained(Color32::from_rgb(46, 125, 50)),
                                    !rec.name.is_empty() && !rec.derived().is_empty(),
                                )
                                .clicked()
                                {
                                    let id = rec
                                        .editing
                                        .as_ref()
                                        .map(|m| m.id.clone())
                                        .unwrap_or_else(|| format!("macro_{}", crate::core::config::now_ms()));
                                    let created = rec
                                        .editing
                                        .as_ref()
                                        .map(|m| m.created)
                                        .unwrap_or_else(crate::core::config::now_ms);
                                    let events = rec.derived();
                                    let m = Macro {
                                        id,
                                        name: rec.name.trim().to_string(),
                                        events,
                                        created,
                                        kind: None,
                                        action: Some(rec.action),
                                        speed: Some(rec.speed),
                                    };
                                    state.send_macro(MacroCmd::SaveMacro(m));
                                    close = true;
                                }
                                if ui.button("Discard").clicked() {
                                    close = true;
                                }
                            },
                        );
                    }
                }
            });
        });

    if !open || close {
        // Ensure a still-running recording is stopped when cancelled.
        if mp.recorder.phase == RecPhase::Recording && state.recording() {
            state.send_macro(MacroCmd::RecordStop);
        }
        state.clear_rec_queues();
        mp.dialog = None;
        mp.recorder = RecorderState::default();
    }
}

// ─── Combo dialog ──────────────────────────────────────────────────────

#[derive(Default)]
pub struct ComboState {
    pub phase: RecPhase,
    pub editing: Option<Macro>,
    pub held: Vec<String>,
    pub chord: Vec<String>,
    pub name: String,
    pub action: GKeyAction,
    pub speed: f64,
    pub error: Option<String>,
}

impl ComboState {
    pub fn editing(m: Macro) -> Self {
        ComboState {
            phase: RecPhase::Review,
            editing: Some(m.clone()),
            name: m.name.clone(),
            action: m.action.unwrap_or(GKeyAction::Once),
            speed: m.speed.unwrap_or(1.0),
            chord: m
                .events
                .iter()
                .filter(|e| e.kind == MacroEventKind::Key && e.value == 1)
                .map(|e| e.code.clone())
                .collect(),
            ..Default::default()
        }
    }
}

const MOD_RANK: [(&str, u32); 8] = [
    ("KEY_LEFTCTRL", 10),
    ("KEY_RIGHTCTRL", 11),
    ("KEY_LEFTSHIFT", 20),
    ("KEY_RIGHTSHIFT", 21),
    ("KEY_LEFTALT", 30),
    ("KEY_RIGHTALT", 31),
    ("KEY_LEFTMETA", 40),
    ("KEY_RIGHTMETA", 41),
];

fn is_modifier(code: &str) -> bool {
    MOD_RANK.iter().any(|(m, _)| *m == code)
}

/// TS ComboDialog normalizeChord: dedupe, sort modifiers canonically, require
/// ≥1 non-modifier, emit dt-0 first key then 10 ms gaps, downs then reversed ups.
pub fn normalize_chord(presses: &[String]) -> (Vec<MacroEvent>, String) {
    let mut seen: Vec<String> = Vec::new();
    for p in presses {
        if !seen.contains(p) {
            seen.push(p.clone());
        }
    }
    let mut mods: Vec<String> = seen.iter().filter(|c| is_modifier(c)).cloned().collect();
    mods.sort_by_key(|c| {
        MOD_RANK
            .iter()
            .find(|(m, _)| m == c)
            .map(|(_, r)| *r)
            .unwrap_or(999)
    });
    let mut mains: Vec<String> = seen.iter().filter(|c| !is_modifier(c)).cloned().collect();
    mains.sort();

    let mut ordered = mods;
    ordered.extend(mains.iter().cloned());

    let mut events = Vec::new();
    for (i, code) in ordered.iter().enumerate() {
        events.push(MacroEvent {
            dt: if i == 0 { 0 } else { 10 },
            kind: MacroEventKind::Key,
            code: code.clone(),
            value: 1,
        });
    }
    for (i, code) in ordered.iter().enumerate().rev() {
        events.push(MacroEvent {
            dt: 10,
            kind: MacroEventKind::Key,
            code: code.clone(),
            value: 0,
        });
        let _ = i;
    }

    // Display name: one label per modifier rank + main keys
    let mut name_parts: Vec<String> = Vec::new();
    let mut used_ranks: Vec<u32> = Vec::new();
    for c in &ordered {
        if let Some((_, rank)) = MOD_RANK.iter().find(|(m, _)| m == c) {
            if !used_ranks.contains(rank) {
                used_ranks.push(*rank);
                name_parts.push(mod_label(c).to_string());
            }
        }
    }
    for c in &mains {
        name_parts.push(key_label(c));
    }
    (events, name_parts.join("+"))
}

fn mod_label(code: &str) -> &'static str {
    match code {
        c if c.contains("CTRL") => "Ctrl",
        c if c.contains("SHIFT") => "Shift",
        c if c.contains("ALT") => "Alt",
        c if c.contains("META") => "Super",
        _ => "?",
    }
}

fn key_label(code: &str) -> String {
    let stripped = code
        .strip_prefix("KEY_")
        .or_else(|| code.strip_prefix("BTN_"))
        .unwrap_or(code);
    stripped
        .to_lowercase()
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn combo_dialog(ui: &mut egui::Ui, state: &mut AppState, mp: &mut MacroPanelState) {
    let cb = &mut mp.combo;
    let editing = cb.editing.is_some();
    let mut close = false;

    if cb.phase == RecPhase::Starting && !editing && !state.recording() {
        state.send_macro(MacroCmd::RecordStart);
    }
    if cb.phase == RecPhase::Starting && state.recording() {
        cb.phase = RecPhase::Recording;
    }
    if let Some((ok, err)) = state.rec_started.take() {
        if ok {
            cb.phase = RecPhase::Recording;
        } else {
            cb.error = Some(err.unwrap_or_else(|| "recording unavailable".into()));
            cb.phase = RecPhase::Review;
        }
    }

    // Chord tracking from live events
    if cb.phase == RecPhase::Recording {
        let events: Vec<MacroEvent> = state.rec_events.drain(..).collect();
        for e in events {
            if e.kind != MacroEventKind::Key {
                continue;
            }
            if e.value == 1 {
                if !cb.held.contains(&e.code) {
                    cb.held.push(e.code.clone());
                }
                if !cb.chord.contains(&e.code) {
                    cb.chord.push(e.code.clone());
                }
            } else {
                cb.held.retain(|c| c != &e.code);
            }
        }
        // Auto-stop when all keys released after ≥1 press
        if cb.held.is_empty() && !cb.chord.is_empty() {
            state.send_macro(MacroCmd::RecordStop);
            let (events, name) = normalize_chord(&cb.chord);
            cb.chord = cb.chord.clone();
            if cb.name.is_empty() {
                cb.name = name;
            }
            let _ = events;
            cb.phase = RecPhase::Review;
        }
    }
    if let Some(stopped) = state.rec_stopped.take() {
        if cb.phase == RecPhase::Recording {
            if !cb.chord.is_empty() {
                if cb.name.is_empty() {
                    cb.name = normalize_chord(&cb.chord).1;
                }
                cb.phase = RecPhase::Review;
            } else {
                cb.error = Some(stopped.err.unwrap_or_else(|| "No keys captured".into()));
                cb.phase = RecPhase::Review;
            }
        }
    }

    let mut open = true;
    egui::Window::new(if editing { "Edit combo" } else { "Record combo" })
        .id(egui::Id::new("combo-dialog"))
        .open(&mut open)
        .collapsible(false)
        .default_width(440.0)
        .show(ui.ctx(), |ui| {
            match cb.phase {
                RecPhase::Starting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Waiting for keyboard…");
                    });
                }
                RecPhase::Recording => {
                    ui.label(
                        RichText::new("Press the key combination, then release…")
                            .size(13.0),
                    );
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        for c in &cb.chord {
                            let held_now = cb.held.contains(c);
                            let fill = if held_now {
                                Some(crate::ui::theme::PINK)
                            } else {
                                None
                            };
                            let label = key_label(c);
                            widgets::chip(
                                ui,
                                &format!("{label}{}", if held_now { " ↓" } else { "" }),
                                fill,
                                if held_now {
                                    Color32::WHITE
                                } else {
                                    Color32::from_gray(210)
                                },
                            );
                        }
                        if cb.chord.is_empty() {
                            widgets::caption(ui, "Listening…");
                        }
                    });
                }
                RecPhase::Review => {
                    if let Some(err) = &cb.error {
                        widgets::alert(ui, AlertKind::Error, err);
                        ui.add_space(4.0);
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Name").strong().size(13.0));
                        ui.add(
                            egui::TextEdit::singleline(&mut cb.name).desired_width(240.0).char_limit(60),
                        );
                    });
                    ui.add_space(4.0);
                    playback_controls(ui, &mut cb.action, &mut cb.speed);
                    ui.add_space(4.0);
                    // Chord chips
                    ui.horizontal_wrapped(|ui| {
                        for c in &cb.chord {
                            widgets::chip(ui, &key_label(c), None, Color32::from_gray(210));
                        }
                        if let Some(first) = cb.chord.first() {
                            let _ = first;
                        }
                    });
                    ui.add_space(3.0);
                    let (events, _) = normalize_chord(&cb.chord);
                    let total: u64 = events.iter().map(|e| e.dt).sum();
                    ui.label(
                        RichText::new(format!(
                            "{} events · {} ms · plays like a macro on its G-key",
                            events.len(),
                            total
                        ))
                        .size(11.5)
                        .color(Color32::from_gray(160)),
                    );
                }
            }

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                match cb.phase {
                    RecPhase::Starting => {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    }
                    RecPhase::Recording => {
                        if ui.button("Cancel").clicked() {
                            state.send_macro(MacroCmd::RecordStop);
                            close = true;
                        }
                    }
                    RecPhase::Review => {
                        if widgets::button(ui, "Re-record", ButtonKind::Outlined, !state.recording())
                            .clicked()
                        {
                            cb.chord.clear();
                            cb.held.clear();
                            cb.error = None;
                            cb.editing = None;
                            state.send_macro(MacroCmd::RecordStart);
                            cb.phase = RecPhase::Starting;
                        }
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                let (events, _) = normalize_chord(&cb.chord);
                                if widgets::button(
                                    ui,
                                    "Save",
                                    ButtonKind::Contained(Color32::from_rgb(46, 125, 50)),
                                    !cb.name.is_empty() && !events.is_empty(),
                                )
                                .clicked()
                                {
                                    let id = cb
                                        .editing
                                        .as_ref()
                                        .map(|m| m.id.clone())
                                        .unwrap_or_else(|| format!("combo_{}", crate::core::config::now_ms()));
                                    let created = cb
                                        .editing
                                        .as_ref()
                                        .map(|m| m.created)
                                        .unwrap_or_else(crate::core::config::now_ms);
                                    let m = Macro {
                                        id,
                                        name: cb.name.trim().to_string(),
                                        events,
                                        created,
                                        kind: Some(MacroKind::Combo),
                                        action: Some(cb.action),
                                        speed: Some(cb.speed),
                                    };
                                    state.send_macro(MacroCmd::SaveMacro(m));
                                    close = true;
                                }
                            },
                        );
                    }
                }
            });
        });

    if !open || close {
        if mp.combo.phase == RecPhase::Recording && state.recording() {
            state.send_macro(MacroCmd::RecordStop);
        }
        state.clear_rec_queues();
        mp.dialog = None;
        mp.combo = ComboState::default();
    }
}

// ─── Calibration dialog ────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default)]
pub enum CalPhase {
    #[default]
    Intro,
    Run,
    Done,
    Error,
}

#[derive(Default)]
pub struct CalibrationState {
    pub phase: CalPhase,
    pub detected: Vec<u32>,
    pub baseline: Vec<u32>,
    pub message: String,
    pub bit0: Option<u32>,
    pub failed_bits: Vec<u32>,
    // ── state-machine bookkeeping ──
    /// Latest raw IFC2 bits (consumed once per tick).
    pub last_bits: Option<Vec<u32>>,
    /// (bit, first-seen ms) waiting for 800 ms stability.
    pub candidate: Option<(u32, u64)>,
    pub baseline_started: Option<u64>,
    pub baseline_done: bool,
    pub stable_since: Option<u64>,
    /// Set once SetGbit0 has been sent for the current result.
    pub persisted: bool,
}

impl CalibrationState {
    /// Decode set bits from an IFC2 packet hex string (skip the 2 header
    /// bytes the Python helper already excluded? — helper sends the FULL
    /// packet hex; TS CalibrationDialog slices from byte 2).
    pub fn bits_from_hex(hex: &str) -> Vec<u32> {
        let bytes: Vec<u8> = (0..hex.len() / 2)
            .filter_map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok())
            .collect();
        let mut bits = Vec::new();
        // TS: for i in 2..bytes: for b in 0..8: if byte & (1<<b) → bit index
        for (idx, byte) in bytes.iter().enumerate().skip(2) {
            for b in 0..8u32 {
                if byte & (1 << b) != 0 {
                    bits.push((idx as u32) * 8 + b);
                }
            }
        }
        bits
    }
}

fn calibration_dialog(ui: &mut egui::Ui, state: &mut AppState, mp: &mut MacroPanelState) {
    let cal = &mut mp.calibration;
    let mut close = false;
    let mut start_watch = false;
    let mut stop_watch = false;
    let mut set_calibrating: Option<bool> = None;

    let mut open = true;
    egui::Window::new("Calibrate G-keys")
        .id(egui::Id::new("calibration-dialog"))
        .open(&mut open)
        .collapsible(false)
        .default_width(460.0)
        .show(ui.ctx(), |ui| {
            match cal.phase {
                CalPhase::Intro => {
                    ui.label(
                        RichText::new(
                            "This wizard watches the keyboard's internal key bitmap while you press G1…G6, \
                             and verifies the six keys map to six consecutive bits.\n\n\
                             The keyboard will switch to macro (SW) mode during calibration.",
                        )
                        .size(13.0),
                    );
                    ui.add_space(6.0);
                    widgets::alert(
                        ui,
                        AlertKind::Warning,
                        "Release to hardware afterwards if you want the stock chords back.",
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Steps:").strong().size(13.0));
                        ui.label(
                            RichText::new(
                                "Press and release G1, G2, … G6 one at a time.",
                            )
                            .size(13.0),
                        );
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if widgets::button(
                                    ui,
                                    "Start",
                                    ButtonKind::Contained(crate::ui::theme::PINK),
                                    true,
                                )
                                .clicked()
                                {
                                    cal.phase = CalPhase::Run;
                                    cal.detected.clear();
                                    cal.baseline.clear();
                                    cal.message = "Preparing baseline…".into();
                                    start_watch = true;
                                    set_calibrating = Some(true);
                                }
                                if ui.button("Cancel").clicked() {
                                    close = true;
                                }
                            },
                        );
                    });
                }
                CalPhase::Run => {
                    // State machine: collect ~600 ms baseline of quiet traffic,
                    // then one new bit at a time, stable 800 ms → accept.
                    let now = crate::core::config::now_ms();
                    // (deadlines handled via cal.message + first_seen bookkeeping)
                    run_calibration_machine(cal, state, now);

                    // Six circles
                    ui.horizontal(|ui| {
                        for g in 1..=GKEY_COUNT {
                            let done = cal.detected.len() >= g;
                            let current = cal.detected.len() + 1 == g;
                            let (fill, stroke) = if done {
                                (Color32::from_rgb(46, 125, 50), Color32::from_rgb(46, 125, 50))
                            } else if current {
                                (Color32::from_rgb(63, 81, 181), Color32::from_rgb(100, 120, 255))
                            } else {
                                (Color32::from_gray(50), Color32::from_gray(90))
                            };
                            let (resp, _) = ui.allocate_painter(
                                Vec2::new(52.0, 52.0),
                                egui::Sense::hover(),
                            );
                            let c = resp.rect.center();
                            let r = 22.0;
                            ui.painter()
                                .circle_filled(c, r, fill);
                            ui.painter().circle_stroke(
                                c,
                                r,
                                egui::Stroke::new(2.0_f32, stroke),
                            );
                            let label = format!("G{g}");
                            ui.painter().text(
                                c,
                                egui::Align2::CENTER_CENTER,
                                label,
                                egui::FontId::proportional(14.0),
                                Color32::WHITE,
                            );
                            if done {
                                let bit = cal.detected[g - 1];
                                ui.painter().text(
                                    egui::pos2(c.x, resp.rect.bottom() - 4.0),
                                    egui::Align2::CENTER_BOTTOM,
                                    format!("bit {bit}"),
                                    egui::FontId::proportional(9.0),
                                    Color32::from_gray(170),
                                );
                            }
                            ui.add_space(4.0);
                        }
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(cal.message.clone()).size(13.0));
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            stop_watch = true;
                            set_calibrating = Some(false);
                            close = true;
                        }
                    });
                }
                CalPhase::Done => {
                    if let Some(bit0) = cal.bit0 {
                        widgets::alert(
                            ui,
                            AlertKind::Info,
                            &format!(
                                "Calibration successful! G1→bit {bit0} … G6→bit {}.",
                                bit0 + 5
                            ),
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if widgets::button(
                                    ui,
                                    "Done",
                                    ButtonKind::Contained(Color32::from_rgb(46, 125, 50)),
                                    true,
                                )
                                .clicked()
                                {
                                    close = true;
                                }
                            },
                        );
                    });
                }
                CalPhase::Error => {
                    widgets::alert(
                        ui,
                        AlertKind::Error,
                        &format!(
                            "Calibration failed: {}",
                            if cal.failed_bits.len() > 1 {
                                format!(
                                    "saw {} keys at once (bits {:?})",
                                    cal.failed_bits.len(),
                                    cal.failed_bits
                                )
                            } else {
                                cal.message.clone()
                            }
                        ),
                    );
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        for b in &cal.failed_bits {
                            widgets::chip(ui, &format!("bit {b}"), None, Color32::from_gray(200));
                        }
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if widgets::button(
                                    ui,
                                    "Try again",
                                    ButtonKind::Contained(crate::ui::theme::PINK),
                                    true,
                                )
                                .clicked()
                                {
                                    cal.phase = CalPhase::Run;
                                    cal.detected.clear();
                                    cal.baseline.clear();
                                    cal.failed_bits.clear();
                                    cal.persisted = false;
                                    cal.baseline_started = None;
                                    cal.baseline_done = false;
                                    cal.candidate = None;
                                    cal.message = "Preparing baseline…".into();
                                    start_watch = true;
                                    set_calibrating = Some(true);
                                }
                            },
                        );
                    });
                }
            }
        });

    // Consume IFC2 packets while running
    if cal.phase == CalPhase::Run && !state.ifc2_packets.is_empty() {
        for (_t, hex) in state.ifc2_packets.drain(..) {
            let bits = CalibrationState::bits_from_hex(&hex);
            cal.last_bits = Some(bits);
        }
    }

    // Persist a successful calibration exactly once.
    if cal.phase == CalPhase::Done && !cal.persisted {
        if let Some(bit0) = cal.bit0 {
            state.send_macro(MacroCmd::SetGbit0(bit0));
            state.send_macro(MacroCmd::Watch(0));
            state.send_macro(MacroCmd::SetCalibrating(false));
            cal.persisted = true;
        }
    }
    if cal.phase == CalPhase::Error && !cal.persisted && cal.failed_bits.len() > 1 {
        // Abort the watch on multi-key failure too (TS stops on error).
        state.send_macro(MacroCmd::Watch(0));
        state.send_macro(MacroCmd::SetCalibrating(false));
        cal.persisted = true;
    }

    if start_watch {
        state.clear_ifc2_queue();
        state.send_macro(MacroCmd::SetCalibrating(true));
        state.send_macro(MacroCmd::Watch(3600));
    }
    if stop_watch {
        state.send_macro(MacroCmd::Watch(0));
        state.send_macro(MacroCmd::SetCalibrating(false));
        state.clear_ifc2_queue();
    }
    if let Some(on) = set_calibrating {
        if !on {
            state.send_macro(MacroCmd::SetCalibrating(false));
        }
    }

    if !open || close {
        if mp.calibration.phase == CalPhase::Run {
            state.send_macro(MacroCmd::Watch(0));
            state.send_macro(MacroCmd::SetCalibrating(false));
        }
        state.clear_ifc2_queue();
        mp.dialog = None;
        mp.calibration = CalibrationState::default();
    }
}

/// Calibration state machine (TS CalibrationDialog 100 ms tick):
/// 600 ms baseline → one new bit → stable 800 ms → accept; >1 new bit → retry.
fn run_calibration_machine(cal: &mut CalibrationState, _state: &AppState, now: u64) {
    let Some(bits) = cal.last_bits.take() else {
        // No packet this tick — check stability timers on stored candidates.
        check_stability(cal, now);
        return;
    };

    if cal.baseline.is_empty() && cal.detected.is_empty() && cal.candidate.is_none() {
        // Baseline phase: collect quiet-traffic bits for 600 ms.
        if cal.baseline_started.is_none() {
            cal.baseline_started = Some(now);
            cal.baseline = bits;
            cal.message = "Preparing baseline…".into();
            return;
        }
        let extras: Vec<u32> = bits
            .into_iter()
            .filter(|b| !cal.baseline.contains(b))
            .collect();
        cal.baseline.extend(extras);
        if now.saturating_sub(cal.baseline_started.unwrap()) >= 600 {
            cal.baseline_done = true;
            cal.message = "Press and release G1".into();
        }
        return;
    }

    if !cal.baseline_done {
        return;
    }

    // New (non-baseline) bits
    let new_bits: Vec<u32> = bits
        .iter()
        .copied()
        .filter(|b| !cal.baseline.contains(b) && !cal.detected.contains(b))
        .collect();

    if new_bits.is_empty() {
        check_stability(cal, now);
        return;
    }
    if new_bits.len() > 1 {
        cal.message = format!("Saw {} keys at once", new_bits.len());
        cal.failed_bits = new_bits;
        cal.candidate = None;
        cal.phase = CalPhase::Error;
        return;
    }
    // Single new bit — candidate; accept after stable 800 ms
    let bit = new_bits[0];
    match cal.candidate {
        Some((b, since)) if b == bit => {
            if now.saturating_sub(since) >= 800 {
                cal.detected.push(bit);
                cal.baseline.push(bit);
                cal.candidate = None;
                cal.stable_since = None;
                let next = cal.detected.len() + 1;
                if cal.detected.len() == GKEY_COUNT {
                    // Verify consecutive
                    let first = cal.detected[0];
                    let consecutive = cal
                        .detected
                        .iter()
                        .enumerate()
                        .all(|(i, b)| *b == first + i as u32);
                    if consecutive {
                        cal.bit0 = Some(first);
                        // Persist via macro manager
                        cal.phase = CalPhase::Done;
                        cal.message = "Done".into();
                        // (the dialog-level handler below sends SetGbit0)
                    } else {
                        cal.failed_bits = cal.detected.clone();
                        cal.message = "Bits are not consecutive".into();
                        cal.phase = CalPhase::Error;
                    }
                } else {
                    cal.message = format!("Captured G{}. Press G{next}", cal.detected.len());
                }
            } else {
                cal.message = "Hold…".into();
            }
        }
        _ => {
            cal.candidate = Some((bit, now));
            cal.message = "Captured — hold…".into();
        }
    }
}

fn check_stability(cal: &mut CalibrationState, now: u64) {
    if let Some((_, since)) = cal.candidate {
        if now.saturating_sub(since) > 1500 {
            cal.candidate = None;
            cal.message = "Waiting…".into();
        }
    } else if cal.baseline_done && cal.message.starts_with("Hold") {
        cal.message = "Waiting…".into();
    }
    let _ = cal.stable_since.take();
}

// ─── Shared: playback controls (PlaybackControls.tsx) ──────────────────

fn playback_controls(ui: &mut egui::Ui, action: &mut GKeyAction, speed: &mut f64) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Play mode").size(13.0));
        let current = ACTION_LABELS
            .iter()
            .find(|(a, _)| *a == *action)
            .map(|(_, l)| *l)
            .unwrap_or("?");
        egui::ComboBox::from_id_salt("play-mode")
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (a, label) in ACTION_LABELS {
                    ui.selectable_value(action, a, label);
                }
            });

        ui.add_space(8.0);
        ui.label(RichText::new("Speed").size(13.0));
        let speed_text = format!("{}×", speed_fmt(*speed));
        egui::ComboBox::from_id_salt("play-speed")
            .selected_text(speed_text)
            .show_ui(ui, |ui| {
                for s in SPEEDS {
                    ui.selectable_value(speed, s, format!("{s}×"));
                }
            });

        ui.add_space(8.0);
        widgets::caption(ui, "applies wherever this plays (G-keys, Test)");
    });
}

fn speed_fmt(s: f64) -> String {
    if s.fract() == 0.0 {
        format!("{}", s as i64)
    } else {
        format!("{s}")
    }
}
