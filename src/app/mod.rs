use std::collections::{HashMap, VecDeque};

use egui::*;

use crate::backend::*;

const POLYMESH_STAGING: &str = "wss://staging-rpc.polymesh.live";
const MAX_BACKEND_UPDATES: usize = 100;
const MAX_RECENT_BLOCKS: usize = 2000;
const MAX_RECENT_EVENTS: usize = 2000;

#[derive(Debug)]
pub struct BlockEventSummary {
  pub block: BlockNumber,
  pub number: u32,
  pub name: String,
  /// Count the number of events in the block with the same type.
  pub count: u32,
}

/// Backend chain state.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct BackendState {
  open: bool,
  url: String,

  #[serde(skip)]
  need_save: bool,

  #[serde(skip)]
  genesis_hash: Option<BlockHash>,

  #[serde(skip)]
  blocks: HashMap<BlockNumber, BlockInfo>,
  #[serde(skip)]
  recent_blocks: VecDeque<BlockNumber>,
  #[serde(skip)]
  recent_events: VecDeque<BlockEventSummary>,
  #[serde(skip)]
  backend: Option<Backend>,
}

impl Default for BackendState {
  fn default() -> Self {
    Self {
      open: true,
      need_save: true,
      url: POLYMESH_STAGING.to_owned(),
      genesis_hash: None,
      blocks: Default::default(),
      recent_blocks: Default::default(),
      recent_events: Default::default(),
      backend: None,
    }
  }
}

impl BackendState {
  fn clear(&mut self) {
    self.genesis_hash = None;
    self.blocks.clear();
    self.recent_events.clear();
  }

