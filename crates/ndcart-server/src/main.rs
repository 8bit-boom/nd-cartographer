//! The `ndcart` binary: a thin shim over the library's CLI entry point. The studio
//! itself lives in the lib (`ndcart_server`) so a host app — nd-world — can compile
//! it in and mount it with `ndcart_server::embed_router`, no separate process.
fn main() {
    ndcart_server::cli_main();
}
