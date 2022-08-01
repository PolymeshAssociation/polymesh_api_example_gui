use std::collections::VecDeque;

use egui::*;

use crate::backend::*;

const POLYMESH_STAGING: &str = "wss://staging-rpc.polymesh.live";
const MAX_BACKEND_UPDATES: usize = 20;

/// Backend chain state.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct BackendState {
  open: bool,
  url: String,

  #[serde(skip)]
  need_save: bool,

  #[serde(skip)]
  blocks: VecDeque<Header>,
  #[serde(skip)]
  backend: Option<Backend>,
}

impl Default for BackendState {
  fn default() -> Self {
    Self {
      open: true,
      need_save: true,
      url: POLYMESH_STAGING.to_owned(),
      blocks: Default::default(),
      backend: None,
    }
  }
}

impl BackendState {
  pub fn restart_backend(&mut self) {
    if self.backend.is_some() {
      log::info!("Reconnect to backend.");
    } else {
      log::info!("Connect to backend.");
    }
    self.blocks.clear();
    self.backend = Some(Backend::new(&self.url));
  }

  fn check_node_url(&mut self) {
    let backend_url = self.backend.as_ref().map(|b| b.get_url());
    if let Some(backend_url) = backend_url {
      if backend_url != self.url {
        log::info!("Node url changed.  Reconnect to backend.");
        self.restart_backend();
      }
    }
  }

  pub fn check_need_save(&mut self) -> bool {
    if self.need_save {
      self.check_node_url();
      // Clear flag.
      self.need_save = false;
      true
    } else {
      false
    }
  }

  pub fn backend_updates(&mut self) {
    if let Some(ref mut backend) = &mut self.backend {
      // Poll the backend for updates.
      for _ in 0..MAX_BACKEND_UPDATES {
        match backend.next_update() {
          Some(UpdateMessage::NewBlock(header)) => {
            if self.blocks.len() > 1000 {
              self.blocks.pop_back();
            }
            self.blocks.push_front(header);
          }
          None => {
            // Channel is empty.
            break;
          }
        }
      }
    }
  }

  pub fn set_url(&mut self, url: &str) {
    self.url = url.into();
    self.need_save = true;
  }

  fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
    // Always repaint to allow pulling the backend for updates.
    ctx.request_repaint();
    // Pull the backend for updates.
    self.backend_updates();
  }

  pub fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
    ui.label("Polymesh: ");
    if ui.button("Staging").clicked() {
      self.set_url(POLYMESH_STAGING);
    }
    ui.label("Custom: ");
    if ui.button("Local").clicked() {
      self.set_url("ws://localhost:9944/");
    }
    ui.horizontal(|ui| {
      ui.label("Custom node: ");
      let resp = ui.text_edit_singleline(&mut self.url);
      if resp.lost_focus() && ui.input().key_pressed(egui::Key::Enter) {
        self.need_save = true;
      }
    });

    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
      ui.heading("Polymesh Rust GUI");
      ui.hyperlink_to(
        "Source code",
        "https://github.com/PolymeshAssociation/polymesh_api_example_gui",
      );
      egui::warn_if_debug_build(ui);
    });
  }
}

/// Main Polymesh app.
#[derive(Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct PolymeshApp {
  backend: BackendState,
}

impl PolymeshApp {
  /// Called once before the first frame.
  pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
    let mut app: Self = if let Some(storage) = cc.storage {
      eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default()
    } else {
      Default::default()
    };

    cc.egui_ctx.set_visuals(egui::Visuals::dark());

    app.backend.restart_backend();

    app
  }

  fn save_changes(&mut self, frame: &mut eframe::Frame) {
    if !self.backend.check_need_save() {
      return;
    }
    if let Some(storage) = frame.storage_mut() {
      use eframe::App;
      self.save(storage);
    }
  }

  fn top_navbar_ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
    egui::widgets::global_dark_light_mode_switch(ui);
    ui.separator();
    ui.toggle_value(&mut self.backend.open, "💻 Backend");
  }

  fn show_current_app(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
    ui.label("Blocks:");
    let blocks = &self.backend.blocks;
    let text_style = TextStyle::Body;
    let row_height = ui.text_style_height(&text_style);
    let num_rows = blocks.len();
    ScrollArea::vertical().auto_shrink([false; 2]).show_rows(
      ui,
      row_height,
      num_rows,
      |ui, row_range| {
        for header in blocks.range(row_range) {
          let text = format!("{}: {}", header.number, header.hash());
          ui.label(text);
        }
      },
    );
  }
}

impl eframe::App for PolymeshApp {
  /// Called by the frame work to save state before shutdown.
  fn save(&mut self, storage: &mut dyn eframe::Storage) {
    log::info!("Save app");
    eframe::set_value(storage, eframe::APP_KEY, self);
  }

  /// Main entry point for UI updates.
  fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
    // Backend updates.
    self.backend.update(ctx, frame);

    self.save_changes(frame);

    egui::TopBottomPanel::top("top_navbar").show(ctx, |ui| {
      ui.horizontal_wrapped(|ui| {
        ui.visuals_mut().button_frame = false;
        self.top_navbar_ui(ui, frame);
      });
    });

    if self.backend.open {
      egui::SidePanel::left("side_panel").show(ctx, |ui| {
        self.backend.ui(ui, frame);
      });
    }

    egui::CentralPanel::default().show(ctx, |ui| {
      self.show_current_app(ui, frame);
    });
  }
}
