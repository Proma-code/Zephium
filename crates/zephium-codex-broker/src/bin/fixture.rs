fn main() {
    #[cfg(all(windows, debug_assertions, feature = "native-fixtures"))]
    {
        let args: Vec<_> = std::env::args_os().collect();
        let mode = args.get(1).and_then(|value| value.to_str());
        let result = match (mode, args.len()) {
            (Some("transport"), 2) => zephium_codex_broker::qualify_transport(),
            (Some("transport-child"), 3) => match args[2].to_str() {
                Some(mode) => zephium_codex_broker::fixture_child(mode),
                None => Err("helper-mode"),
            },
            (Some(kind @ ("cli" | "standalone")), 3) => {
                zephium_codex_broker::qualify_original(kind, std::path::Path::new(&args[2]))
            }
            _ => Err("arguments"),
        };
        if let Err(category) = result {
            eprintln!("codex-qualification-error={category}");
            std::process::exit(1);
        }
    }
    #[cfg(not(all(windows, debug_assertions, feature = "native-fixtures")))]
    {
        eprintln!("Codex qualification requires a debug Windows fixture build.");
        std::process::exit(2);
    }
}
