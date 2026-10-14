//! The SnapLapse window.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, TextStyle, Vec2};

use crate::capture::{self, Method, WindowInfo};
use crate::encode::Format;
use crate::pack::ZipSplit;
use crate::recorder::{Config, Output, Recorder, SharedRecorder, State};
use crate::settings::{Settings, Unit, MIN_INTERVAL_MS};

const BRAND: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);
const BRAND_CYAN: Color32 = Color32::from_rgb(0x67, 0xE8, 0xF9);
const STOP_RED: Color32 = Color32::from_rgb(0xDC, 0x26, 0x26);
const WARN: Color32 = Color32::from_rgb(0xF5, 0x9E, 0x0B);

const ICON_PNG: &[u8] = include_bytes!("../branding/out/icons/snaplapse-256.png");
const LOGO_PNG: &[u8] = include_bytes!("../branding/out/icons/snaplapse-128.png");
#[cfg(windows)]
const TRAY_PNG: &[u8] = include_bytes!("../branding/out/icons/snaplapse-32.png");

pub fn run() -> Result<(), String> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("SnapLapse")
        .with_app_id("snaplapse")
        .with_inner_size([480.0, 680.0])
        .with_min_inner_size([420.0, 360.0]);
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
    /// The settings as last written to disk, to save again only when something changes.
    saved_settings: Settings,
    windows: Vec<WindowInfo>,
    selected: Option<WindowInfo>,
    list_error: Option<String>,
    recorder: SharedRecorder,
    logo: Option<egui::TextureHandle>,
    /// Whether the tray icon exists, so closing to the tray can't hide the window for good.
    has_tray: bool,
    /// Mirrors the "Auto-hide at screen edge" toggle for the docking thread (Windows only).
    docking: Option<Arc<AtomicBool>>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_style(&cc.egui_ctx);
        let settings = Settings::load();
        let recorder = SharedRecorder::default();
        let has_tray = install_tray(cc, &recorder);
        let docking = start_docking(cc, settings.edge_autohide);
        let mut app = App {
            saved_settings: settings.clone(),
            settings,
            windows: Vec::new(),
            selected: None,
            list_error: None,
            recorder,
            logo: load_logo(&cc.egui_ctx),
            has_tray,
            docking,
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

    fn with_recorder<R>(&self, f: impl FnOnce(&Recorder) -> R) -> Option<R> {
        self.recorder.lock().unwrap().as_ref().map(f)
    }

    fn is_recording(&self) -> bool {
        self.with_recorder(|r| !r.is_done()).unwrap_or(false)
    }

    /// Saves settings as soon as they change, so nothing is lost if SnapLapse is quit from the tray.
    fn save_settings_if_changed(&mut self) {
        if self.settings != self.saved_settings {
            self.settings.save();
            self.saved_settings = self.settings.clone();
        }
    }

    fn start(&mut self) {
        let Some(window) = self.selected.clone() else {
            return;
        };
        self.settings.last_window = Some(window.title.clone());
        self.save_settings_if_changed();
        *self.recorder.lock().unwrap() = Some(Recorder::start(Config {
            window: window.id,
            title: window.title,
            folder: self.settings.folder.clone(),
            interval: self.settings.interval_duration(),
            format: self.settings.format,
            method: self.settings.method,
            output: self.settings.output,
            zip_split: self.settings.zip_split,
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

            ui.label("Keep as");
            egui::ComboBox::from_id_salt("output")
                .selected_text(self.settings.output.label())
                .show_ui(ui, |ui| {
                    for o in Output::ALL {
                        ui.selectable_value(&mut self.settings.output, o, o.label());
                    }
                })
                .response
                .on_hover_text(
                    "Each recording gets its own folder, named with its start time and window. \
                     ZIP packs that folder into a single file when you press Stop.",
                );
            ui.end_row();

            if self.settings.output == Output::Zip {
                ui.label("Split ZIP");
                egui::ComboBox::from_id_salt("zip_split")
                    .selected_text(self.settings.zip_split.label())
                    .show_ui(ui, |ui| {
                        for z in ZipSplit::ALL {
                            ui.selectable_value(&mut self.settings.zip_split, z, z.label());
                        }
                    })
                    .response
                    .on_hover_text(
                        "Big recordings are split into several ZIP files (part 1, part 2…). \
                         Each one opens on its own, and no picture is cut in half.",
                    );
                ui.end_row();
            }

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

            if self.has_tray {
                ui.label("Closing");
                ui.horizontal(|ui| {
                    let hint = "When on, the X button hides SnapLapse to the icon by the clock \
                                and recording continues. Right-click that icon to quit.";
                    toggle(ui, &mut self.settings.close_to_tray).on_hover_text(hint);
                    let label = ui.add(egui::Label::new("Close to tray").sense(egui::Sense::click()));
                    if label.on_hover_text(hint).clicked() {
                        self.settings.close_to_tray = !self.settings.close_to_tray;
                    }
                });
                ui.end_row();
            }

            if self.docking.is_some() {
                ui.label("Screen edge");
                ui.horizontal(|ui| {
                    let hint = "When on, drag SnapLapse against an edge of the screen to dock it. \
                                It hides when the mouse leaves and leaves a small red marker on \
                                the edge; point at the marker to bring it back.";
                    toggle(ui, &mut self.settings.edge_autohide).on_hover_text(hint);
                    let label =
                        ui.add(egui::Label::new("Auto-hide when docked").sense(egui::Sense::click()));
                    if label.on_hover_text(hint).clicked() {
                        self.settings.edge_autohide = !self.settings.edge_autohide;
                    }
                });
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
        let stopping = recording && self.with_recorder(|r| r.stop_requested()).unwrap_or(false);
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
                self.with_recorder(|r| r.request_stop());
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
            let Some(s) = self.with_recorder(|r| r.stats().snapshot()) else {
                ui.label(RichText::new(self.idle_message()).weak());
                return;
            };
            let (headline, color) = match &s.state {
                State::Starting => ("Starting…".to_string(), None),
                State::Running => ("Recording".to_string(), Some(BRAND_CYAN)),
                State::Minimized => (
                    "Paused: the window is minimized. Restore it to continue.".to_string(),
                    Some(WARN),
                ),
                State::Packing => ("Packing into a ZIP file…".to_string(), Some(BRAND_CYAN)),
                State::Stopped => ("Stopped".to_string(), None),
                State::Ended(reason) => (format!("Stopped: {reason}"), Some(STOP_RED)),
            };
            let mut headline = RichText::new(headline).strong();
            if let Some(color) = color {
                headline = headline.color(color);
            }
            ui.label(headline);
            ui.label(format!("{} saved", s.saved));
            if let Some(destination) = &s.destination {
                ui.add(
                    egui::Label::new(RichText::new(format!("In: {destination}")).weak()).truncate(),
                );
            }
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
        if self.has_tray
            && self.settings.close_to_tray
            && ctx.input(|i| i.viewport().close_requested())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        self.save_settings_if_changed();
        if let Some(docking) = &self.docking {
            docking.store(self.settings.edge_autohide, Ordering::Relaxed);
        }
        let recording = self.is_recording();
        if recording {
            // Refresh the counters a few times a second. When idle, nothing redraws at all.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        let panel = egui::Frame::central_panel(&ctx.style()).inner_margin(20.0);
        egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
            // Scrolls only if the window is made smaller than everything needs.
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    self.header(ui);
                    ui.add_space(16.0);
                    ui.add_enabled_ui(!recording, |ui| self.form(ui));
                    ui.add_space(18.0);
                    self.start_stop(ui);
                    ui.add_space(12.0);
                    self.status(ui);
                });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let recorder = self.recorder.lock().unwrap().take();
        if let Some(mut recorder) = recorder {
            recorder.stop();
        }
        self.settings.save();
    }
}

/// SnapLapse's own window handle on Windows, for the tray icon and edge docking.
#[cfg(windows)]
fn main_window(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(win32) => Some(win32.hwnd.get()),
        _ => None,
    }
}

/// Adds the SnapLapse icon to the Windows notification area. Returns whether it worked.
#[cfg(windows)]
fn install_tray(cc: &eframe::CreationContext<'_>, recorder: &SharedRecorder) -> bool {
    let Some(hwnd) = main_window(cc) else {
        return false;
    };
    let Ok(icon) = image::load_from_memory(TRAY_PNG) else {
        return false;
    };
    let icon = icon.to_rgba8();
    let (width, height) = icon.dimensions();
    crate::tray::install(hwnd, recorder.clone(), icon.into_raw(), width, height).is_ok()
}

#[cfg(not(windows))]
fn install_tray(_cc: &eframe::CreationContext<'_>, _recorder: &SharedRecorder) -> bool {
    false
}

/// Starts edge docking on Windows. Returns the switch that mirrors the app's toggle.
#[cfg(windows)]
fn start_docking(cc: &eframe::CreationContext<'_>, on: bool) -> Option<Arc<AtomicBool>> {
    let hwnd = main_window(cc)?;
    let enabled = Arc::new(AtomicBool::new(on));
    crate::dock_windows::start(hwnd, enabled.clone());
    Some(enabled)
}

#[cfg(not(windows))]
fn start_docking(_cc: &eframe::CreationContext<'_>, _on: bool) -> Option<Arc<AtomicBool>> {
    None
}

/// An on/off switch, which reads more clearly than a checkbox for settings like this.
/// Adapted from egui's own toggle-switch example.
fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = Vec2::new(40.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, "")
    });
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let fill = if *on { BRAND } else { visuals.bg_fill };
        let radius = 0.5 * rect.height();
        ui.painter().rect(
            rect,
            radius,
            fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let knob = egui::pos2(x, rect.center().y);
        ui.painter()
            .circle(knob, 0.75 * radius, Color32::WHITE, egui::Stroke::NONE);
    }
    response
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
