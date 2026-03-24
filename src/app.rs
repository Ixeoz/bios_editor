//! All the GUI glue. Parsing / SCEWIN / presets / validation are split out so this file
//! doesn't grow forever. Process starts in `main.rs`.

use crate::change_format;
use crate::nvram::{load_nvram, option_pretty, save_nvram, BiosSetting, LoadedNvram};
use crate::presets;
use crate::scewin;
use crate::validation;
use eframe::egui;
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use std::sync::Arc;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[cfg(windows)]
fn win32_maximize_window(frame: &eframe::Frame) {
    let Ok(handle) = frame.window_handle() else {
        return;
    };
    let raw = handle.as_raw();
    let RawWindowHandle::Win32(h) = raw else {
        return;
    };
    let hwnd = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(
            hwnd,
            windows_sys::Win32::UI::WindowsAndMessaging::SW_MAXIMIZE,
        );
    }
}

fn apply_startup_maximize(ctx: &egui::Context, frame: &mut eframe::Frame, attempts_left: &mut u8) {
    if *attempts_left == 0 {
        return;
    }
    *attempts_left -= 1;
    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
    #[cfg(windows)]
    win32_maximize_window(frame);
    #[cfg(not(windows))]
    let _ = frame;
}

fn load_window_icon() -> Option<Arc<egui::IconData>> {
    let decoded = image::load_from_memory_with_format(
        include_bytes!("icon/nvram.ico"),
        image::ImageFormat::Ico,
    )
    .ok()?;
    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();
    Some(Arc::new(egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    }))
}

enum PendingTask {
    Export,
    Import,
}

struct PendingTaskResult {
    task: PendingTask,
    work: PathBuf,
    result: Result<std::process::ExitStatus, String>,
}

