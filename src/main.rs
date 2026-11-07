mod channels;
mod local;
mod state;

use actix_web::{App, HttpServer, web};
use dotenv::dotenv;
use std::env;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenv().ok();

    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");

    let db = vox_db::create_pool(&database_url, local::PG_MAX_CONNECTION)
        .await
        .map_err(std::io::Error::other)?;

    let app_state = web::Data::new(state::init_actix_app_state(db));

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .configure(channels::channels_routes)
    })
    .bind(("127.0.0.1", local::SERVE_PORT))?
    .run()
    .await
}
