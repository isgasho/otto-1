/**
 * The main module for otto-eventbus simply sets up the responders and the main
 * server loop that the eventbus uses.
 */
extern crate actix;
extern crate actix_http;
extern crate actix_web;
extern crate config;
extern crate log;
extern crate pretty_env_logger;
#[macro_use]
extern crate rust_embed;
#[macro_use]
extern crate serde_json;

use actix::{Actor, Addr};
use actix_web::{middleware, web};
use actix_web::{App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_web_actors::ws;
use chrono::Local;
use handlebars::Handlebars;
use log::{info, trace};

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use otto_eventbus::*;

pub mod connection;
pub mod eventbus;

/**
 * Templates is a rust-embed struct which will contain all the files embedded from the
 * eventbus/templates/ directory
 */
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/templates"]
struct Templates;

/**
 * Static is a rust-embed struct which contains everything in the static/
 * folder at compile-time
 */
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/static"]
struct Static;

struct AppState {
    bus: Addr<eventbus::EventBus>,
    // Handlebars uses a repository for the compiled templates. This object must be
    // shared between the application threads, and is therefore passed to the
    // Application Builder as an atomic reference-counted pointer.
    hb: Arc<Handlebars>,
}

/**
 * index serves up the homepage which is not really functional, but at least shows lost users
 * something
 */
async fn index(state: web::Data<AppState>) -> HttpResponse {
    let data = json!({
        "version" : option_env!("CARGO_PKG_VERSION").unwrap_or("unknown"),
    });

    let template = Templates::get("index.html").unwrap();
    let body = state
        .hb
        .render_template(std::str::from_utf8(template.as_ref()).unwrap(), &data)
        .expect("Failed to render the index.html template!");
    HttpResponse::Ok().body(body)
}

/**
 * ws_index is the handler for all websocket connections, all it is responsible for doing is
 * handling the inbound connection and creating a new WSClient for the connection
 */
async fn ws_index(
    r: HttpRequest,
    stream: web::Payload,
    state: web::Data<AppState>,
) -> Result<HttpResponse, Error> {
    let actor = connection::WSClient::new(state.bus.clone());
    let res = ws::start(actor, &r, stream);
    trace!("{:?}", res.as_ref().unwrap());
    res
}

#[actix_rt::main]
async fn main() -> std::io::Result<()> {
    pretty_env_logger::init();

    let embedded_settings = Static::get("eventbus.yml").unwrap();
    let defaults = std::str::from_utf8(embedded_settings.as_ref()).unwrap();
    /*
     * Load our settings in the priority order of:
     *
     *   - built-in defaults
     *   - yaml file
     *   - environment variables
     *
     * Each layer overriding properties from the last
     */
    let mut settings = config::Config::default();
    settings
        .merge(config::File::from_str(defaults, config::FileFormat::Yaml))
        .unwrap()
        .merge(config::File::with_name("eventbus"))
        .unwrap()
        .merge(config::Environment::with_prefix("OTTO_EB"))
        .unwrap();

    let motd: String = settings
        .get("motd")
        .expect("Configuration had no `motd` setting");

    info!("motd: {}", motd);

    let stateless = settings
        .get::<Vec<String>>("channels.stateless")
        .expect("Failed to load `channels.stateless` configuration, which must be an array");
    let stateful = settings
        .get::<Vec<String>>("channels.stateful")
        .expect("Failed to load `channels.stateful` configuration, which must be an array");

    let events = eventbus::EventBus::with_channels(stateless, stateful).start();
    let bus = events.clone();

    thread::spawn(move || loop {
        let pulse = format!("heartbeat {}", Local::now());
        trace!("sending pulse: {}", pulse);
        let event = eventbus::Event {
            e: Arc::new(Output::Heartbeat),
            channel: Arc::new("all".to_string()),
        };
        bus.do_send(event);
        let seconds = settings
            .get("heartbeat")
            .expect("Invalid `heartbeat` configuration, must be an integer");
        thread::sleep(Duration::from_secs(seconds));
    });

    let state = AppState {
        bus: events,
        hb: Arc::new(Handlebars::new()),
    };
    let wd = web::Data::new(state);

    HttpServer::new(move || {
        App::new()
            .app_data(wd.clone())
            .wrap(middleware::Compress::default())
            .wrap(middleware::Logger::default())
            .route("/", web::get().to(index))
            .route("/ws/", web::get().to(ws_index))
    })
    .bind("127.0.0.1:8000")?
    .run()
    .await
}

#[cfg(test)]
mod test {
    use super::*;
    use actix_web::{test, web, App};

    use regex::Regex;

    /**
     * This test just ensures that the server can come online properly and render its index handler
     * properly.
     *
     * It doesn't really test much useful, but does ensure that critical failures in the eventbus
     * can sometimes be prevented
     */
    #[actix_rt::test]
    async fn test_basic_http() {
        let events = eventbus::EventBus::with_channels(vec![], vec![]).start();
        let state = AppState {
            bus: events,
            hb: Arc::new(Handlebars::new()),
        };
        let wd = web::Data::new(state);
        let srv = test::start(move || {
            App::new()
                .app_data(wd.clone())
                .route("/", web::get().to(index))
        });

        let req = srv.get("/");
        let mut response = req.send().await.unwrap();
        assert!(response.status().is_success());

        let re = Regex::new(r"(v\d\.\d\.\d)").unwrap();

        let body = response.body().await.unwrap();
        let buffer = String::from_utf8(body.to_vec()).unwrap();
        let matches = re.captures(&buffer).unwrap();

        let version = matches.get(1).unwrap();
        assert_eq!(
            version.as_str(),
            format!("v{}", option_env!("CARGO_PKG_VERSION").unwrap_or("unknown"))
        );
    }
}
