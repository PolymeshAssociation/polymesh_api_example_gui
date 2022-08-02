use anyhow::Result;

use tokio::sync::mpsc;

use serde_json::{to_value, Value};

pub use polymesh_api::client::*;
use polymesh_api::*;

#[cfg(not(target_arch = "wasm32"))]
use tokio::spawn as spawn_local;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::spawn_local;

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
pub enum BackendRequest {
  ConnectTo(String),
  GetBlockInfo(BlockHash),
}

pub type BackendRequestSender = mpsc::Sender<BackendRequest>;
pub type BackendRequestReceiver = mpsc::Receiver<BackendRequest>;

#[derive(Clone, Debug)]
pub enum BackendEvent {
  /// Connected(`genesis_hash`, `is_reconnect`)
  Connected {
    genesis: BlockHash,
    is_reconnect: bool,
  },
  NewHeader(Header),
  BlockInfo(BlockInfo),
}

pub type BackendEventSender = mpsc::Sender<BackendEvent>;
pub type BackendEventReceiver = mpsc::Receiver<BackendEvent>;

pub struct Backend {
  url: String,
  event_rx: BackendEventReceiver,
  req_tx: BackendRequestSender,
}

impl Backend {
  pub fn new() -> Self {
    let (event_tx, event_rx) = mpsc::channel(16);
    let (req_tx, req_rx) = mpsc::channel(16);
    let inner = SpawnBackend::new(req_rx, event_tx);
    inner.spawn();
    Self {
      url: "".into(),
      event_rx,
      req_tx,
    }
  }

  pub fn get_url(&self) -> &str {
    &self.url
  }

  pub fn connect_to(&mut self, url: &str) -> Result<()> {
    self.url = url.to_string();
    self
      .req_tx
      .blocking_send(BackendRequest::ConnectTo(url.to_string()))?;
    Ok(())
  }

  pub fn get_block_info(&self, hash: BlockHash) -> Result<()> {
    self
      .req_tx
      .blocking_send(BackendRequest::GetBlockInfo(hash))?;
    Ok(())
  }

  pub fn next_update(&mut self) -> Option<BackendEvent> {
    use tokio::sync::mpsc::error::TryRecvError;
    match self.event_rx.try_recv() {
      Ok(msg) => Some(msg),
      Err(TryRecvError::Empty) => None,
      Err(TryRecvError::Disconnected) => None,
    }
  }
}

pub struct SpawnBackend {
  event_tx: BackendEventSender,
  req_rx: BackendRequestReceiver,
}

impl SpawnBackend {
  fn new(req_rx: BackendRequestReceiver, event_tx: BackendEventSender) -> Self {
    Self { req_rx, event_tx }
  }

  #[cfg(not(target_arch = "wasm32"))]
  fn spawn(self) {
    let rt = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .unwrap();

    std::thread::spawn(move || {
      rt.block_on(self.run_backend());
    });
  }

  #[cfg(target_arch = "wasm32")]
  fn spawn(self) {
    wasm_bindgen_futures::spawn_local(self.run_backend());
  }

  async fn run_backend(self) {
    let Self {
      event_tx,
      mut req_rx,
    } = self;
    // Wait for url from frontend.
    while let Some(req) = req_rx.recv().await {
      match req {
        BackendRequest::ConnectTo(url) => {
          log::info!("Backend connect to: {url:?}");
          let api = match Api::new(&url).await {
            Ok(api) => api,
            Err(err) => {
              log::error!("Failed to connect to backend: {err:?}");
              continue;
            }
          };

          match InnerBackend::start(api, req_rx, event_tx).await {
            Ok(_) => {
              log::info!("backend stopped.");
            }
            Err(err) => {
              log::error!("backend failed to start: {err:?}");
            }
          }
          break;
        }
        req => {
          log::error!("Backend not started yet: {req:?}");
        }
      }
    }
  }
}

pub struct InnerBackend {
  api: Api,
  event_tx: BackendEventSender,
  req_rx: BackendRequestReceiver,
}

impl InnerBackend {
  async fn start(
    api: Api,
    req_rx: BackendRequestReceiver,
    event_tx: BackendEventSender,
  ) -> Result<()> {
    let mut inner = Self {
      api,
      event_tx,
      req_rx,
    };
    // First connect.
    let mut is_reconnect = false;

    while !inner.is_closed() {
      match inner.run(is_reconnect).await {
        Ok(true) => (),
        Ok(false) => {
          // Exit.
          break;
        }
        Err(err) => {
          log::error!("{err:?}");
        }
      }
      is_reconnect = true;
    }
    Ok(())
  }

  fn is_closed(&self) -> bool {
    self.event_tx.is_closed()
  }

  async fn send(&self, msg: BackendEvent) -> Result<()> {
    Ok(self.event_tx.send(msg).await?)
  }

  async fn push_block(&self, header: Header) -> Result<()> {
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
    self.send(BackendEvent::BlockInfo(block)).await?;
    Ok(())
  }

  async fn get_block_hash(&self, number: BlockNumber) -> Result<BlockHash> {
    Ok(self.api.client().get_block_hash(number).await?)
  }

  async fn get_block_header(&self, hash: Option<BlockHash>) -> Result<Option<Header>> {
    Ok(self.api.client().get_block_header(hash).await?)
  }

  async fn connected(&self, is_reconnect: bool) -> Result<()> {
    let genesis = self.get_block_hash(0).await?;
    self
      .send(BackendEvent::Connected {
        genesis,
        is_reconnect,
      })
      .await?;
    Ok(())
  }

  async fn run(&mut self, is_reconnect: bool) -> Result<bool> {
    self.connected(is_reconnect).await?;

    let client = self.api.client();

    // Spawn background watcher for new blocks.
    let sub_blocks = client.subscribe_blocks().await?;
    HeaderWatcher::spawn(sub_blocks, self.event_tx.clone());

    // Grab and push the current block.
    if let Some(current) = self.get_block_header(None).await? {
      self.push_block(current).await?;
    }

    // Process requests from frontend.
    while let Some(req) = self.req_rx.recv().await {
      match req {
        BackendRequest::ConnectTo(url) => {
          // Reconnect and restart.
          self.api = Api::new(&url).await?;
          return Ok(true);
        }
        BackendRequest::GetBlockInfo(hash) => match self.get_block_header(Some(hash)).await? {
          Some(header) => {
            self.push_block(header).await?;
          }
          None => (),
        },
      }
    }

    Ok(false)
  }
}

pub struct HeaderWatcher {
  sub: Subscription<Header>,
  event_tx: BackendEventSender,
}

impl HeaderWatcher {
  fn spawn(sub: Subscription<Header>, event_tx: BackendEventSender) {
    let watcher = Self { sub, event_tx };
    spawn_local(watcher.start());
  }

  async fn start(self) {
    match self.run().await {
      Err(err) => {
        log::error!("HeaderWatcher: {err:?}");
      }
      Ok(_) => (),
    }
  }

  async fn run(mut self) -> Result<()> {
    while let Some(header) = self.sub.next().await.transpose()? {
      //log::info!("{}: {}", header.number, header.hash());
      self.event_tx.send(BackendEvent::NewHeader(header)).await?;
    }
    Ok(())
  }
}
