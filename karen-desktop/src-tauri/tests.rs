#[tokio::main]
async fn main() {
    let args = serde_json::json!({"path": "~/"});
    let result = app::brain::browse_files::list_directory(args).await;
    println!("{}", result);
}
