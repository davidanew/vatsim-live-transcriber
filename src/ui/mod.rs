//! Native egui transcript window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui::{
    self, Color32, ComboBox, Frame, Label, Layout, Margin, RichText, ScrollArea, Stroke, TextEdit,
    Vec2,
};

use crate::audio::{AudioDevice, ChannelSelection, resolve_device};
use crate::realtime::{TranscriberConfig, TranscriberHandle, TranscriptEvent};
use crate::recording::play_wav;

const ACCURACY_CHOICES: [&str; 5] = ["minimal", "low", "medium", "high", "xhigh"];

#[derive(Debug)]
pub struct AppOptions {
    pub devices: Vec<AudioDevice>,
    pub requested_device: Option<String>,
    pub requested_channel: Option<ChannelSelection>,
    pub accuracy: String,
    pub api_key: String,
    pub prompt: String,
    pub keywords: Vec<String>,
    pub output_path: PathBuf,
    pub recordings_dir: PathBuf,
    pub vad_rms: f32,
    pub silence_ms: u32,
}

#[derive(Debug)]
struct TransmissionRow {
    original: String,
    converted: String,
    audio_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Setup,
    Transcript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionStatus {
    Ready,
    Connecting,
    Connected,
    Stopping,
    Error,
}

pub struct TranscriberApp {
    screen: Screen,
    devices: Vec<AudioDevice>,
    selected_device: usize,
    channel: ChannelSelection,
    accuracy: String,
    api_key: String,
    prompt: String,
    keywords: Vec<String>,
    output_path: PathBuf,
    recordings_dir: PathBuf,
    vad_rms: f32,
    silence_ms: u32,
    rows: Vec<TransmissionRow>,
    row_for_item: HashMap<String, usize>,
    worker: Option<TranscriberHandle>,
    event_receiver: Option<Receiver<TranscriptEvent>>,
    session_status: SessionStatus,
    status_text: String,
}

impl TranscriberApp {
    pub fn new(context: &eframe::CreationContext<'_>, options: AppOptions) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(11, 18, 32);
        visuals.window_fill = Color32::from_rgb(11, 18, 32);
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(31, 41, 55);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(55, 65, 81);
        context.egui_ctx.set_visuals(visuals);

        let resolved_device = options
            .requested_device
            .as_deref()
            .and_then(|requested| resolve_device(&options.devices, requested).ok());
        let selected_device = resolved_device
            .and_then(|selected| {
                options
                    .devices
                    .iter()
                    .position(|device| device.id == selected.id)
            })
            .unwrap_or(0);
        let fully_configured = resolved_device.is_some()
            && options.requested_channel.is_some()
            && !options.api_key.trim().is_empty();

        Self {
            screen: if fully_configured {
                Screen::Transcript
            } else {
                Screen::Setup
            },
            devices: options.devices,
            selected_device,
            channel: options.requested_channel.unwrap_or(ChannelSelection::Left),
            accuracy: options.accuracy,
            api_key: options.api_key,
            prompt: options.prompt,
            keywords: options.keywords,
            output_path: options.output_path,
            recordings_dir: options.recordings_dir,
            vad_rms: options.vad_rms,
            silence_ms: options.silence_ms,
            rows: Vec::new(),
            row_for_item: HashMap::new(),
            worker: None,
            event_receiver: None,
            session_status: SessionStatus::Ready,
            status_text: "Ready - press Start".to_owned(),
        }
    }

    fn start(&mut self) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            self.set_error("The previous session is still stopping. Try again.");
            return;
        }
        let Some(device) = self.devices.get(self.selected_device).cloned() else {
            self.set_error("No Windows output device is selected.");
            return;
        };
        if self.api_key.trim().is_empty() {
            self.set_error("Enter an OpenAI API key in the setup screen.");
            return;
        }

