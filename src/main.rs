mod channels;
mod local;
mod state;

use std::env;

use actix_web::{App, HttpServer, web};
use dotenv::dotenv;
use sqlx::postgres::PgPoolOptions;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenv().ok();

    let db_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set in the environment");

    let db = PgPoolOptions::new()
        .max_connections(local::PG_MAX_CONNECTION)
        .connect(&db_url)
        .await
        .expect("Failed to connect to Supabase");

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
