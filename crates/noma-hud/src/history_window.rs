//! A plain window listing what has been dictated, opened from the tray.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, RichText, ViewportBuilder, ViewportId};
use noma_config::{now_secs, History};

/// Draw the history window as a second viewport.
///
/// Deferred rather than immediate so it keeps painting while the HUD is idle.
pub fn show(ctx: &egui::Context, history: &Arc<Mutex<History>>, open: &Arc<AtomicBool>) {
    let history = Arc::clone(history);
    let open = Arc::clone(open);

    ctx.show_viewport_deferred(
        ViewportId::from_hash_of("noma-history"),
        ViewportBuilder::default()
            .with_title("Noma - History")
            .with_inner_size([560.0, 460.0])
            .with_min_inner_size([360.0, 240.0]),
        move |ctx, _class| {
            // The main window is transparent, and clear_color is app-wide, so
            // this window has to paint its own background.
            let frame = egui::Frame::default()
                .fill(Color32::from_rgb(17, 21, 30))
                .inner_margin(14.0);

            egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                let mut clear = false;
                {
                    let history = history.lock().expect("history");
                    ui.horizontal(|ui| {
                        ui.heading("Recent dictations");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(!history.is_empty(), egui::Button::new("Clear"))
                                .clicked()
                            {
                                clear = true;
                            }
                        });
                    });
                    ui.add_space(6.0);

                    if history.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "Nothing yet. Hold {} and say something.",
                                noma_hotkey::key_label()
                            ))
                            .color(Color32::from_rgb(148, 163, 184)),
                        );
                    } else {
                        let now = now_secs();
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for entry in history.entries() {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(entry.age(now))
                                            .small()
                                            .color(Color32::from_rgb(129, 140, 160)),
                                    );
                                    ui.label(
                                        RichText::new(format!("{:.1}s", entry.seconds))
                                            .small()
                                            .color(Color32::from_rgb(129, 140, 160)),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("Copy").clicked() {
                                                ui.ctx().copy_text(entry.text.clone());
                                            }
                                        },
                                    );
                                });
                                ui.label(&entry.text);
                                if entry.was_edited() {
                                    // Show what the model actually heard, so a
                                    // bad cleanup rule is visible rather than
                                    // looking like a bad transcription.
                                    ui.label(
                                        RichText::new(format!("heard: {}", entry.raw))
                                            .small()
                                            .italics()
                                            .color(Color32::from_rgb(110, 120, 140)),
                                    );
                                }
                                ui.separator();
                            }
                        });
                    }
                }

                if clear {
                    if let Err(err) = history.lock().expect("history").clear() {
                        eprintln!("noma: could not clear history: {err:#}");
                    }
                }
            });

            if ctx.input(|input| input.viewport().close_requested()) {
                open.store(false, Ordering::SeqCst);
            }
        },
    );
}
