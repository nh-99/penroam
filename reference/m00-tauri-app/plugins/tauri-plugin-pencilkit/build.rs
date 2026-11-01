// registerListener / remove_listener: used by addPluginListener() for plugin events.
const COMMANDS: &[&str] = &["call", "registerListener", "remove_listener"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).ios_path("ios").build();
}
