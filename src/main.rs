use std::{fs::read_to_string, path::Path, time::Duration};

use anyhow::anyhow;
use axum::{extract::State, http::StatusCode, response::{sse::Event, Html, IntoResponse, Sse}, routing::get, Router};
use html::generate;
use markdown::Options;
use notify::{ EventHandler, Watcher};
use serde::Serialize;
use tokio::sync::mpsc::{channel, unbounded_channel, Sender};
use tokio_stream::{Stream, StreamExt};
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tracing::{debug, error, info, span, Instrument, Level};

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

    /// Enable debug-level logging. Supply twice for trace logging.
    #[arg(short, long, action = clap::ArgAction::Count)]
    debug: u8,

    /// Use an inotify watcher instead of a polling watcher
    #[arg(short, long)]
    inotify: bool,

    /// Render unsafe HTML in markdown. Only use for trusted files
    #[arg(long)]
    html: bool,
}

impl Cli {
    /// wrapper to allow us to easily swap between the inotify and poll watcher
    fn poller<F: EventHandler>(&self, cb: F, cfg: notify::Config) -> anyhow::Result<Box<dyn notify::Watcher + Send>> {
        if self.inotify {
            debug!("Using inotify watcher");
            Ok(Box::new(notify::INotifyWatcher::new(cb, cfg)?))
        } else {
            debug!("Using poll watcher");
            Ok(Box::new(notify::PollWatcher::new(cb, cfg)?))
        }
    } 

    fn logging(&self) -> String {
        match self.debug {
            0 => String::from("info"),
            1 => String::from("debug"),
            _ => String::from("trace")
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let trace_level = cli.logging();
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
    let newstate = Cli{filename: path.clone(), ..state};
    let (tx, rx) = channel::<Result<Event, anyhow::Error>>(30);

    let fspan = span!(Level::DEBUG, "file_watch", file=&path);
    tokio::spawn(async move {
        debug!("starting new SSE handler");
       if let Err(e) = file_watch(tx, newstate).await {
        error!("error watching file, closing SSE task: {}", e);
       }
    }.instrument(fspan));
    
    let filestream = tokio_stream::wrappers::ReceiverStream::new(rx);

    // I'm sure there's a better way to log this...
    Sse::new(filestream.map(|msg|{
        match &msg {
            Ok(_e) =>  tracing::trace!("got update event from file watcher"),
            Err(e) => error!("error in filestream event from watcher: {:?}", e)
        };
        msg
    })).keep_alive(
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
    debug!("starting new file notify watcher");
    //initialize with base file event
    tx.send(read_to_event(&opts.filename, opts.html).await).await?;

    let (watch_tx,mut watch_rx) = unbounded_channel::<Result<notify::Event, notify::Error>>();
    
    
    let mut watcher = opts.poller(
        move |res: Result<notify::Event, notify::Error>| {
                tracing::trace!("got event of type {:?}", res);
                if let Err(e) = watch_tx.send(res) {
                    error!("error sending from watcher, exiting handler: {}", e);
                }
        },
        notify::Config::default().with_poll_interval(Duration::from_secs(1)),
    )?;

    let path = Path::new(&opts.filename);
    watcher.watch(path, notify::RecursiveMode::Recursive)?;


    while let Some(evt) = watch_rx.recv().await {
        
        let file_evt = evt?;
        if file_evt.kind.is_modify() {
            let path = file_evt.paths[0].clone();
            let monitor_path = path.to_string_lossy().to_string();
            debug!{%monitor_path, "updating file"};
            tx.send(read_to_event(path, opts.html).await).await?;
        }

    }
    Ok(())
}

/// turn a filepath into a complete SSE event from parsed markdown
async fn read_to_event<P: AsRef<Path>>(filepath: P, html_mode: bool) -> Result<Event, anyhow::Error>{
    let md = retry_read(&filepath).await?;
    let mut md_opts = Options::gfm();
    if html_mode {
        md_opts.compile.allow_dangerous_html = true;
        md_opts.compile.allow_dangerous_protocol = true;
        md_opts.compile.gfm_tagfilter = false;
    }
    let res_html = match markdown::to_html_with_options(&md, &md_opts){
        Ok(h) => h,
        Err(m) => {
            error!{%m.source, %m.reason, "error rendering markdown"}
            m.to_string()
        }
    };
    Ok(Event::default().data(res_html).event("body"))

}

/// The MOVE_SELF behavior of vim tends to produce race conditions, we might try to read a file while vim is moving things around.
async fn retry_read<P: AsRef<Path>>(filepath: P) -> anyhow::Result<String> {
    let count = 3;
    for _i in 0..count {
        match read_to_string(&filepath) {
            Ok(r) => {
                return Ok(r)
            }
            Err(e) => {
                error!("error reading file, retrying: {e}");
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await
    };

    Err(anyhow!("Could not read from file {}", filepath.as_ref().to_string_lossy()))
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