use anyhow::Result;

use tokio::sync::mpsc;

use serde_json::{to_value, Value};

pub use polymesh_api::client::*;
use polymesh_api::*;

#[cfg(target_arch = "wasm32")]
const PRELOAD_BLOCKS: usize = 5;
#[cfg(not(target_arch = "wasm32"))]
const PRELOAD_BLOCKS: usize = 1000;

#[derive(Clone, Debug)]
pub struct EventInfo {
  pub block: BlockNumber,
  pub number: u32,
  pub phase: Phase,
  pub name: String,
  pub value: Value,
}

impl EventInfo {
  pub fn new(
    block: BlockNumber,
    number: u32,
    event: EventRecord<<Api as ChainApi>::RuntimeEvent>,
  ) -> Self {
    let phase = event.phase;
    let (name, value) = match to_value(event.event) {
      Err(err) => (format!("Unknown event: {err:?}"), Value::Null),
      Ok(Value::Object(map)) if map.len() == 1 => {
        let (mod_name, event) = map.into_iter().next().unwrap();
        match event {
          Value::Object(map) if map.len() == 1 => {
            let (name, value) = map.into_iter().next().unwrap();
            (format!("{mod_name}.{name}"), value)
          }
          Value::String(name) => (format!("{mod_name}.{name}"), Value::Null),
          event => (
            format!("Invalid {mod_name} event type: {:?}.", event),
            event,
          ),
        }
      }
      Ok(event) => (format!("Invalid runtime event type."), event),
    };
    Self {
      block,
      number,
      phase,
      name,
      value,
    }
  }
}

#[derive(Clone, Debug)]
pub struct BlockInfo {
  pub hash: BlockHash,
  pub header: Header,
  pub events: Vec<EventInfo>,
}

impl BlockInfo {
  pub fn number(&self) -> BlockNumber {
    self.header.number
  }
}

#[derive(Clone, Debug)]
pub enum UpdateMessage {
  /// Connected(`genesis_hash`, `is_reconnect`)
  Connected {
    genesis: BlockHash,
    is_reconnect: bool
  },
  NewBlock(BlockInfo),
}

pub type UpdateSender = mpsc::Sender<UpdateMessage>;
pub type UpdateReceiver = mpsc::Receiver<UpdateMessage>;

pub struct Backend {
  url: String,
  recv: UpdateReceiver,
}

impl Backend {
  pub fn new(url: &str) -> Self {
    Self {
      url: url.to_string(),
      recv: spawn_backend(url),
    }
  }

  pub fn get_url(&self) -> &str {
    &self.url
  }

  pub fn next_update(&mut self) -> Option<UpdateMessage> {
    use tokio::sync::mpsc::error::TryRecvError;
    match self.recv.try_recv() {
      Ok(msg) => Some(msg),
      Err(TryRecvError::Empty) => None,
      Err(TryRecvError::Disconnected) => None,
    }
  }
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_backend(url: &str) -> UpdateReceiver {
  let url = url.to_string();
  let (send, recv) = mpsc::channel(16);

  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();

  std::thread::spawn(move || {
    rt.block_on(run_backend(&url, send));
  });

  recv
}

#[cfg(target_arch = "wasm32")]
fn spawn_backend(url: &str) -> UpdateReceiver {
  let url = url.to_string();
  let (send, recv) = mpsc::channel(16);

  wasm_bindgen_futures::spawn_local(run_backend(&url, send));

  recv
}

async fn run_backend(url: &str, send: UpdateSender) {
  match InnerBackend::start(url, send).await {
    Ok(_) => {
      log::info!("backend stopped.");
    }
    Err(err) => {
      log::error!("backend failed to start: {err:?}");
    }
  }
}

pub struct InnerBackend {
  api: Api,
  send: UpdateSender,
  auto_reconnect: bool,
}

impl InnerBackend {
  async fn start(url: &str, send: UpdateSender) -> Result<()> {
    log::info!("Backend connect to: {url:?}");
    let api = Api::new(url).await?;
    let inner = Self {
      api,
      send,
      auto_reconnect: true,
    };
    inner.run().await;
    Ok(())
  }

  async fn send(&mut self, msg: UpdateMessage) -> Result<()> {
    let res = self.send.send(msg).await;
    if res.is_err() {
      // Frontend closed channel, need to shutdown.
      self.auto_reconnect = false;
    }
    res.map_err(|e| e.into())
  }

  async fn push_block(&mut self, header: Header) -> Result<()> {
    let hash = header.hash();
    // Get block events.
    let events = self
      .api
      .block_events(Some(hash))
      .await?
      .into_iter()
      .enumerate()
      .map(|(idx, ev)| EventInfo::new(header.number, idx as u32, ev))
      .collect();
    let block = BlockInfo {
      hash,
      header,
      events,
    };
    self.send(UpdateMessage::NewBlock(block)).await?;
    Ok(())
  }

  async fn run(mut self) {
    // First connect.
    let mut is_reconnect = false;

    while self.auto_reconnect {
      match self.wait_for_blocks(is_reconnect).await {
        Ok(_) => (),
        Err(err) => {
          log::error!("{err:?}");
        }
      }
      is_reconnect = true;
    }
  }

  async fn connected(&mut self, is_reconnect: bool) -> Result<()> {
    let genesis = self.api.client().get_block_hash(0).await?;
    self.send(UpdateMessage::Connected { genesis, is_reconnect }).await?;
    Ok(())
  }

  async fn wait_for_blocks(&mut self, is_reconnect: bool) -> Result<()> {
    self.connected(is_reconnect).await?;

    let client = self.api.client();

    let mut sub_blocks = client.subscribe_blocks().await?;

    let mut last_block_number = 0;
    // Grab the last X blocks.
    if let Some(current) = client.get_block_header(None).await? {
      let mut parent = current.parent_hash;
      let mut headers = Vec::new();
      headers.push(current);
      for _ in 0..PRELOAD_BLOCKS {
        match client.get_block_header(Some(parent)).await? {
          Some(header) => {
            parent = header.parent_hash;
            headers.push(header);
          }
          None => {
            break;
          }
        }
      }

      for header in headers.into_iter().rev() {
        last_block_number = header.number;
        self.push_block(header).await?;
      }
    }

    while let Some(header) = sub_blocks.next().await.transpose()? {
      //log::info!("{}: {}", header.number, header.hash());
      if header.number > last_block_number {
        self.push_block(header).await?;
      }
    }

    Ok(())
  }
}
