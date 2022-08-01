use anyhow::Result;

use tokio::sync::mpsc;

use polymesh_api::*;
pub use polymesh_api::client::*;

#[derive(Clone, Debug)]
pub enum UpdateMessage {
  NewBlock(Header),
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
      recv: spawn_backend(url)
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
    rt.block_on(async {
      let res = run_backend(&url, send).await;
      log::info!("backend stopped: {:?}", res);
    });
  });

  recv
}

#[cfg(target_arch = "wasm32")]
fn spawn_backend(url: &str) -> UpdateReceiver {
  let url = url.to_string();
  let (send, recv) = mpsc::channel(16);

  wasm_bindgen_futures::spawn_local(async move {
    let res = run_backend(&url, send).await;
    log::info!("backend stopped: {:?}", res);
  });

  recv
}

async fn run_backend(url: &str, send: UpdateSender) -> Result<()> {
  log::info!("Backend connect to: {url:?}");
  let api = Api::new(url).await?;
  let client = api.client();

  let mut sub_blocks = client.subscribe_blocks().await?;

  while let Some(header) = sub_blocks.next().await.transpose()? {
    //log::info!("{}: {}", header.number, header.hash());
    send.send(UpdateMessage::NewBlock(header)).await?;
  }

  Ok(())
}