        let (sender, receiver) = mpsc::channel();
        let config = TranscriberConfig {
            api_key: self.api_key.trim().to_owned(),
            device,
            channel: self.channel,
            accuracy: self.accuracy.clone(),
            prompt: self.prompt.clone(),
            keywords: self.keywords.clone(),
            output_path: self.output_path.clone(),
            recordings_dir: self.recordings_dir.clone(),
            vad_rms: self.vad_rms,
            silence_ms: self.silence_ms,
        };
        self.event_receiver = Some(receiver);
        self.worker = Some(TranscriberHandle::start(config, sender));
        self.session_status = SessionStatus::Connecting;
        self.status_text = "Connecting...".to_owned();
    }

    fn stop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.stop();
            self.session_status = SessionStatus::Stopping;
            self.status_text = "Stopping...".to_owned();
        }
    }

    fn set_error(&mut self, message: impl Into<String>) {
        self.session_status = SessionStatus::Error;
        self.status_text = message.into();
    }

    fn process_events(&mut self) {
        let events = self
            .event_receiver
            .as_ref()
            .map(|receiver| receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for event in events {
            match event {
                TranscriptEvent::Connected => {
                    self.session_status = SessionStatus::Connected;
                    self.status_text = "Connected - waiting for radio audio".to_owned();
                }
                TranscriptEvent::Delta {
                    item_id,
                    original,
                    converted,
                } => {
                    let index = self.ensure_row(&item_id);
                    self.rows[index].original = original.trim_start().to_owned();
                    self.rows[index].converted = converted.trim_start().to_owned();
                }
                TranscriptEvent::Completed {
                    item_id,
                    original,
                    converted,
                    audio_path,
                } => {
                    let index = self.ensure_row(&item_id);
                    self.rows[index].original = original;
                    self.rows[index].converted = converted;
                    self.rows[index].audio_path = audio_path;
                    self.row_for_item.remove(&item_id);
                }
                TranscriptEvent::Error(message) => self.set_error(message),
                TranscriptEvent::Stopped => {
                    if let Some(mut worker) = self.worker.take() {
                        worker.join();
                    }
                    self.event_receiver = None;
                    if self.session_status != SessionStatus::Error {
                        self.session_status = SessionStatus::Ready;
                        self.status_text = "Stopped - press Start to reconnect".to_owned();
                    }
                }
            }
        }
    }

    fn ensure_row(&mut self, item_id: &str) -> usize {
        if let Some(index) = self.row_for_item.get(item_id) {
            return *index;
        }
        let index = self.rows.len();
        self.rows.push(TransmissionRow {
            original: String::new(),
            converted: String::new(),
            audio_path: None,
        });
        self.row_for_item.insert(item_id.to_owned(), index);
        index
    }

    fn status_color(&self) -> Color32 {
        match self.session_status {
            SessionStatus::Connected => Color32::from_rgb(34, 197, 94),
            SessionStatus::Connecting | SessionStatus::Stopping => Color32::from_rgb(251, 191, 36),
            SessionStatus::Error => Color32::from_rgb(239, 68, 68),
            SessionStatus::Ready => Color32::from_rgb(156, 163, 175),
        }
    }

    fn show_setup(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.heading("Start live transcription");
            ui.label("Choose the Windows output carrying your VATSIM audio.");
        });
        ui.add_space(24.0);

        egui::Grid::new("setup-grid")
            .num_columns(2)
            .spacing([20.0, 14.0])
            .show(ui, |ui| {
                ui.strong("Output device");
                let selected_name = self
                    .devices
                    .get(self.selected_device)
                    .map_or("No devices found", |device| device.name.as_str());
                ComboBox::from_id_salt("device")
                    .selected_text(selected_name)
                    .width(450.0)
                    .show_ui(ui, |ui| {
                        for (index, device) in self.devices.iter().enumerate() {
                            ui.selectable_value(&mut self.selected_device, index, &device.name);
                        }
                    });
                ui.end_row();

                ui.strong("Channel");
                ComboBox::from_id_salt("channel")
                    .selected_text(self.channel.to_string())
                    .show_ui(ui, |ui| {
                        for channel in [
                            ChannelSelection::Left,
                            ChannelSelection::Right,
                            ChannelSelection::Mix,
                        ] {
                            ui.selectable_value(&mut self.channel, channel, channel.to_string());
                        }
                    });
                ui.end_row();

                ui.strong("Accuracy");
                ComboBox::from_id_salt("accuracy")
                    .selected_text(&self.accuracy)
                    .show_ui(ui, |ui| {
                        for accuracy in ACCURACY_CHOICES {
                            ui.selectable_value(&mut self.accuracy, accuracy.to_owned(), accuracy);
                        }
                    });
                ui.end_row();

                ui.strong("OpenAI API key");
                ui.add(
                    TextEdit::singleline(&mut self.api_key)
                        .password(true)
                        .desired_width(450.0),
                );
                ui.end_row();
            });

        ui.add_space(24.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = !self.devices.is_empty() && !self.api_key.trim().is_empty();
                if ui
                    .add_enabled(enabled, egui::Button::new("Open transcriber"))
                    .clicked()
                {
                    self.screen = Screen::Transcript;
                    self.session_status = SessionStatus::Ready;
                    self.status_text = "Ready - press Start".to_owned();
                }
            });
        });
    }

    fn show_transcript(&mut self, ui: &mut egui::Ui) {
        let active = self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished());
        ui.heading("VATSIM Live Transcriber");
        let device_name = self
            .devices
            .get(self.selected_device)
            .map_or("Unknown device", |device| device.name.as_str());
        ui.label(
            RichText::new(format!(
                "{}  |  {}  |  {} accuracy",
                device_name,
                self.channel.to_string().to_ascii_uppercase(),
                self.accuracy
            ))
            .color(Color32::from_rgb(156, 163, 175)),
        );
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&self.status_text)
                    .strong()
                    .color(self.status_color()),
            );
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(active, egui::Button::new("Stop")).clicked() {
                    self.stop();
                }
                if ui
                    .add_enabled(!active, egui::Button::new("Start"))
                    .clicked()
                {
                    self.start();
                }
                if ui
                    .add_enabled(!self.rows.is_empty(), egui::Button::new("Clear"))
                    .clicked()
                {
                    self.rows.clear();
                    self.row_for_item.clear();
                }
                if ui
                    .add_enabled(!active, egui::Button::new("Setup"))
                    .clicked()
                {
                    self.screen = Screen::Setup;
                }
            });
        });
        ui.add_space(8.0);

        let mut play_requested = None;
        Frame::new()
            .fill(Color32::from_rgb(3, 7, 18))
            .stroke(Stroke::new(1.0, Color32::from_rgb(31, 41, 55)))
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt("transcript-history")
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_min_height((ui.available_height() - 8.0).max(100.0));
                        for row in &self.rows {
                            Frame::new()
                                .fill(Color32::from_rgb(17, 24, 39))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(31, 41, 55)))
                                .corner_radius(5)
                                .inner_margin(Margin::symmetric(14, 10))
                                .show(ui, |ui| {
                                    ui.horizontal_top(|ui| {
                                        ui.vertical(|ui| {
                                            ui.set_width((ui.available_width() - 70.0).max(200.0));
                                            ui.add(
                                                Label::new(
                                                    RichText::new(format!("> {}", row.original))
                                                        .monospace()
                                                        .color(Color32::from_rgb(243, 244, 246)),
                                                )
                                                .wrap()
                                                .selectable(true),
                                            );
                                            ui.add(
                                                Label::new(
                                                    RichText::new(&row.converted)
                                                        .monospace()
                                                        .color(Color32::from_rgb(34, 197, 94)),
                                                )
                                                .wrap()
                                                .selectable(true),
                                            );
                                        });
                                        if ui
                                            .add_enabled(
                                                row.audio_path.is_some(),
                                                egui::Button::new("Play"),
                                            )
                                            .clicked()
                                        {
                                            play_requested = row.audio_path.clone();
                                        }
                                    });
                                });
                            ui.add_space(8.0);
                        }
                    });
            });
        if let Some(path) = play_requested
            && let Err(error) = play_wav(&path)
        {
            self.set_error(format!("{error:#}"));
        }

        ui.add_space(7.0);
        ui.label(
            RichText::new(format!(
                "Transcript: {}\nRecordings: {}",
                self.output_path.display(),
                self.recordings_dir.display()
            ))
            .small()
            .color(Color32::from_rgb(107, 114, 128)),
        );
    }
}

impl eframe::App for TranscriberApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events();
        if self.worker.is_some() {
            context.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        Frame::central_panel(ui.style())
            .inner_margin(Margin::same(16))
            .show(ui, |ui| match self.screen {
                Screen::Setup => self.show_setup(ui),
                Screen::Transcript => self.show_transcript(ui),
            });
    }
}

impl Drop for TranscriberApp {
    fn drop(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            worker.join();
        }
    }
}

pub fn run(options: AppOptions) -> Result<(), String> {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(Vec2::new(1000.0, 650.0))
            .with_min_inner_size(Vec2::new(650.0, 380.0)),
        ..Default::default()
    };
    eframe::run_native(
        "VATSIM Live Transcriber",
        native_options,
        Box::new(move |context| Ok(Box::new(TranscriberApp::new(context, options)))),
    )
    .map_err(|error| error.to_string())
}
