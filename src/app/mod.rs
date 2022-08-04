use std::collections::{HashMap, VecDeque};

use egui::*;
use egui_extras::{Size, StripBuilder, TableBuilder};

use crate::backend::*;

const POLYMESH_STAGING: &str = "wss://staging-rpc.polymesh.live";
const MAX_BACKEND_UPDATES: usize = 100;
const MAX_RECENT_BLOCKS: usize = 2000;
const MAX_RECENT_EVENTS: usize = 2000;

#[cfg(target_arch = "wasm32")]
const PRELOAD_BLOCKS: u32 = 20;
#[cfg(not(target_arch = "wasm32"))]
const PRELOAD_BLOCKS: u32 = 1000;

#[derive(Debug)]
pub struct BlockEventSummary {
  pub block: BlockNumber,
  pub number: u32,
  pub name: &'static str,
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
  preload_blocks: u32,
  #[serde(skip)]
  preload_next: Option<BlockHash>,

  #[serde(skip)]
  best_block: BlockNumber,

  #[serde(skip)]
  hash_to_number: HashMap<BlockHash, BlockNumber>,
  #[serde(skip)]
  blocks: HashMap<BlockNumber, BlockInfo>,
  #[serde(skip)]
  recent_blocks: VecDeque<BlockNumber>,
  #[serde(skip)]
  recent_events: VecDeque<BlockEventSummary>,
  #[serde(skip)]
  backend: Backend,
}

impl Default for BackendState {
  fn default() -> Self {
    Self {
      open: true,
      need_save: true,
      url: POLYMESH_STAGING.to_owned(),
      genesis_hash: None,
      best_block: 0,
      preload_blocks: PRELOAD_BLOCKS as u32,
      preload_next: None,

      hash_to_number: Default::default(),
      blocks: Default::default(),
      recent_blocks: Default::default(),
      recent_events: Default::default(),
      backend: Backend::new(),
    }
  }
}

impl BackendState {
  fn clear(&mut self) {
    self.genesis_hash = None;
    self.best_block = 0;
    self.preload_blocks = PRELOAD_BLOCKS;
    self.preload_next = None;

    self.hash_to_number.clear();
    self.blocks.clear();
    self.recent_blocks.clear();
    self.recent_events.clear();
  }

  fn connect(&mut self) {
    match self.backend.connect_to(&self.url) {
      Err(err) => {
        log::error!("Failed to send ConnectTo reqest to backend: {err:?}");
      }
      _ => (),
    }
  }

  fn get_block_info(&self, hash: BlockHash) {
    match self.backend.get_block_info(hash) {
      Err(err) => {
        log::error!("Failed to send block info reqest to backend: {err:?}");
      }
      _ => (),
    }
  }

