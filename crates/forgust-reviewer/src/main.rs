//! `forgust-review`: run in an AI review job of Forgust (see the README for
//! the variables it reads and the report it writes).

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("forgust-review {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
    std::process::exit(rt.block_on(forgust_reviewer::run_from_env()));
}