  pub fn restart_backend(&mut self) {
    if self.backend.is_some() {
      log::info!("Reconnect to backend.");
    } else {
      log::info!("Connect to backend.");
    }
    self.clear();
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
    let mut need_clear = false;
    if let Some(ref mut backend) = &mut self.backend {
      // Poll the backend for updates.
      for _ in 0..MAX_BACKEND_UPDATES {
        match backend.next_update() {
          Some(UpdateMessage::Connected {
            genesis,
            is_reconnect,
          }) => {
            log::info!("Connected to backend: {genesis:?}, is_reconnect={is_reconnect}");
            if is_reconnect {
              // Check if the chain is the same.
              if self.genesis_hash != Some(genesis) {
                log::info!("---- Different genesis hash clear chain state.");
                // Clear old chain data.
                need_clear = true;
              }
            }
            self.genesis_hash = Some(genesis);
          }
          Some(UpdateMessage::NewBlock(block)) => {
            // Update recent events.
            block
              .events
              .iter()
              .fold(
                HashMap::new(),
                |mut events: HashMap<(_, _), BlockEventSummary>, event| {
                  use std::collections::hash_map::Entry;
                  // Ignore some common events.
                  if event.name.starts_with("System.") {
                    return events;
                  }

                  let key = (event.block, &event.name);
                  match events.entry(key) {
                    Entry::Occupied(entry) => {
                      // Duplicate event type, just bump the count.
                      entry.into_mut().count += 1;
                    }
                    Entry::Vacant(entry) => {
                      // New event type.
                      entry.insert(BlockEventSummary {
                        block: event.block,
                        number: event.number,
                        name: event.name.clone(),
                        count: 1,
                      });
                    }
                  }

                  events
                },
              )
              .into_iter()
              .for_each(|(_, event)| {
                self.recent_events.push_front(event);
              });
            // Update recent blocks.
            let number = block.number();
            if self.blocks.insert(number, block).is_none() {
              self.recent_blocks.push_front(number);
            }
            // Trim old events.
            while self.recent_events.len() > MAX_RECENT_EVENTS {
              self.recent_events.pop_back();
            }
            // Trim old blocks.
            while self.recent_blocks.len() > MAX_RECENT_BLOCKS {
              if let Some(number) = self.recent_blocks.pop_back() {
                self.blocks.remove(&number);
              }
            }
          }
          None => {
            // Channel is empty.
            break;
          }
        }
      }
    }
    if need_clear {
      self.clear();
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

pub enum SubAppEvent {
  BlockDetails(BlockHash),
}

pub trait SubApp {
  fn name(&self) -> &str;
  fn anchor(&self) -> &str;

  fn match_anchor(&self, anchor: &str) -> bool {
    anchor.starts_with(self.anchor())
  }

  fn update(
    &mut self,
    backend: &mut BackendState,
    ctx: &egui::Context,
    anchor: &str,
  ) -> Option<SubAppEvent>;
}

/// Chain Info sub-app.
#[derive(Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct ChainInfoApp {}

impl ChainInfoApp {
  fn recent_blocks_ui(&self, backend: &mut BackendState, ui: &mut egui::Ui) -> Option<SubAppEvent> {
    let mut app_event = None;
    ui.label("Recent blocks:");
    ui.separator();
    ui.push_id("Blocks", |ui| {
      let blocks = &backend.recent_blocks;
      let text_style = TextStyle::Body;
      let row_height = ui.text_style_height(&text_style);
      let num_rows = blocks.len();
      ScrollArea::vertical().auto_shrink([false; 2]).show_rows(
        ui,
        row_height,
        num_rows,
        |ui, row_range| {
          for number in blocks.range(row_range) {
            let block = backend.blocks.get(number).unwrap();
            ui.horizontal(|ui| {
              if ui.link(format!("{}", block.number())).clicked() {
                app_event = Some(SubAppEvent::BlockDetails(block.hash));
              }
              ui.label(format!("{:?}", block.hash));
            });
          }
        },
      );
    });
    app_event
  }

  fn recent_events_ui(&self, backend: &mut BackendState, ui: &mut egui::Ui) -> Option<SubAppEvent> {
    let mut app_event = None;
    ui.label("Recent events:");
    ui.separator();
    ui.push_id("Events", |ui| {
      let events = &backend.recent_events;
      let text_style = TextStyle::Body;
      let row_height = ui.text_style_height(&text_style);
      let num_rows = events.len();
      ScrollArea::vertical().auto_shrink([false; 2]).show_rows(
        ui,
        row_height,
        num_rows,
        |ui, row_range| {
          for event in events.range(row_range) {
            ui.horizontal(|ui| {
              ui.label(format!("{}", event.name));
              ui.with_layout(egui::Layout::right_to_left(), |ui| {
                if ui
                  .link(format!("{}-{}", event.block, event.number))
                  .clicked()
                {
                  if let Some(block) = backend.blocks.get(&event.block) {
                    app_event = Some(SubAppEvent::BlockDetails(block.hash));
                  }
                }
                if event.count > 1 {
                  ui.label(format!("({}x)", event.count));
                }
              });
            });
          }
        },
      );
    });
    app_event
  }
}

impl SubApp for ChainInfoApp {
  fn name(&self) -> &str {
    "Explorer"
  }

  fn anchor(&self) -> &str {
    "explorer"
  }

  fn match_anchor(&self, anchor: &str) -> bool {
    anchor.starts_with("explorer")
  }

  fn update(
    &mut self,
    backend: &mut BackendState,
    ctx: &egui::Context,
    _anchor: &str,
  ) -> Option<SubAppEvent> {
    let mut app_event = None;
    egui::CentralPanel::default().show(ctx, |ui| {
      let height = ui.available_height();
      ui.horizontal(|ui| {
        ui.set_height(height);
        ui.group(|ui| {
          let width = ui.available_width() / 2.0;
          let height = ui.available_height();
          ui.set_height(height);
          ui.set_width(width);
          ui.vertical(|ui| {
            if let Some(event) = self.recent_blocks_ui(backend, ui) {
              app_event = Some(event);
            }
          });
        });
        ui.group(|ui| {
          let height = ui.available_height();
          let width = ui.available_width();
          ui.set_height(height);
          ui.set_width(width);
          ui.vertical(|ui| {
            if let Some(event) = self.recent_events_ui(backend, ui) {
              app_event = Some(event);
            }
          });
        });
      });
    });
    app_event
  }
}

/// Chain Info sub-app.
#[derive(Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct BlockDetailsApp {}

impl SubApp for BlockDetailsApp {
  fn name(&self) -> &str {
    "Block details"
  }

  fn anchor(&self) -> &str {
    "block_details"
  }

  fn update(
    &mut self,
    _backend: &mut BackendState,
    ctx: &egui::Context,
    anchor: &str,
  ) -> Option<SubAppEvent> {
    egui::CentralPanel::default().show(ctx, |ui| {
      ui.label(format!("TODO: show block details for: {anchor:?}"));
    });
    None
  }
}

/// Sub-Apps.
#[derive(Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct State {
  selected: String,
  current_anchor: String,
  chain_info: ChainInfoApp,
  block_details: BlockDetailsApp,
}

impl State {
  fn apps(&mut self) -> impl Iterator<Item = &mut dyn SubApp> {
    let apps = vec![
      &mut self.chain_info as &mut dyn SubApp,
      &mut self.block_details as &mut dyn SubApp,
    ];

    apps.into_iter()
  }

  fn open_anchor(&mut self, anchor: &str, ctx: &egui::Context, frame: &mut eframe::Frame) {
    if self.current_anchor == anchor {
      return;
    }
    self.current_anchor = anchor.to_string();
    if frame.is_web() {
      ctx.output().open_url(format!("#{}", anchor));
    }
  }

  fn update(&mut self, backend: &mut BackendState, ctx: &egui::Context, frame: &mut eframe::Frame) {
    let anchor = self.current_anchor.clone();
    let mut app_event = None;
    for app in self.apps() {
      if app.match_anchor(&anchor) {
        match app.update(backend, ctx, &anchor) {
          Some(event) => {
            app_event = Some(event);
            break;
          }
          None => (),
        }
      }
    }
    match app_event {
      Some(SubAppEvent::BlockDetails(hash)) => {
        let anchor = format!("block_details/{:?}", hash);
        self.open_anchor(&anchor, ctx, frame);
      }
      _ => (),
    }
  }
}

/// Main Polymesh app.
#[derive(Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct PolymeshApp {
  state: State,

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

  fn top_navbar_ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
    egui::widgets::global_dark_light_mode_switch(ui);
    ui.separator();
    ui.toggle_value(&mut self.backend.open, "💻 Backend");
    // Sub-apps
    let current_anchor = self.state.current_anchor.clone();
    let mut changed = None;
    for app in self.state.apps() {
      let name = app.name();
      if ui
        .selectable_label(app.match_anchor(&current_anchor), name)
        .clicked()
      {
        changed = Some(app.anchor().to_string());
      }
    }
    if let Some(new_anchor) = changed {
      self.state.open_anchor(&new_anchor, ui.ctx(), frame);
    }
  }
}

impl eframe::App for PolymeshApp {
  /// Called by the frame work to save state before shutdown.
  fn save(&mut self, storage: &mut dyn eframe::Storage) {
    eframe::set_value(storage, eframe::APP_KEY, self);
  }

  /// Main entry point for UI updates.
  fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
    // Backend updates.
    self.backend.update(ctx, frame);

    self.save_changes(frame);

    #[cfg(target_arch = "wasm32")]
    if let Some(web_info) = frame.info().web_info.as_ref() {
      if let Some(anchor) = web_info.location.hash.strip_prefix('#') {
        self.state.current_anchor = anchor.to_owned();
      }
    }

    // Make sure one of the sub-apps is selected.  Default to the first one.
    if self.state.current_anchor.is_empty() {
      let anchor = self.state.apps().next().unwrap().anchor();
      self.state.current_anchor = anchor.to_string();
    }

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

    self.state.update(&mut self.backend, ctx, frame);
  }
}
