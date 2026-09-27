/**
* this file code contains a shared retry-with-backoff helper for outbound provider calls
*/
use std::time::Duration;

pub async fn retry_with_backoff<T, E, F, Fut>(attempts: u32, base_delay: Duration, mut f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut attempt = 1;
    loop {
        match f().await {
            Ok(value) => return Ok(value),
            Err(_) if attempt < attempts => {
                tokio::time::sleep(base_delay * attempt).await;
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}
