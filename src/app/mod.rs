use std::collections::VecDeque;

use egui::*;

use crate::backend::*;

/// We derive Deserialize/Serialize so we can persist app state on shutdown.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct PolymeshApp {
  url: String,

  #[serde(skip)]
  blocks: VecDeque<Header>,
  #[serde(skip)]
  backend: Option<Backend>,
}

impl Default for PolymeshApp {
  fn default() -> Self {
    Self {
      url: "wss://staging-rpc.polymesh.live".to_owned(),
      blocks: Default::default(),
      backend: None,
    }
  }
}

impl PolymeshApp {
  /// Called once before the first frame.
  pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
    let mut app: Self = if let Some(storage) = cc.storage {
      eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default()
    } else {
      Default::default()
    };

    app.backend = Some(Backend::new(&app.url));

    app
  }

  fn backend_updates(&mut self) {
    if let Some(ref mut backend) = &mut self.backend {
      match backend.next_update() {
        Some(UpdateMessage::NewBlock(header)) => {
          println!("{}: {}", header.number, header.hash());
          //eprintln!("Got new block:")
          if self.blocks.len() > 1000 {
            self.blocks.pop_back();
          }
          self.blocks.push_front(header);
        }
        None => (),
      }
    }
  }
}

impl eframe::App for PolymeshApp {
  /// Called by the frame work to save state before shutdown.
  fn save(&mut self, storage: &mut dyn eframe::Storage) {
    eframe::set_value(storage, eframe::APP_KEY, self);
  }

  /// Called each time the UI needs repainting, which may be many times per second.
  /// Put your widgets into a `SidePanel`, `TopPanel`, `CentralPanel`, `Window` or `Area`.
  fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
    // Always repaint to allow pulling the backend for updates.
    ctx.request_repaint();
    // Pull the backend for updates.
    self.backend_updates();

    let Self { url, blocks, .. } = self;

    // Examples of how to create different panels and windows.
    // Pick whichever suits you.
    // Tip: a good default choice is to just keep the `CentralPanel`.
    // For inspiration and more examples, go to https://emilk.github.io/egui

    egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
      // The top panel is often a good place for a menu bar:
      egui::menu::bar(ui, |ui| {
        ui.menu_button("File", |ui| {
          if ui.button("Quit").clicked() {
            frame.quit();
          }
        });
      });
    });

    egui::SidePanel::left("side_panel").show(ctx, |ui| {
      ui.heading("Side Panel");

      ui.horizontal(|ui| {
        ui.label("node url: ");
        ui.text_edit_singleline(url);
      });

      ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        ui.heading("Polymesh Rust GUI");
        ui.hyperlink_to(
          "Source code",
          "https://github.com/PolymeshAssociation/polymesh_api_example_gui",
        );
        egui::warn_if_debug_build(ui);
      });
    });

    egui::CentralPanel::default().show(ctx, |ui| {
      ui.label("Blocks:");
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
    });
  }
}
