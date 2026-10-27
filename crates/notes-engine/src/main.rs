fn main() {
    if let Some(code) = notes_engine::run_hidden_subcommand() {
        std::process::exit(code);
    }
    notes_engine::preview_impl::SELF_EXEC_INK_CHILD.store(true, std::sync::atomic::Ordering::SeqCst);
    let args: Vec<String> = std::env::args().collect();
    std::process::exit(notes_engine::launcher::main(args.get(1..).unwrap_or(&[])));
}