#[derive(Clone)]
struct ChangeLogEntry {
    seq: u32,
    setting: String,
    summary: String,
    token: String,
    offset: String,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum AppTab {
    Editor,
    History,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum ListFilterKind {
    All,
    Options,
    Values,
}

pub fn run() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_app_id("nvram_editor_scewin")
        .with_inner_size([1480.0, 900.0])
        .with_maximized(true)
        .with_title("NVRAM Editor — SCEWIN / AMI");
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let native_options = eframe::NativeOptions {
        // Still loads old `window` entry from app.ron unless app_id changes — that entry forced non-maximized size.
        persist_window: false,
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "NVRAM Editor",
        native_options,
        Box::new(|cc| Ok(Box::new(BiosEditorApp::new(cc)))),
    )
}

struct BiosEditorApp {
    data: Option<LoadedNvram>,
    original_settings: Vec<BiosSetting>,
    active_tab: AppTab,
    search: String,
    search_id: egui::Id,
    search_focus_request: bool,
    history_search: String,
    history_search_id: egui::Id,
    selected: Option<usize>,
    status: Option<(bool, String)>,
    value_edit_buffer: String,
    value_outer_prefix: String,
    value_outer_suffix: String,
    /// What the text field held when you landed on this setting (used for history diff).
    value_edit_baseline: String,
    dirty: bool,
    show_only_changed: bool,
    list_filter_kind: ListFilterKind,
    scewin_work: Option<PathBuf>,
    autostart_done: bool,
    change_log: Vec<ChangeLogEntry>,
    change_seq: u32,
    pending_task_rx: Option<Receiver<PendingTaskResult>>,
    import_preview_open: bool,
    import_preview_filter: String,
    preset_input: String,
    /// Popup after export/import: (when to hide, success?, text).
    center_toast: Option<(f64, bool, String)>,
    /// eframe/winit can restore a non-maximized size from disk; re-apply maximize for a few frames (Win32 ShowWindow too).
    startup_maximize_attempts_left: u8,
}

impl BiosEditorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = egui::Color32::from_rgb(15, 15, 15);
        visuals.panel_fill = egui::Color32::from_rgb(12, 12, 12);
        visuals.faint_bg_color = egui::Color32::from_rgb(20, 20, 20);
        visuals.extreme_bg_color = egui::Color32::from_rgb(8, 8, 8);
        let accent = egui::Color32::from_rgb(20, 173, 255);
        visuals.selection.bg_fill = egui::Color32::from_rgb(10, 50, 90);
        visuals.selection.stroke.color = accent;
        visuals.hyperlink_color = accent;
        visuals.text_cursor.stroke.color = accent;
        cc.egui_ctx.set_visuals(visuals);
        cc.egui_ctx.style_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 8.0);
            style.spacing.button_padding = egui::vec2(10.0, 6.0);
            style.spacing.indent = 16.0;
            style.spacing.scroll = egui::style::ScrollStyle::floating();
            style.spacing.scroll.bar_width = 9.0;
            style.spacing.scroll.handle_min_length = 36.0;
            style.spacing.scroll.bar_inner_margin = 0.0;
            style.spacing.scroll.bar_outer_margin = 0.0;
            style.spacing.scroll.foreground_color = true;
            style.spacing.scroll.floating_width = 7.0;
            style.spacing.scroll.floating_allocated_width = 0.0;
        });

        Self {
            data: None,
            original_settings: Vec::new(),
            active_tab: AppTab::Editor,
            search: String::new(),
            search_id: egui::Id::new("nvram_editor_search"),
            search_focus_request: false,
            history_search: String::new(),
            history_search_id: egui::Id::new("nvram_editor_history_search"),
            selected: None,
            status: None,
            value_edit_buffer: String::new(),
            value_outer_prefix: String::new(),
            value_outer_suffix: String::new(),
            value_edit_baseline: String::new(),
            dirty: false,
            show_only_changed: false,
            list_filter_kind: ListFilterKind::All,
            scewin_work: scewin::resolve_scewin_work_dir(),
            autostart_done: false,
            change_log: Vec::new(),
            change_seq: 0,
            pending_task_rx: None,
            import_preview_open: false,
            import_preview_filter: String::new(),
            preset_input: String::new(),
            center_toast: None,
            startup_maximize_attempts_left: 3,
        }
    }

    fn scewin_nvram_path(&self) -> Option<PathBuf> {
        self.scewin_work
            .as_ref()
            .map(|w| scewin::nvram_path_in_work(w))
    }

    fn is_loaded_from_scewin_nvram(&self) -> bool {
        let Some(data) = self.data.as_ref() else {
            return false;
        };
        let Some(ref want) = self.scewin_nvram_path() else {
            return false;
        };
        data.path
            .as_ref()
            .map(|p| p == want)
            .unwrap_or(false)
    }

    fn is_busy(&self) -> bool {
        self.pending_task_rx.is_some()
    }

    fn push_change_log(
        &mut self,
        setting: &str,
        token: &str,
        offset: &str,
        summary: String,
    ) {
        self.change_seq = self.change_seq.wrapping_add(1);
        self.change_log.insert(
            0,
            ChangeLogEntry {
                seq: self.change_seq,
                setting: setting.trim().to_string(),
                summary,
                token: token.trim().to_string(),
                offset: offset.trim().to_string(),
            },
        );
        const MAX: usize = 400;
        if self.change_log.len() > MAX {
            self.change_log.truncate(MAX);
        }
    }

    fn setting_matches_search(s: &BiosSetting, query: &str) -> bool {
        let q = query.trim();
        if q.is_empty() {
            return true;
        }
        let blob = format!(
            "{} {} {} {}",
            s.setup_question, s.help_string, s.token, s.display_current()
        )
        .to_lowercase();
        q.split_whitespace()
            .all(|w| !w.is_empty() && blob.contains(&w.to_lowercase()))
    }

    fn is_setting_changed_at(&self, idx: usize) -> bool {
        if let (Some(data), Some(original)) = (self.data.as_ref(), self.original_settings.get(idx)) {
            if let Some(current) = data.settings.get(idx) {
                return validation::setting_value_changed(current, original);
            }
        }
        false
    }

    fn filtered_indices(&self, data: &LoadedNvram, search: &str) -> Vec<usize> {
        data.settings
            .iter()
            .enumerate()
            .filter(|(i, s)| {
                let kind_ok = match self.list_filter_kind {
                    ListFilterKind::All => true,
                    ListFilterKind::Options => !s.options.is_empty(),
                    ListFilterKind::Values => s.options.is_empty(),
                };
                let changed_ok = !self.show_only_changed || self.is_setting_changed_at(*i);
                let search_ok =
                    search.trim().is_empty() || Self::setting_matches_search(s, search);
                kind_ok && changed_ok && search_ok
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn filter_stats(&self, data: &LoadedNvram, search: &str) -> (usize, usize) {
        let n = self.filtered_indices(data, search).len();
        (n, data.settings.len())
    }

    fn changed_count(&self) -> usize {
        let Some(data) = self.data.as_ref() else { return 0 };
        data.settings
            .iter()
            .enumerate()
            .filter(|(i, _)| self.is_setting_changed_at(*i))
            .count()
    }

    fn collect_changed_rows(&self) -> Vec<(usize, String, String, String)> {
        let mut rows = Vec::new();
        let Some(data) = self.data.as_ref() else {
            return rows;
        };
        for (i, current) in data.settings.iter().enumerate() {
            let Some(original) = self.original_settings.get(i) else {
                continue;
            };
            if !validation::setting_value_changed(current, original) {
                continue;
            }
            rows.push((
                i,
                current.setup_question.trim().to_string(),
                original.display_current(),
                current.display_current(),
            ));
        }
        rows
    }

    fn apply_presets_from_input(&mut self) {
        let parsed = presets::parse_preset_input_lines(&self.preset_input);
        if parsed.is_empty() {
            self.status = Some((
                false,
                "No valid preset lines. Use: Setting [Option] or Setting = Option".into(),
            ));
            return;
        }

        let Some(data) = self.data.as_mut() else {
            self.status = Some((false, "Load nvram.txt before applying presets.".into()));
            return;
        };

        let outcome = presets::apply_presets_to_settings(&mut data.settings, &parsed);
        for (name, token, offset, summary) in outcome.logs {
            self.push_change_log(&name, &token, &offset, summary);
        }
        if outcome.applied > 0 {
            self.dirty = true;
            self.sync_value_buffer();
        }

        let ok = outcome.errors.is_empty();
        let mut msg = format!(
            "Presets applied: {}, unchanged: {}.",
            outcome.applied, outcome.unchanged
        );
        if outcome.applied == 0 && outcome.unchanged > 0 {
            msg.push_str(" All matching settings were already at that option.");
        }
        if !outcome.errors.is_empty() {
            let show = outcome
                .errors
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ");
            msg.push_str(&format!(" Errors: {show}"));
        }
        self.status = Some((ok, msg));
    }

    fn find_setting_index(&self, token: &str, offset: &str, setting: &str) -> Option<usize> {
        let data = self.data.as_ref()?;
        if !token.is_empty() && !offset.is_empty() {
            if let Some((i, _)) = data.settings.iter().enumerate().find(|(_, s)| {
                s.token.trim() == token.trim() && s.offset.trim() == offset.trim()
            }) {
                return Some(i);
            }
        }
        data.settings
            .iter()
            .enumerate()
            .find(|(_, s)| s.setup_question.trim() == setting.trim())
            .map(|(i, _)| i)
    }

    fn open_file(&mut self, path: PathBuf) {
        self.status = None;
        match load_nvram(&path) {
            Ok(loaded) => {
                self.selected = if loaded.settings.is_empty() {
                    None
                } else {
                    Some(0)
                };
                self.original_settings = loaded.settings.clone();
                self.data = Some(loaded);
                self.change_log.clear();
                self.change_seq = 0;
                self.sync_value_buffer();
                self.dirty = false;
                self.status = Some((true, format!("Loaded: {}", path.display())));
            }
            Err(e) => {
                self.status = Some((false, format!("Failed to open: {e}")));
            }
        }
    }

    fn sync_value_buffer(&mut self) {
        let Some(data) = self.data.as_ref() else {
            self.value_edit_buffer.clear();
            self.value_outer_prefix.clear();
            self.value_outer_suffix.clear();
            return;
        };
        let Some(i) = self.selected else {
            self.value_edit_buffer.clear();
            self.value_outer_prefix.clear();
            self.value_outer_suffix.clear();
            return;
        };
        if let Some(s) = data.settings.get(i) {
            if s.options.is_empty() {
                let raw = s.value.clone().unwrap_or_default();
                if let Some(inner) = validation::extract_angle_value(&raw) {
                    self.value_outer_prefix = "<".to_string();
                    self.value_outer_suffix = ">".to_string();
                    self.value_edit_buffer = inner.to_string();
                } else {
                    self.value_outer_prefix.clear();
                    self.value_outer_suffix.clear();
                    self.value_edit_buffer = raw;
                }
            } else {
                self.value_edit_buffer.clear();
                self.value_outer_prefix.clear();
                self.value_outer_suffix.clear();
            }
        }
        self.value_edit_baseline = self.value_edit_buffer.clone();
    }

    fn save_as(&mut self) {
        let Some(data_ro) = self.data.as_ref() else {
            self.status = Some((false, "No file loaded.".to_string()));
            return;
        };
        if let Err(msg) = validation::validate_all_settings(&data_ro.settings, &self.original_settings) {
            self.status = Some((false, msg));
            return;
        }

        let Some(ref mut data) = self.data else {
            self.status = Some((false, "No file loaded.".to_string()));
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("nvram / text", &["txt"])
            .set_file_name("nvram.txt")
            .save_file()
        else {
            return;
        };
        match save_nvram(&path, &data.original_lines, &data.settings) {
            Ok(()) => {
                data.path = Some(path.clone());
                self.dirty = false;
                self.status = Some((true, format!("Saved: {}", path.display())));
            }
            Err(e) => {
                self.status = Some((false, format!("Failed to save: {e}")));
            }
        }
    }

    fn save_to_scewin_folder(&mut self) -> bool {
        let Some(data_ro) = self.data.as_ref() else {
            self.status = Some((false, "Nothing to save.".into()));
            return false;
        };
        if let Err(msg) = validation::validate_all_settings(&data_ro.settings, &self.original_settings) {
            self.status = Some((false, msg));
            return false;
        }

        let Some(work) = self.scewin_work.clone() else {
            self.status = Some((false, "SCEWIN folder not found next to the application.".into()));
            return false;
        };
        let Some(ref mut data) = self.data else {
            self.status = Some((false, "Nothing to save.".into()));
            return false;
        };
        let path = scewin::nvram_path_in_work(&work);
        match save_nvram(&path, &data.original_lines, &data.settings) {
            Ok(()) => {
                data.path = Some(path.clone());
                self.dirty = false;
                self.status = Some((true, format!("Saved to {}", path.display())));
                true
            }
            Err(e) => {
                self.status = Some((false, format!("Failed to save to SCEWIN folder: {e}")));
                false
            }
        }
    }

    fn run_export(&mut self) {
        if self.is_busy() {
            return;
        }
        let Some(work) = self.scewin_work.clone() else {
            self.status = Some((
                false,
                "SCEWIN not available: add the SCEWIN folder next to nvram_editor.exe, or use a build compiled with SCEWIN/ embedded.".into(),
            ));
            return;
        };
        self.status = Some((true, "Export in progress... approve UAC if prompted.".into()));
        let (tx, rx) = mpsc::channel::<PendingTaskResult>();
        self.pending_task_rx = Some(rx);
        std::thread::spawn(move || {
            let result = scewin::export_nvram(&work).map_err(|e| e.to_string());
            let _ = tx.send(PendingTaskResult {
                task: PendingTask::Export,
                work,
                result,
            });
        });
    }

    fn run_import(&mut self) {
        if self.is_busy() {
            return;
        }
        let Some(work) = self.scewin_work.clone() else {
            self.status = Some((false, "SCEWIN folder not found next to the application.".into()));
            return;
        };
        if self.data.is_none() {
            self.status = Some((
                false,
                "Open or create nvram.txt before Import (or run Export first).".into(),
            ));
            return;
        }
        if !self.save_to_scewin_folder() {
            return;
        }
        self.status = Some((true, "Import in progress... approve UAC if prompted.".into()));
        let (tx, rx) = mpsc::channel::<PendingTaskResult>();
        self.pending_task_rx = Some(rx);
        std::thread::spawn(move || {
            let result = scewin::import_nvram(&work).map_err(|e| e.to_string());
            let _ = tx.send(PendingTaskResult {
                task: PendingTask::Import,
                work,
                result,
            });
        });
    }

    fn open_bundled_nvram(&mut self) {
        let Some(work) = self.scewin_work.clone() else {
            self.status = Some((false, "SCEWIN not found.".into()));
            return;
        };
        let p = scewin::nvram_path_in_work(&work);
        if p.is_file() {
            self.open_file(p);
        } else {
            self.status = Some((
                false,
                "nvram.txt is not in the SCEWIN folder yet. Run Export first.".into(),
            ));
        }
    }

}

impl eframe::App for BiosEditorApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        apply_startup_maximize(
            ctx,
            frame,
            &mut self.startup_maximize_attempts_left,
        );

        let now = ctx.input(|i| i.time);
        if let Some((until, _, _)) = self.center_toast {
            if now >= until {
                self.center_toast = None;
            }
        }

        const TOAST_SECS: f64 = 5.0;
        if let Some(rx) = self.pending_task_rx.take() {
            match rx.try_recv() {
                Ok(done) => match done.task {
                    PendingTask::Export => match done.result {
                        Ok(status) => {
                            let p = scewin::nvram_path_in_work(&done.work);
                            if p.is_file() {
                                self.open_file(p);
                            }
                            let ok = status.success();
                            let msg = if ok {
                                "Export finished. BIOS settings saved to nvram.txt in the SCEWIN folder.".into()
                            } else {
                                format!(
                                    "Export failed (code {}). Approve UAC if nothing changed.",
                                    status.code().unwrap_or(-1)
                                )
                            };
                            self.status = Some((ok, msg.clone()));
                            self.center_toast = Some((now + TOAST_SECS, ok, msg));
                        }
                        Err(err) => {
                            let msg = format!("Failed to start SCEWIN: {err}");
                            self.status = Some((false, msg.clone()));
                            self.center_toast = Some((now + TOAST_SECS, false, msg));
                        }
                    },
                    PendingTask::Import => match done.result {
                        Ok(status) => {
                            let ok = status.success();
                            let msg = if ok {
                                "Settings imported successfully.\nCheck BIOS / reboot if required."
                                    .into()
                            } else {
                                let code = status.code().unwrap_or(-1);
                                if code == 13 {
                                    "Import failed (code 13): access/driver issue.\nCheck amifldrv64.sys + amigendrv64.sys and Windows security."
                                        .into()
                                } else {
                                    format!(
                                        "Import failed (code {code}).\nSee log-file.txt in the SCEWIN folder."
                                    )
                                }
                            };
                            self.status = Some((ok, msg.replace('\n', " ")));
                            self.center_toast = Some((now + TOAST_SECS, ok, msg));
                        }
                        Err(err) => {
                            let msg = format!("Failed to start Import: {err}");
                            self.status = Some((false, msg.clone()));
                            self.center_toast = Some((now + TOAST_SECS, false, msg));
                        }
                    },
                },
                Err(TryRecvError::Empty) => {
                    self.pending_task_rx = Some(rx);
                    ctx.request_repaint_after(std::time::Duration::from_millis(120));
                }
                Err(TryRecvError::Disconnected) => {
                    let msg = "Background task failed unexpectedly.".to_string();
                    self.status = Some((false, msg.clone()));
                    self.center_toast = Some((now + TOAST_SECS, false, msg));
                }
            }
        }

        if !self.autostart_done {
            self.autostart_done = true;
            if self.data.is_none() {
                if let Some(ref w) = self.scewin_work {
                    let p = scewin::nvram_path_in_work(w);
                    if p.is_file() {
                        self.open_file(p);
                    }
                }
            }
        }

        ctx.input(|i| {
            for file in &i.raw.dropped_files {
                if let Some(path) = file.path.clone() {
                    if path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case("txt"))
                        .unwrap_or(false)
                    {
                        self.open_file(path);
                    }
                }
            }
            if i.modifiers.ctrl && i.key_pressed(egui::Key::F) {
                self.search_focus_request = true;
            }
            if i.modifiers.ctrl && i.key_pressed(egui::Key::S) {
                if self.scewin_work.is_some() {
                    let _ = self.save_to_scewin_folder();
                } else {
                    self.save_as();
                }
            }
        });

        if self.search_focus_request {
            let focus_id = match self.active_tab {
                AppTab::Editor => self.search_id,
                AppTab::History => self.history_search_id,
            };
            ctx.memory_mut(|m| m.request_focus(focus_id));
            self.search_focus_request = false;
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("NVRAM Editor");
                ui.separator();
                ui.selectable_value(&mut self.active_tab, AppTab::Editor, "Editor");
                ui.selectable_value(&mut self.active_tab, AppTab::History, "History");
                ui.menu_button("File", |ui| {
                    if ui.button("Open…").clicked() {
                        ui.close();
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("NVRAM text", &["txt"])
                            .pick_file()
                        {
                            self.open_file(path);
                        }
                    }
                    let busy = self.is_busy();
                    let from_scewin = self.is_loaded_from_scewin_nvram();
                    let can_reload = self.scewin_work.is_some() && !from_scewin && !busy;
                    let reload_tip = if self.scewin_work.is_none() {
                        "SCEWIN folder not found."
                    } else if from_scewin {
                        "Already using SCEWIN nvram.txt."
                    } else if busy {
                        "Wait for SCEWIN to finish."
                    } else {
                        "Load nvram.txt from the SCEWIN folder (drops unsaved edits in the editor)."
                    };
                    if ui
                        .add_enabled(can_reload, egui::Button::new("Reload SCEWIN nvram…"))
                        .on_hover_text(reload_tip)
                        .clicked()
                    {
                        ui.close();
                        self.open_bundled_nvram();
                    }
                    ui.separator();
                    ui.add_enabled_ui(self.data.is_some() && !busy, |ui| {
                        if ui
                            .button("Save copy…")
                            .on_hover_text("Save to any folder (backup). Use Save nvram file, then Import, to apply to the BIOS.")
                            .clicked()
                        {
                            ui.close();
                            self.save_as();
                        }
                    });
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.dirty {
                        ui.label(
                            egui::RichText::new("Unsaved changes")
                                .color(egui::Color32::from_rgb(255, 180, 60)),
                        );
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label(match self.active_tab {
                    AppTab::Editor => "Search settings",
                    AppTab::History => "Search history",
                });
                ui.label(
                    egui::RichText::new("(Ctrl+F)")
                        .small()
                        .weak(),
                );
                match self.active_tab {
                    AppTab::Editor => {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.search)
                                .desired_width(360.0)
                                .hint_text("multiple words = all must match…")
                                .id(self.search_id),
                        );
                        ui.separator();
                        ui.selectable_value(&mut self.list_filter_kind, ListFilterKind::All, "All");
                        ui.selectable_value(&mut self.list_filter_kind, ListFilterKind::Options, "Options");
                        ui.selectable_value(&mut self.list_filter_kind, ListFilterKind::Values, "Values");
                        ui.checkbox(&mut self.show_only_changed, "Only changed");
                        if ui.small_button("Reset").clicked() {
                            self.list_filter_kind = ListFilterKind::All;
                            self.show_only_changed = false;
                            self.search.clear();
                        }
                        if let Some(ref d) = self.data {
                            let (n, total) = self.filter_stats(d, &self.search);
                            let changed = self.changed_count();
                            if self.search.trim().is_empty() {
                                ui.label(egui::RichText::new(format!("{total} settings")).weak());
                            } else {
                                ui.label(
                                    egui::RichText::new(format!("{n} / {total} matches"))
                                        .strong(),
                                );
                            }
                            if changed > 0 {
                                ui.label(
                                    egui::RichText::new(format!("{changed} changed"))
                                        .color(egui::Color32::from_rgb(255, 180, 60)),
                                );
                            }
                        }
                    }
                    AppTab::History => {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.history_search)
                                .desired_width(420.0)
                                .hint_text("setting name or value change…")
                                .id(self.history_search_id),
                        );
                        let total = self.change_log.len();
                        let shown = self
                            .change_log
                            .iter()
                            .filter(|e| {
                                let blob =
                                    format!("{} {}", e.setting, e.summary).to_lowercase();
                                self.history_search
                                    .split_whitespace()
                                    .all(|w| blob.contains(&w.to_lowercase()))
                            })
                            .count();
                        ui.label(egui::RichText::new(format!("{shown} / {total} entries")).weak());
                    }
                }
            });
        });

        if let Some((ok, msg)) = self.status.clone() {
            egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
                let color = if ok {
                    egui::Color32::from_rgb(140, 220, 140)
                } else {
                    egui::Color32::from_rgb(255, 120, 120)
                };
                ui.label(egui::RichText::new(msg).color(color));
            });
        }

        let dragging_txt = ctx.input(|i| {
            i.raw.hovered_files.iter().any(|f| {
                f.path
                    .as_ref()
                    .and_then(|p| p.extension())
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("txt"))
                    .unwrap_or(false)
            })
        });
        if dragging_txt {
            egui::Area::new("drag_overlay".into())
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(300.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("Drop NVRAM .txt to open")
                                    .strong()
                                    .size(18.0),
                            );
                            ui.label(egui::RichText::new("Any .txt filename is supported.").weak());
                        });
                    });
                });
        }

        if self.is_busy() {
            egui::Area::new("busy_overlay".into())
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(260.0);
                        ui.vertical_centered(|ui| {
                            ui.add_space(6.0);
                            ui.add(egui::Spinner::new().size(28.0));
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new("Running SCEWIN...").strong());
                            ui.label(egui::RichText::new("Please approve UAC and wait.").weak());
                            ui.add_space(4.0);
                        });
                    });
                });
        } else if let Some((_until, ok, text)) = self.center_toast.clone() {
            egui::Area::new("center_toast".into())
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(300.0);
                        ui.vertical_centered(|ui| {
                            ui.add_space(6.0);
                            // No unicode tick/cross - bundled font shows a box on Windows.
                            let (headline, color) = if ok {
                                ("OK", egui::Color32::from_rgb(100, 220, 130))
                            } else {
                                ("FAILED", egui::Color32::from_rgb(255, 130, 130))
                            };
                            ui.label(
                                egui::RichText::new(headline)
                                    .size(16.0)
                                    .strong()
                                    .color(color),
                            );
                            ui.add_space(4.0);
                            for line in text.split('\n') {
                                if !line.is_empty() {
                                    ui.label(
                                        egui::RichText::new(line)
                                            .size(14.0)
                                            .color(egui::Color32::from_rgb(220, 220, 220)),
                                    );
                                }
                            }
                            ui.add_space(3.0);
                            ui.label(
                                egui::RichText::new("Auto-dismiss in a few seconds, or click OK.")
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(155, 155, 155)),
                            );
                            ui.add_space(6.0);
                            if ui.button("OK").clicked() {
                                self.center_toast = None;
                            }
                            ui.add_space(3.0);
                        });
                    });
                });
        }

        if self.data.is_none() {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.heading("NVRAM Editor");
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(if scewin::has_embedded_scewin() {
                            "SCEWIN is bundled: first run may unpack to AppData; you can still use a SCEWIN folder next to the exe instead."
                        } else {
                            "Need SCEWIN (SCEWIN_64.exe + .sys): put the folder next to nvram_editor.exe, or rebuild with SCEWIN/ in the project to embed."
                        })
                        .weak(),
                    );
                    ui.add_space(8.0);
                    if self.scewin_work.is_some() {
                        if ui.button("Export (UAC)").clicked() {
                            self.run_export();
                        }
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("After that: File menu, Reload SCEWIN nvram, or drag a .txt here.")
                                .weak(),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new(
                                "SCEWIN not found. Use File → Open or drag a .txt. For Export/Import, add SCEWIN next to the exe or use a build that embeds it.",
                            )
                            .weak(),
                        );
                    }
                    ui.add_space(10.0);
                    ui.hyperlink_to("SCEHUB", "https://github.com/ab3lkaizen/SCEHUB");
                });
            });
            return;
        }

        if self.active_tab == AppTab::History {
            let mut jump_to: Option<(String, String, String)> = None;
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Change history");
                    if ui.button("Clear history").clicked() {
                        self.change_log.clear();
                    }
                });
                ui.separator();
                if self.change_log.is_empty() {
                    ui.label(egui::RichText::new("No changes in this session yet.").weak());
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for e in self.change_log.iter().filter(|e| {
                        let blob = format!("{} {}", e.setting, e.summary).to_lowercase();
                        self.history_search
                            .split_whitespace()
                            .all(|w| blob.contains(&w.to_lowercase()))
                    }) {
                        let row = format!("#{} · {}", e.seq, e.setting);
                        if ui
                            .selectable_label(false, egui::RichText::new(row).strong())
                            .on_hover_text("Jump to setting")
                            .clicked()
                        {
                            jump_to = Some((e.setting.clone(), e.token.clone(), e.offset.clone()));
                        }
                        if let Some((before, after)) = change_format::split_change_summary(&e.summary) {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new("From:")
                                        .size(14.0)
                                        .color(egui::Color32::from_rgb(170, 170, 170)),
                                );
                                ui.label(
                                    egui::RichText::new(before)
                                        .size(15.0)
                                        .color(egui::Color32::from_rgb(220, 220, 220)),
                                );
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new("To:")
                                        .size(14.0)
                                        .color(egui::Color32::from_rgb(170, 170, 170)),
                                );
                                ui.label(
                                    egui::RichText::new(after)
                                        .size(15.0)
                                        .color(egui::Color32::from_rgb(140, 220, 140)),
                                );
                            });
                        } else {
                            ui.label(
                                egui::RichText::new(&e.summary)
                                    .size(15.0)
                                    .color(egui::Color32::from_rgb(200, 200, 200)),
                            );
                        }
                        ui.add_space(2.0);
                        ui.separator();
                    }
                });
            });
            if let Some((name, token, offset)) = jump_to {
                if let Some(i) = self.find_setting_index(&token, &offset, &name) {
                    self.selected = Some(i);
                    self.sync_value_buffer();
                    self.active_tab = AppTab::Editor;
                }
            }
            return;
        }

        let mut list_click: Option<usize> = None;
        let mut do_export = false;
        let mut do_open_import_preview = false;
        let mut do_import_now = false;
        let mut do_save_scewin = false;
        let mut do_apply_presets = false;
        egui::SidePanel::left("list")
            .resizable(true)
            .default_width(440.0)
            .min_width(280.0)
            .show(ctx, |ui| {
                ui.collapsing("BIOS (SCEWIN file)", |ui| {
                    let busy = self.is_busy();
                    ui.label(
                        egui::RichText::new("Same nvram.txt in the SCEWIN folder: Export from BIOS, edit, Save, Import to BIOS.")
                            .size(11.0)
                            .color(egui::Color32::from_rgb(140, 140, 140)),
                    );
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(!busy && self.scewin_work.is_some(), egui::Button::new("Export"))
                            .on_hover_text("Read BIOS into nvram.txt (UAC). Reloads the list.")
                            .clicked()
                        {
                            do_export = true;
                        }
                        if ui
                            .add_enabled(!busy && self.data.is_some(), egui::Button::new("Save nvram file"))
                            .on_hover_text("Write your edits to nvram.txt. Do this before Import.")
                            .clicked()
                        {
                            do_save_scewin = true;
                        }
                        if ui
                            .add_enabled(
                                !busy && self.data.is_some(),
                                egui::Button::new("Import"),
                            )
                            .on_hover_text("Preview, then flash nvram.txt into the BIOS (UAC).")
                            .clicked()
                        {
                            do_open_import_preview = true;
                        }
                    });
                    ui.add_space(8.0);
                    ui.separator();
                    ui.label(egui::RichText::new("Presets").strong());
                    ui.label(
                        egui::RichText::new("Lines: Name [Option] or Name = Option (fuzzy match).")
                            .size(11.0)
                            .weak(),
                    );
                    ui.add_sized(
                        [ui.available_width(), 90.0],
                        egui::TextEdit::multiline(&mut self.preset_input)
                            .hint_text("Global C-state Control [Disabled]"),
                    );
                    if ui
                        .add_enabled(!busy && self.data.is_some(), egui::Button::new("Apply presets"))
                        .clicked()
                    {
                        do_apply_presets = true;
                    }
                });
                ui.separator();
                let Some(data_ro) = self.data.as_ref() else {
                    return;
                };
                let indices = self.filtered_indices(data_ro, &self.search);
                let (n, total) = self.filter_stats(data_ro, &self.search);
                ui.label(egui::RichText::new("Settings").strong().size(18.0));
                ui.label(
                    egui::RichText::new(if self.search.trim().is_empty() {
                        format!("{total} settings")
                    } else {
                        format!("{n} of {total} (search)")
                    })
                    .size(15.0),
                );
                ui.separator();
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(9, 9, 9))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(34, 34, 34)))
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                    for &idx in &indices {
                        let s = &data_ro.settings[idx];
                        let changed = self.is_setting_changed_at(idx);
                        let label = s.setup_question.trim().to_string();
                        let selected = self.selected == Some(idx);
                        let text = if changed {
                            egui::RichText::new(label)
                                .color(egui::Color32::from_rgb(255, 180, 60))
                                .strong()
                        } else {
                            egui::RichText::new(label)
                        };
                        let response = ui.add_sized(
                            [ui.available_width(), 24.0],
                            egui::Button::new(text)
                                .selected(selected)
                                .fill(if selected {
                                    egui::Color32::from_rgb(34, 58, 84)
                                } else {
                                    egui::Color32::TRANSPARENT
                                }),
                        );
                        if response.clicked() {
                            list_click = Some(idx);
                        }
                    }
                        });
                    });
            });
        if do_export {
            self.run_export();
        }
        if do_open_import_preview {
            if !self.import_preview_open {
                self.import_preview_filter.clear();
            }
            self.import_preview_open = true;
        }
        if do_save_scewin {
            self.save_to_scewin_folder();
        }
        if do_apply_presets {
            self.apply_presets_from_input();
        }
        if self.import_preview_open {
            let rows = self.collect_changed_rows();
            let mut open = true;
            let mut jump_to_idx: Option<usize> = None;
            egui::Window::new("Import (preview)")
                .open(&mut open)
                .resizable(true)
                .default_width(820.0)
                .default_height(520.0)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("These settings will be imported into the BIOS:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.import_preview_filter)
                            .desired_width(ui.available_width())
                            .hint_text("Filter by setting name or value..."),
                    );
                    ui.separator();
                    if rows.is_empty() {
                        ui.label(egui::RichText::new("No changes to import.").weak());
                    } else {
                        let q = self.import_preview_filter.trim().to_lowercase();
                        let filtered: Vec<_> = rows
                            .iter()
                            .filter(|(_, name, old_v, new_v)| {
                                if q.is_empty() {
                                    return true;
                                }
                                let blob = format!("{name} {old_v} {new_v}").to_lowercase();
                                q.split_whitespace().all(|w| blob.contains(w))
                            })
                            .collect();
                        ui.label(
                            egui::RichText::new(format!(
                                "{} / {} changed settings",
                                filtered.len(),
                                rows.len()
                            ))
                            .weak(),
                        );
                        ui.separator();
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for (idx, name, old_v, new_v) in filtered {
                                if ui
                                    .selectable_label(false, egui::RichText::new(name).strong())
                                    .clicked()
                                {
                                    jump_to_idx = Some(*idx);
                                }
                                ui.label(egui::RichText::new(format!("Old: {old_v}")).weak());
                                ui.label(egui::RichText::new(format!("New: {new_v}")));
                                ui.separator();
                            }
                        });
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.import_preview_open = false;
                        }
                        let can_import = !rows.is_empty() && !self.is_busy();
                        if ui
                            .add_enabled(
                                can_import,
                                egui::Button::new("Import (UAC)"),
                            )
                            .clicked()
                        {
                            do_import_now = true;
                        }
                    });
                });
            self.import_preview_open = self.import_preview_open && open;
            if let Some(i) = jump_to_idx {
                self.selected = Some(i);
                self.sync_value_buffer();
            }
        }
        if do_import_now {
            self.import_preview_open = false;
            self.run_import();
        }
        if let Some(i) = list_click {
            self.selected = Some(i);
            self.sync_value_buffer();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(idx) = self.selected else {
                ui.label("Select a setting on the left.");
                return;
            };
            let value_outer_prefix = self.value_outer_prefix.clone();
            let value_outer_suffix = self.value_outer_suffix.clone();
            let Some(data) = self.data.as_mut() else {
                return;
            };
            let setting = data.settings[idx].clone();
            let current_value_label = if !setting.options.is_empty() {
                setting
                    .active_option
                    .and_then(|i| setting.options.get(i))
                    .map(|s| option_pretty(s))
                    .unwrap_or_else(|| setting.display_current())
            } else {
                setting.value.clone().unwrap_or_default()
            };

            ui.heading(&setting.setup_question);
            ui.add_space(8.0);

            egui::Grid::new("meta").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Token");
                ui.monospace(&setting.token);
                ui.label("Offset");
                ui.monospace(&setting.offset);
                ui.label("Width");
                ui.monospace(&setting.width);
                if let Some(ref d) = setting.bios_default {
                    ui.label("BIOS Default");
                    ui.label(d);
                }
                ui.end_row();
            });

            if !setting.help_string.is_empty() {
                ui.add_space(6.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Help").strong());
                    ui.label(&setting.help_string);
                });
            }

            ui.add_space(6.0);
            ui.group(|ui| {
                ui.label(egui::RichText::new("Current setting").strong());
                ui.label(
                    egui::RichText::new(current_value_label)
                        .monospace()
                        .color(egui::Color32::from_rgb(180, 220, 255)),
                );
            });

            let show_options = !setting.options.is_empty();
            let show_value_editor = !show_options;

            if show_options || show_value_editor {
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
            }

            if !setting.options.is_empty() {
                let setup_name = setting.setup_question.clone();
                let opts = setting.options.clone();
                let before = data.settings[idx].active_option;
                let mut sel = before.unwrap_or(0).min(opts.len().saturating_sub(1));
                ui.add_space(6.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Options").strong());
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .id_salt(("options_scroll", idx))
                        .show(ui, |ui| {
                            for (i, opt) in opts.iter().enumerate() {
                                let checked = sel == i;
                                let pretty = option_pretty(opt);
                                let rich = if checked {
                                    egui::RichText::new(pretty).strong()
                                } else {
                                    egui::RichText::new(pretty)
                                };
                                if ui
                                    .add_sized(
                                        [ui.available_width() - 12.0, 22.0],
                                        egui::Button::new(rich).selected(checked),
                                    )
                                    .clicked()
                                {
                                    sel = i;
                                }
                            }
                        });
                });
                if Some(sel) != before {
                    let plan = presets::duplicate_name_option_sync_plan(
                        &*data,
                        setup_name.trim(),
                        sel,
                        opts.as_slice(),
                    );
                    let mut log_entries: Vec<(usize, usize, String, String, String, String)> =
                        Vec::new();
                    for &(j, ts) in &plan {
                        let s = &data.settings[j];
                        let old_s = s
                            .active_option
                            .and_then(|oi| s.options.get(oi))
                            .map(|v| option_pretty(v))
                            .unwrap_or_else(|| "(unset)".to_string());
                        let new_s = s
                            .options
                            .get(ts)
                            .map(|v| option_pretty(v))
                            .unwrap_or_else(|| "(unset)".to_string());
                        log_entries.push((
                            j,
                            ts,
                            s.setup_question.clone(),
                            s.token.clone(),
                            s.offset.clone(),
                            format!("{old_s} → {new_s}"),
                        ));
                    }
                    for (j, ts, _, _, _, _) in &log_entries {
                        data.settings[*j].active_option = Some(*ts);
                    }
                    for (_, _, q, tk, off, summary) in log_entries {
                        self.push_change_log(&q, &tk, &off, summary);
                    }
                    self.dirty = true;
                }
            } else if show_value_editor {
                let setup_name = setting.setup_question.clone();
                let setup_token = setting.token.clone();
                let setup_offset = setting.offset.clone();
                let mut value_changed = false;
                let mut value_lost_focus = false;
                ui.add_space(6.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Value").strong());
                    let out = if value_outer_prefix == "<" && value_outer_suffix == ">" {
                        ui.horizontal(|ui| {
                            let full_w = ui.available_width();
                            let row_w = (full_w - 8.0).clamp(280.0, 720.0);
                            let left_pad = ((full_w - row_w) * 0.5).max(0.0);
                            ui.add_space(left_pad);
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(
                                egui::RichText::new("<")
                                    .monospace()
                                    .size(18.0)
                                    .color(egui::Color32::from_rgb(170, 170, 170)),
                            );
                            let response = ui.add_sized(
                                [row_w - 34.0, 30.0],
                                egui::TextEdit::singleline(&mut self.value_edit_buffer)
                                    .font(egui::TextStyle::Monospace)
                                    .horizontal_align(egui::Align::Center),
                            );
                            ui.label(
                                egui::RichText::new(">")
                                    .monospace()
                                    .size(18.0)
                                    .color(egui::Color32::from_rgb(170, 170, 170)),
                            );
                            response
                        })
                        .inner
                    } else {
                        ui.add_sized(
                            [ui.available_width(), 30.0],
                            egui::TextEdit::singleline(&mut self.value_edit_buffer)
                                .font(egui::TextStyle::Monospace)
                                .horizontal_align(egui::Align::Center),
                        )
                    };
                    value_changed = out.changed();
                    value_lost_focus = out.lost_focus();
                });
                if value_changed {
                    data.settings[idx].value = Some(format!(
                        "{}{}{}",
                        value_outer_prefix, self.value_edit_buffer, value_outer_suffix
                    ));
                    self.dirty = true;
                }
                if value_lost_focus && self.value_edit_buffer != self.value_edit_baseline {
                    let old_value = format!(
                        "{}{}{}",
                        value_outer_prefix, self.value_edit_baseline, value_outer_suffix
                    );
                    let new_value = format!(
                        "{}{}{}",
                        value_outer_prefix, self.value_edit_buffer, value_outer_suffix
                    );
                    self.push_change_log(
                        &setup_name,
                        &setup_token,
                        &setup_offset,
                        format!("{old_value} → {new_value}"),
                    );
                    self.value_edit_baseline = self.value_edit_buffer.clone();
                }
            }
        });
    }
}
