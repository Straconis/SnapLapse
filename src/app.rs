//! The SnapLapse window.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, TextStyle, Vec2};

use crate::capture::{self, Method, WindowInfo};
use crate::encode::Format;
use crate::recorder::{Config, Recorder, State};
use crate::settings::{Settings, Unit, MIN_INTERVAL_MS};

const BRAND: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);
const BRAND_CYAN: Color32 = Color32::from_rgb(0x67, 0xE8, 0xF9);
const STOP_RED: Color32 = Color32::from_rgb(0xDC, 0x26, 0x26);
const WARN: Color32 = Color32::from_rgb(0xF5, 0x9E, 0x0B);

const ICON_PNG: &[u8] = include_bytes!("../branding/out/icons/snaplapse-256.png");
const LOGO_PNG: &[u8] = include_bytes!("../branding/out/icons/snaplapse-128.png");

pub fn run() -> Result<(), String> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("SnapLapse")
        .with_app_id("snaplapse")
        .with_inner_size([480.0, 430.0])
        .with_min_inner_size([420.0, 400.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON_PNG) {
        viewport = viewport.with_icon(Arc::new(icon));
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "SnapLapse",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
    .map_err(|e| e.to_string())
}

struct App {
    settings: Settings,
    windows: Vec<WindowInfo>,
    selected: Option<WindowInfo>,
    list_error: Option<String>,
    recorder: Option<Recorder>,
    logo: Option<egui::TextureHandle>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_style(&cc.egui_ctx);
        let mut app = App {
            settings: Settings::load(),
            windows: Vec::new(),
            selected: None,
            list_error: None,
            recorder: None,
            logo: load_logo(&cc.egui_ctx),
        };
        app.refresh_windows();
        if let Some(last) = &app.settings.last_window {
            app.selected = app.windows.iter().find(|w| &w.title == last).cloned();
        }
        app
    }

    fn refresh_windows(&mut self) {
        match capture::list_windows() {
            Ok(list) => {
                self.windows = list;
                self.list_error = None;
            }
            Err(e) => {
                self.windows.clear();
                self.list_error = Some(e);
            }
        }
        // Keep the selection if that window is still open, picking up any new title.
        if let Some(selected) = &self.selected {
            self.selected = self.windows.iter().find(|w| w.id == selected.id).cloned();
        }
    }

    fn is_recording(&self) -> bool {
        self.recorder.as_ref().is_some_and(|r| !r.is_done())
    }

    fn start(&mut self) {
        let Some(window) = self.selected.clone() else {
            return;
        };
        self.settings.last_window = Some(window.title.clone());
        self.settings.save();
        self.recorder = Some(Recorder::start(Config {
            window: window.id,
            title: window.title,
            folder: self.settings.folder.clone(),
            interval: self.settings.interval_duration(),
            format: self.settings.format,
            method: self.settings.method,
            limit: None,
        }));
    }

    fn header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(logo) = &self.logo {
                ui.add(egui::Image::new((logo.id(), Vec2::splat(40.0))));
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(RichText::new("Snap").size(24.0).strong());
                    ui.label(RichText::new("Lapse").size(24.0));
                });
                ui.label(RichText::new("Timed screenshots of any window").weak());
            });
        });
    }

    fn form(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("form").num_columns(2).spacing([14.0, 12.0]).show(ui, |ui| {
            ui.label("Window");
            ui.horizontal(|ui| {
                let chosen = self.selected.as_ref().map_or("Choose a window…".to_string(), |w| short(&w.title, 38));
                egui::ComboBox::from_id_salt("window")
                    .width(ui.available_width() - 80.0)
                    .selected_text(chosen)
                    .show_ui(ui, |ui| {
                        for window in &self.windows {
                            let is_selected = self.selected.as_ref().is_some_and(|s| s.id == window.id);
                            if ui.selectable_label(is_selected, short(&window.title, 60)).clicked() {
                                self.selected = Some(window.clone());
                            }
                        }
                        if self.windows.is_empty() {
                            ui.label(RichText::new("No windows found").weak());
                        }
                    });
                if ui.button("Refresh").on_hover_text("Reload the list of open windows").clicked() {
                    self.refresh_windows();
                }
            });
            ui.end_row();

            ui.label("Save to");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Open").on_hover_text("Show this folder").clicked() {
                    open_folder(&self.settings.folder);
                }
                if ui.button("Browse…").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().set_directory(&self.settings.folder).pick_folder() {
                        self.settings.folder = folder;
                    }
                }
                let full = self.settings.folder.display().to_string();
                ui.add(egui::Label::new(&full).truncate()).on_hover_text(&full);
            });
            ui.end_row();

            ui.label("Every");
            ui.horizontal(|ui| {
                let (min, speed) = match self.settings.unit {
                    Unit::Seconds => (MIN_INTERVAL_MS / 1000.0, 0.1),
                    Unit::Milliseconds => (MIN_INTERVAL_MS, 10.0),
                };
                ui.add(
                    egui::DragValue::new(&mut self.settings.interval)
                        .range(min..=86_400_000.0)
                        .speed(speed)
                        .max_decimals(2),
                );
                let mut unit = self.settings.unit;
                egui::ComboBox::from_id_salt("unit").selected_text(unit.label()).show_ui(ui, |ui| {
                    for u in [Unit::Seconds, Unit::Milliseconds] {
                        ui.selectable_value(&mut unit, u, u.label());
                    }
                });
                self.settings.set_unit(unit);
            });
            ui.end_row();

            ui.label("Format");
            egui::ComboBox::from_id_salt("format")
                .selected_text(self.settings.format.label())
                .show_ui(ui, |ui| {
                    for f in Format::ALL {
                        ui.selectable_value(&mut self.settings.format, f, f.label());
                    }
                });
            ui.end_row();

            if capture::HAS_METHOD_CHOICE {
                ui.label("Capture");
                egui::ComboBox::from_id_salt("method")
                    .selected_text(self.settings.method.label())
                    .show_ui(ui, |ui| {
                        for m in Method::ALL {
                            ui.selectable_value(&mut self.settings.method, m, m.label());
                        }
                    })
                    .response
                    .on_hover_text(
                        "Automatic works for almost everything. Try PrintWindow if Windows 10's yellow border bothers you.",
                    );
                ui.end_row();
            }
        });

        if self.settings.format == Format::Png
            && self.settings.interval_duration() < Duration::from_millis(150)
        {
            ui.add_space(6.0);
            ui.label(
                RichText::new("Tip: at very short intervals, JPG saves faster than PNG.")
                    .small()
                    .weak(),
            );
        }
    }

    fn start_stop(&mut self, ui: &mut egui::Ui) {
        let recording = self.is_recording();
        let stopping = recording && self.recorder.as_ref().is_some_and(|r| r.stop_requested());
        let (text, fill) = match (recording, stopping) {
            (true, true) => ("Finishing…", STOP_RED.gamma_multiply(0.6)),
            (true, false) => ("Stop", STOP_RED),
            (false, _) => ("Start", BRAND),
        };
        let enabled = !stopping && (recording || self.selected.is_some());
        let button = egui::Button::new(
            RichText::new(text)
                .size(17.0)
                .strong()
                .color(Color32::WHITE),
        )
        .fill(fill)
        .min_size(Vec2::new(ui.available_width(), 44.0));
        let response = ui.add_enabled(enabled, button);
        if response.clicked() {
            if recording {
                if let Some(r) = &self.recorder {
                    r.request_stop();
                }
            } else {
                self.start();
            }
        }
        if !enabled && !recording && self.selected.is_none() {
            response.on_disabled_hover_text("Choose a window first");
        }
    }

    fn status(&self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let Some(recorder) = &self.recorder else {
                ui.label(RichText::new(self.idle_message()).weak());
                return;
            };
            let s = recorder.stats().snapshot();
            let (headline, color) = match &s.state {
                State::Starting => ("Starting…".to_string(), None),
                State::Running => ("Recording".to_string(), Some(BRAND_CYAN)),
                State::Minimized => (
                    "Paused: the window is minimized. Restore it to continue.".to_string(),
                    Some(WARN),
                ),
                State::Stopped => ("Stopped".to_string(), None),
                State::Ended(reason) => (format!("Stopped: {reason}"), Some(STOP_RED)),
            };
            let mut headline = RichText::new(headline).strong();
            if let Some(color) = color {
                headline = headline.color(color);
            }
            ui.label(headline);
            ui.label(format!("{} saved", s.saved));
            if let Some(file) = &s.last_file {
                ui.add(egui::Label::new(RichText::new(format!("Last: {file}")).weak()).truncate());
            }
            if s.skipped > 0 {
                ui.label(
                    RichText::new(format!(
                        "{} skipped because saving couldn't keep up",
                        s.skipped
                    ))
                    .color(WARN),
                );
            }
            if let Some(error) = s.last_error.as_ref().filter(|_| s.failed > 0) {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{} failed: {error}", s.failed)).color(WARN),
                    )
                    .wrap(),
                );
            }
        });
    }

    fn idle_message(&self) -> String {
        if let Some(e) = &self.list_error {
            return e.clone();
        }
        if let Some(note) = capture::platform_note() {
            return note.to_string();
        }
        if self.selected.is_none() {
            return "Choose a window, then press Start.".into();
        }
        "Ready.".into()
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let recording = self.is_recording();
        if recording {
            // Refresh the counters a few times a second. When idle, nothing redraws at all.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        let panel = egui::Frame::central_panel(&ctx.style()).inner_margin(20.0);
        egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
            self.header(ui);
            ui.add_space(16.0);
            ui.add_enabled_ui(!recording, |ui| self.form(ui));
            ui.add_space(18.0);
            self.start_stop(ui);
            ui.add_space(12.0);
            self.status(ui);
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(mut recorder) = self.recorder.take() {
            recorder.stop();
        }
        self.settings.save();
    }
}

fn apply_style(ctx: &egui::Context) {
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            style.visuals.selection.bg_fill = BRAND;
            style.visuals.hyperlink_color = BRAND;
            style.spacing.button_padding = Vec2::new(10.0, 5.0);
            style.spacing.interact_size.y = 28.0;
            style
                .text_styles
                .insert(TextStyle::Body, FontId::proportional(14.5));
            style
                .text_styles
                .insert(TextStyle::Button, FontId::proportional(14.5));
        });
    }
}

fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let image = image::load_from_memory(LOGO_PNG).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let pixels = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    Some(ctx.load_texture("logo", pixels, egui::TextureOptions::LINEAR))
}

/// Shortens long titles with an ellipsis so the dropdown stays a sensible width.
fn short(title: &str, max_chars: usize) -> String {
    if title.chars().count() <= max_chars {
        return title.to_string();
    }
    let mut cut: String = title.chars().take(max_chars - 1).collect();
    cut.push('…');
    cut
}

fn open_folder(folder: &Path) {
    let _ = std::fs::create_dir_all(folder);
    #[cfg(windows)]
    let opener = "explorer";
    #[cfg(not(windows))]
    let opener = "xdg-open";
    let _ = std::process::Command::new(opener).arg(folder).spawn();
}
