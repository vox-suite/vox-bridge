mod channels;
mod state;

use actix_web::{App, HttpServer, web};
use dotenv::dotenv;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenv().ok();

    HttpServer::new(|| {
        App::new()
            .app_data(web::Data::new(state::init_actix_app_state()))
            .configure(channels::channels_routes)
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