  fn check_node_url(&mut self) {
    if self.backend.get_url() != self.url {
      log::info!("Node url changed.  Reconnect to backend.");
      self.connect();
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

  fn next_preload(&mut self, block: &BlockInfo) {
    // Check if we are still preloading.
    if self.preload_blocks == 0 {
      return;
    }
    // Make sure it was our last requested block.
    if let Some(next) = &self.preload_next {
      if next != &block.hash {
        return;
      }
    }
    // Preload parent block.
    self.preload_blocks -= 1;
    let hash = block.header.parent_hash;
    self.preload_next = Some(hash);
    match self.backend.get_block_info(hash) {
      Err(err) => log::error!("Backend error: {err:?}"),
      _ => (),
    }
  }

  pub fn backend_updates(&mut self) {
    // Poll the backend for updates.
    for _ in 0..MAX_BACKEND_UPDATES {
      match self.backend.next_update() {
        Some(BackendEvent::Connected {
          genesis,
          is_reconnect,
        }) => {
          log::info!("Connected to backend: {genesis:?}, is_reconnect={is_reconnect}");
          if is_reconnect {
            // Check if the chain is the same.
            if self.genesis_hash != Some(genesis) {
              log::info!("---- Different genesis hash clear chain state.");
              // Clear old chain data.
              self.clear();
            }
          }
          self.genesis_hash = Some(genesis);
        }
        Some(BackendEvent::NewHeader(header)) => {
          // New block header.  Request block info.
          match self.backend.get_block_info(header.hash()) {
            Err(err) => log::error!("Backend error: {err:?}"),
            _ => (),
          }
        }
        Some(BackendEvent::BlockInfo(block)) => {
          // Check if the block is the newest best.
          let number = block.number();
          let is_best = number > self.best_block;
          if is_best {
            self.best_block = number;
          }

          // Handle preloading.
          self.next_preload(&block);

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
                      name: event.name,
                      count: 1,
                    });
                  }
                }

                events
              },
            )
            .into_iter()
            .for_each(|(_, event)| {
              if is_best {
                self.recent_events.push_front(event);
              } else {
                self.recent_events.push_back(event);
              }
            });
          // Update blocks.
          self.hash_to_number.insert(block.hash, number);
          if self.blocks.insert(number, block).is_none() {
            // Update recent blocks.
            if is_best {
              self.recent_blocks.push_front(number);
            } else {
              self.recent_blocks.push_back(number);
            }
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
pub struct ChainInfoApp {
  #[serde(skip)]
  reset_scroll: bool,
}

impl ChainInfoApp {
  // HACK(egui): Validate `row_range`.  `egui::ScrollArea` can give an invalid row range.
  fn validate_range(&mut self, max: usize, range: &std::ops::Range<usize>) -> bool {
    if range.start > range.end || range.end > max {
      self.reset_scroll = true;
      false
    } else {
      true
    }
  }

  fn recent_blocks_ui(
    &mut self,
    backend: &mut BackendState,
    ui: &mut egui::Ui,
  ) -> Option<SubAppEvent> {
    let mut app_event = None;
    ui.label("Recent blocks:");
    ui.separator();
    ui.push_id("Blocks", |ui| {
      let blocks = &backend.recent_blocks;
      let text_style = TextStyle::Body;
      let row_height = ui.text_style_height(&text_style);
      let num_rows = blocks.len();
      let mut scroll = ScrollArea::vertical().auto_shrink([false; 2]);
      if self.reset_scroll {
        scroll = scroll.vertical_scroll_offset(0.0);
      }
      scroll.show_rows(ui, row_height, num_rows, |ui, row_range| {
        if !self.validate_range(num_rows, &row_range) {
          return;
        }

        for number in blocks.range(row_range) {
          let block = backend.blocks.get(number).unwrap();
          ui.horizontal(|ui| {
            if ui.link(format!("{}", block.number())).clicked() {
              app_event = Some(SubAppEvent::BlockDetails(block.hash));
            }
            ui.label(format!("{:?}", block.hash));
          });
        }
      });
    });
    app_event
  }

  fn recent_events_ui(
    &mut self,
    backend: &mut BackendState,
    ui: &mut egui::Ui,
  ) -> Option<SubAppEvent> {
    let mut app_event = None;
    ui.label("Recent events:");
    ui.separator();
    ui.push_id("Events", |ui| {
      let events = &backend.recent_events;
      let text_style = TextStyle::Body;
      let row_height = ui.text_style_height(&text_style);
      let num_rows = events.len();
      let mut scroll = ScrollArea::vertical().auto_shrink([false; 2]);
      if self.reset_scroll {
        scroll = scroll.vertical_scroll_offset(0.0);
      }
      self.reset_scroll = false;
      scroll.show_rows(ui, row_height, num_rows, |ui, row_range| {
        if !self.validate_range(num_rows, &row_range) {
          return;
        }

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
      });
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
pub struct BlockDetailsApp {
  last_anchor: String,
  block_hash: BlockHash,
  requested: bool,
}

impl BlockDetailsApp {
  fn parse_anchor_and_load_block<'a>(
    &mut self,
    backend: &'a mut BackendState,
    anchor: &str,
  ) -> Result<Option<&'a BlockInfo>, String> {
    // If the nav `anchor` changed, then update our block hash to display.
    if self.last_anchor != anchor {
      self.last_anchor = anchor.to_string();
      if let Some(param) = anchor.strip_prefix(self.anchor()) {
        if param.len() == 0 {
          let number = backend.best_block;
          return Ok(backend.blocks.get(&number));
        } else if param.starts_with("0x") {
          // Parse block hash.
          let hash = hex::decode(&param.as_bytes()[2..]).ok().and_then(|raw| {
            if raw.len() == BlockHash::len_bytes() {
              Some(BlockHash::from_slice(raw.as_slice()))
            } else {
              None
            }
          });
          match hash {
            Some(hash) => {
              self.block_hash = hash;
              self.requested = false;
            }
            None => {
              return Err(format!("Failed to parse block hash: {param:?}"));
            }
          }
        } else {
          return Err(format!("Unsupported block number lookup: {}", param));
        }
      } else {
        return Err(format!("Failed to parse nav anchor: {}", anchor));
      }
    }
    // Check if the block is already loaded.
    let block = backend
      .hash_to_number
      .get(&self.block_hash)
      .and_then(|number| backend.blocks.get(number));
    if block.is_some() {
      // The block is loaded, return it.
      return Ok(block);
    }
    // Need to request the block.
    if !self.requested {
      self.requested = true;
      backend.get_block_info(self.block_hash);
    }
    Ok(None)
  }

  fn block_header_ui(&self, ui: &mut egui::Ui, block: &BlockInfo) -> Option<SubAppEvent> {
    let mut app_event = None;
    let width = ui.available_width();
    ui.set_width(width);
    let text_height = egui::TextStyle::Body.resolve(ui.style()).size;
    TableBuilder::new(ui)
      .striped(true)
      .cell_layout(egui::Layout::left_to_right().with_cross_align(egui::Align::Center))
      .column(Size::initial(100.0).at_least(60.0))
      .column(Size::remainder().at_least(60.0))
      .column(Size::remainder().at_least(60.0))
      .column(Size::remainder().at_least(60.0))
      .column(Size::remainder().at_least(60.0))
      .resizable(false)
      .header(20.0, |mut header| {
        header.col(|ui| {
          ui.heading("Number");
        });
        header.col(|ui| {
          ui.heading("Hash");
        });
        header.col(|ui| {
          ui.heading("Parent");
        });
        header.col(|ui| {
          ui.heading("Extrinsics");
        });
        header.col(|ui| {
          ui.heading("State");
        });
      })
      .body(|mut body| {
        body.row(text_height, |mut row| {
          row.col(|ui| {
            ui.label(format!("{}", block.number()));
          });
          row.col(|ui| {
            ui.label(format!("{}", block.hash));
          });
          row.col(|ui| {
            if ui.link(format!("{}", block.header.parent_hash)).clicked() {
              app_event = Some(SubAppEvent::BlockDetails(block.header.parent_hash));
            }
          });
          row.col(|ui| {
            ui.label(format!("{:?}", block.header.extrinsics_root));
          });
          row.col(|ui| {
            ui.label(format!("{:?}", block.header.state_root));
          });
        })
      });
    app_event
  }

  fn block_extrinsics_ui(&self, ui: &mut egui::Ui, block: &BlockInfo) {
    let width = ui.available_width();
    ui.set_width(width);
    let text_height = egui::TextStyle::Body.resolve(ui.style()).size;
    TableBuilder::new(ui)
      .striped(true)
      .cell_layout(egui::Layout::left_to_right().with_cross_align(egui::Align::Center))
      .column(Size::initial(150.0).at_least(60.0))
      .column(Size::initial(150.0).at_least(60.0))
      .column(Size::remainder().at_least(100.0))
      .resizable(false)
      .header(20.0, |mut header| {
        header.col(|ui| {
          ui.heading("Phase");
        });
        header.col(|ui| {
          ui.heading("Name");
        });
        header.col(|ui| {
          ui.heading("Value");
        });
      })
      .body(|body| {
        let num_rows = block.events.len();
        body.rows(text_height, num_rows, |row_index, mut row| {
          if let Some(event) = block.events.get(row_index) {
            row.col(|ui| {
              ui.label(format!("{:?}", event.phase));
            });
            row.col(|ui| {
              ui.label(format!("{}", event.name));
            });
            row.col(|ui| {
              ui.label(format!("{:?}", event.value));
            });
          }
        })
      });
  }

  fn show_block_ui(&self, ui: &mut egui::Ui, block: &BlockInfo) -> Option<SubAppEvent> {
    let mut app_event = None;
    let width = ui.available_width();
    ui.set_width(width);
    let height = ui.available_height();
    ui.set_height(height);
    StripBuilder::new(ui)
      .size(Size::initial(60.0).at_least(40.0)) // Block header
      .size(Size::remainder()) // Extrinsics.
      .vertical(|mut strip| {
        strip.cell(|ui| {
          ui.push_id("Block Header", |ui| {
            app_event = self.block_header_ui(ui, block);
          });
        });
        strip.cell(|ui| {
          ui.push_id("Block Extrinsics", |ui| {
            self.block_extrinsics_ui(ui, block);
          });
        });
      });
    app_event
  }
}

impl SubApp for BlockDetailsApp {
  fn name(&self) -> &str {
    "Block details"
  }

  fn anchor(&self) -> &str {
    "block_details/"
  }

  fn update(
    &mut self,
    backend: &mut BackendState,
    ctx: &egui::Context,
    anchor: &str,
  ) -> Option<SubAppEvent> {
    let res = self.parse_anchor_and_load_block(backend, anchor);

    let mut app_event = None;
    egui::CentralPanel::default().show(ctx, |ui| match res {
      Ok(block) => {
        if let Some(block) = block {
          app_event = self.show_block_ui(ui, block);
        } else {
          ui.label(format!("Loading block..."));
        }
      }
      Err(err) => {
        ui.label(format!("Failed: {err:?}"));
      }
    });
    app_event
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

    app.backend.connect();

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
