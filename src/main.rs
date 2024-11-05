use std::{fs::read_to_string, path::Path, time::Duration};

use anyhow::Context;
use axum::{extract::State, http::StatusCode, response::{sse::Event, Html, IntoResponse, Sse}, routing::get, Router};
use html::generate;
use markdown::Options;
use notify::{event::{DataChange, MetadataKind, ModifyKind}, EventKind, RecommendedWatcher, Watcher};
use serde::Serialize;
use tokio::sync::mpsc::{channel, unbounded_channel, Sender};
use tokio_stream::Stream;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tracing::{debug, error, info};

mod html;

/// The Cli. Implements Serialize so we can send it right to the templating engine that renders HTML
#[derive(Clone, Parser, Serialize)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Name of the file to preview
    filename: String,

    /// local address to bind.
    #[arg(short, long, default_value_t=String::from("localhost:3000"))]
    address: String,

    /// Supply a custom CSS stylesheet.
    #[arg(short, long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    stylesheet: Option<String>,

    /// Run web page in dark mode.
    #[arg(long)]
    dark: bool,

    /// Enable debug-level logging.
    #[arg(short, long, action = clap::ArgAction::Count)]
    debug: u8,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let trace_level = if cli.debug == 0 {
        "info"
    } else if cli.debug == 1 {
        "debug"
    } else {
        "trace"
    };
    tracing_subscriber::registry()
    .with(
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            format!("boca={trace_level},tower_http={trace_level}").into()
        }),
    )
    .with(tracing_subscriber::fmt::layer())
    .init();

    let app = Router::new()
    .route("/", get(root))
    .route("/sse/:path", get(sse_handler))
    .route("/*filename", get(link_handler))
    .layer(
        tower_http::trace::TraceLayer::new_for_http()
    )
    .with_state(cli.clone());

    let listener = tokio::net::TcpListener::bind(cli.address).await?;

    info!("serving on {}", listener.local_addr()?);

    axum::serve(listener, app).await?;

    Ok(())
}

/// handler for /sse
async fn sse_handler(State(state): State<Cli>, axum::extract::Path(path): axum::extract::Path<String>) 
-> Sse<impl Stream<Item = Result<Event, anyhow::Error>>> {
    let newstate = Cli{filename: path, ..state};
    debug!{%newstate.filename, "starting new SSE handler"};
    let (tx, rx) = channel::<Result<Event, anyhow::Error>>(30);

    tokio::spawn(async move {
       if let Err(e) = file_watch(tx, newstate).await {
        error!("error watching file, closing SSE task: {}", e);
       }
    });
    
    let filestream = tokio_stream::wrappers::ReceiverStream::new(rx);

    Sse::new(filestream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(1))
            .text("keep-alive-text"),
    )
}

/// Handler for any filepaths other than /
async fn link_handler(State(state): State<Cli>, axum::extract::Path(path): axum::extract::Path<String>)  -> Result<Html<String>, BocaError> {
    info!{%path, "rendering new file"};
    let newstate = Cli{filename: path, ..state};
    let raw = generate(newstate)?;
    Ok(Html(raw.to_string()))
}

/// handler for  /
async fn root(State(state): State<Cli>) -> Result<Html<String>, BocaError> {
    let raw = generate(state)?;
    Ok(Html(raw.to_string()))
}

/// blocks until the receiver closes, waits and sends file updates
async fn file_watch(tx: Sender<Result<Event, anyhow::Error>>, opts: Cli) -> anyhow::Result<()> {
    debug!{%opts.filename, "starting new file notify watcher"};
    //initialize with base file event
    tx.send(read_to_event(&opts.filename)).await?;

    let (watch_tx,mut watch_rx) = unbounded_channel::<Result<notify::Event, notify::Error>>();
    
    
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
                if let Err(e) = watch_tx.send(res) {
                    error!("error sending from watcher, exiting handler: {}", e);
                }
        },
        notify::Config::default(),
    )?;

    let path = Path::new(&opts.filename);
    watcher.watch(path, notify::RecursiveMode::Recursive)?;
    while let Some(evt) = watch_rx.recv().await {
        tracing::trace!("got event of type {:?}", evt);
        let file_evt = evt?;
        if EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)) == file_evt.kind ||
        EventKind::Modify(ModifyKind::Data(DataChange::Any)) == file_evt.kind {
            let path = file_evt.paths[0].clone();
            let monitor_path = path.to_string_lossy().to_string();
            debug!{%monitor_path, "updating file"};
            tx.send(read_to_event(path)).await?;
        }

    }
    Ok(())
}

/// turn a filepath into a complete SSE event from parsed markdown
fn read_to_event<P: AsRef<Path>>(filepath: P) -> Result<Event, anyhow::Error>{
    let md = read_to_string(&filepath).context(format!("error reading path {}", filepath.as_ref().to_string_lossy()))?;
    let res_html = match markdown::to_html_with_options(&md, &Options::gfm()){
        Ok(h) => h,
        Err(m) => {
            error!{%m.source, %m.reason, "error rendering markdown"}
            m.to_string()
        }
    };
    Ok(Event::default().data(res_html).event("body"))

}

struct BocaError(anyhow::Error);

impl IntoResponse for BocaError {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        ).into_response()
    }
}


impl<E> From<E> for BocaError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}